//! RAG 增量管线（ADR-19 决策 3）：已知语料 join → 抓取 → 落状态。
//!
//! # 为什么需要"已知语料库"
//!
//! 客户端上报的是 `sha256(月盐 ‖ 规范化标识)` 的十六进制。服务端拿到哈希后**无法反推**
//! 明文标识——这是 ADR-19 的隐私设计。所以抓取管线不能"照着哈希去抓"，而要先有另一份输入：
//! 服务端把**公开可抓**的语料（arXiv / 开放 DOI / 校规页 …）预先索引，运行时按同一个盐
//! 现算哈希，与上报哈希在**内存里** join，命中者才知道该抓什么。
//!
//! # 关键：库里不存 hash（captain 在 t58 裁决）
//!
//! `rag_known_docs` 只有 `(month, identifier, source, fetched_at, doc_text)`。
//! 一旦把 `(month, hash → identifier)` 落库，静态数据就变成一本"当月去匿名化字典"：
//! 任何能读库的人都能把任意上报哈希翻成明文。现在哈希只在 [`FetchPipeline::run_once`]
//! 运行期按当月盐现算、join 完即丢，**静态数据里不存在哈希→明文映射**。
//! 代价是全表扫描——已知语料是万级到十万级，10 万个 sha256 是毫秒级，完全可接受。
//!
//! # 归一化契约（客户端必须逐字节一致）
//!
//! ```text
//! canonical = trim → 小写 → 去 #fragment → 去 ?query
//!           → 去协议/前缀（https://doi.org/ / doi: / arxiv abs|pdf / arXiv:）
//!           → 去 arXiv 版本号 vN → 去末尾 '/'
//! hash      = hex(sha256(salt_bytes ‖ b"\n" ‖ canonical_bytes))     // 64 位小写
//!
//! 注意：普通网页标识（source=web）**保留小写的 scheme**（https://…），
//! 因为抓取时需要直接当 URL 用；只有 arXiv/DOI 的前缀会被去掉。
//! ```
//!
//! 注意：这是**密钥化摘要，不是 HMAC**——密钥（月盐）是公开下发给客户端的，
//! 所以它提供的是"跨月不可链接"与"客户端各自算不出别人的哈希"，**不是** MAC 意义上的
//! 来源认证。语义边界见 ADR-19 实施说明：匿名性对"语料库外的标识"成立，
//! 对"语料库内的标识"服务端可字典反查。
//!
//! 用 SHA-256 而不是 HMAC-SHA256 的字面构造：端侧 Dart 与 Rust 都只要一份现成的 sha256
//! 就能逐字节对齐，少一类"两端 HMAC 参数不一致"的坑。**改动这里必须同步端侧与 docs**。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use crate::config::Config;
use crate::rag::{RagError, RagService};

/// 单条标识的最大长度（规范化之前）。超过就是畸形输入。
const MAX_IDENTIFIER_BYTES: usize = 512;

/// 日志里允许出现的哈希前缀长度（**绝不打印完整哈希**）。
const HASH_PREFIX_LEN: usize = 8;

/// join 命中的明文目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchTarget {
    pub identifier: String,
    pub source: String,
}

/// 抓取失败：`retryable=false` 时不再重试（例如 404）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    pub message: String,
    pub retryable: bool,
}

impl FetchError {
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

/// 抓取器的盒子 future（不引入 `async-trait` 依赖）。
pub type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<String, FetchError>> + Send + 'a>>;

/// 抓取器抽象：真实实现走 HTTP，测试注入 mock（**CI 里不允许真的联网抓取**）。
pub trait Fetcher: Send + Sync {
    fn fetch<'a>(&'a self, target: &'a FetchTarget) -> FetchFuture<'a>;
}

/// 管线配置（来自 `Config`，测试里可改小）。
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// 每轮最多处理多少个候选（按 count 排序取 Top N）
    pub top_n: usize,
    /// 失败重试次数（总计 `retries + 1` 次尝试）
    pub retries: u32,
    /// 退避基数：第 n 次重试等 `backoff * 2^(n-1)`（测试里设 0 免等）
    pub backoff: Duration,
    /// 是否把上次失败的候选也纳入本轮（默认 false：默认只跑 pending，保证幂等）
    pub retry_failed: bool,
    /// 同一轮内两次抓取之间的最小间隔（限速；arXiv 官方要求礼貌访问）
    pub min_interval: Duration,
}

impl FetchConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            top_n: config.rag_fetch_top_n.max(1),
            retries: config.rag_fetch_retries,
            backoff: Duration::from_millis(config.rag_fetch_backoff_ms),
            retry_failed: false,
            min_interval: Duration::from_millis(config.rag_fetch_min_interval_ms),
        }
    }
}

/// 一轮抓取的结果计数（只含数量，不含任何标识/哈希——避免"结果对象"变成泄漏面）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FetchReport {
    /// 本轮处理（抢占成功）的候选数
    pub processed: usize,
    /// 成功抓到正文
    pub fetched: usize,
    /// 重试后仍失败
    pub failed: usize,
    /// 没有对应的已知语料（无法反推，诚实标记）
    pub skipped: usize,
    /// 抢占失败：被别的 worker 拿走了 / 已经是终态
    pub contended: usize,
}

#[derive(Debug)]
pub enum FetchPipelineError {
    Db(sqlx::Error),
    Salt(RagError),
}

impl From<sqlx::Error> for FetchPipelineError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl From<RagError> for FetchPipelineError {
    fn from(error: RagError) -> Self {
        Self::Salt(error)
    }
}

/// 抓取管线：选候选 → 内存 join 已知语料 → 抓取 → 写状态。
pub struct FetchPipeline {
    pool: PgPool,
    rag: Arc<RagService>,
    fetcher: Arc<dyn Fetcher>,
    config: FetchConfig,
    /// 限速闸：上一次实际发起抓取的时刻（同一轮内串行处理，所以一个时刻就够）
    last_fetch_at: std::sync::Mutex<Option<std::time::Instant>>,
}

impl FetchPipeline {
    pub fn new(
        pool: PgPool,
        rag: Arc<RagService>,
        fetcher: Arc<dyn Fetcher>,
        config: FetchConfig,
    ) -> Self {
        Self {
            pool,
            rag,
            fetcher,
            config,
            last_fetch_at: std::sync::Mutex::new(None),
        }
    }

    /// 限速：等到"距上次抓取至少 `min_interval`"再放行。
    ///
    /// 为什么不并发抓：已知语料是公开资源站（arXiv 等），礼貌访问比吞吐重要；
    /// 单实例串行 + 间隔已经足够（每轮上限是 `top_n`）。
    async fn wait_for_slot(&self) {
        let wait = {
            let mut last = self
                .last_fetch_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let wait = match *last {
                Some(at) => remaining_wait(at, self.config.min_interval),
                None => None,
            };
            *last = Some(std::time::Instant::now());
            wait
        };
        if let Some(wait) = wait {
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
    }

    /// 跑一轮。幂等：只处理 `pending`（可选用 `retry_failed` 纳入 `failed`），
    /// `fetched / skipped` 一律不碰；`fetching` 视为上次崩溃残留，本轮开头放回 `pending`。
    pub async fn run_once(&self) -> Result<FetchReport, FetchPipelineError> {
        let month = self.rag.current_month();
        let salt = self.rag.salt().await?.salt;

        // 崩溃恢复：单实例部署下，把"抢占后没来得及落结果"的 fetching 放回 pending。
        sqlx::query(
            "UPDATE rag_candidates SET status = 'pending' \
             WHERE month = $1 AND status = 'fetching'",
        )
        .bind(&month)
        .execute(&self.pool)
        .await?;

        // **运行期瞬时索引**：hash → (identifier, source)。用完即丢，库里没有这份映射。
        let index = self.load_known_index(&month, &salt).await?;

        let candidates = sqlx::query(
            "SELECT hash FROM rag_candidates \
             WHERE month = $1 AND (status = 'pending' OR ($2 AND status = 'failed')) \
             ORDER BY count DESC, hash \
             LIMIT $3",
        )
        .bind(&month)
        .bind(self.config.retry_failed)
        .bind(self.config.top_n as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut report = FetchReport::default();
        for row in candidates {
            let hash: String = row.try_get("hash")?;
            // 抢占：只有把 pending/failed 改成 fetching 的 worker 才继续（多 worker 安全）
            let claimed = sqlx::query(
                "UPDATE rag_candidates SET status = 'fetching', last_error = NULL \
                 WHERE month = $1 AND hash = $2 AND status IN ('pending', 'failed')",
            )
            .bind(&month)
            .bind(&hash)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if claimed == 0 {
                report.contended += 1;
                continue;
            }
            report.processed += 1;

            let Some(target) = index.get(&hash) else {
                // 反推不出来：诚实标 skipped，不要当成失败（否则会被反复重试）
                sqlx::query(
                    "UPDATE rag_candidates SET status = 'skipped', last_error = 'no_known_doc' \
                     WHERE month = $1 AND hash = $2",
                )
                .bind(&month)
                .bind(&hash)
                .execute(&self.pool)
                .await?;
                report.skipped += 1;
                tracing::info!(
                    hash_prefix = %hash_prefix(&hash),
                    status = "skipped",
                    "rag fetch：已知语料里没有对应项（服务端无法反推该哈希）"
                );
                continue;
            };

            let (attempts, last_error, fetched_text) = self.try_fetch(target).await;
            match fetched_text {
                Some(text) => {
                    sqlx::query(
                        "UPDATE rag_known_docs SET fetched_at = now(), doc_text = $3 \
                         WHERE month = $1 AND identifier = $2",
                    )
                    .bind(&month)
                    .bind(&target.identifier)
                    .bind(&text)
                    .execute(&self.pool)
                    .await?;
                    sqlx::query(
                        "UPDATE rag_candidates \
                         SET status = 'fetched', attempts = $3, last_error = NULL, \
                             fetched_at = now(), doc_id = $4 \
                         WHERE month = $1 AND hash = $2",
                    )
                    .bind(&month)
                    .bind(&hash)
                    .bind(i32::try_from(attempts).unwrap_or(i32::MAX))
                    .bind(&target.identifier)
                    .execute(&self.pool)
                    .await?;
                    report.fetched += 1;
                    tracing::info!(
                        hash_prefix = %hash_prefix(&hash),
                        source = %target.source,
                        status = "fetched",
                        bytes = text.len(),
                        attempts,
                        "rag fetch 成功"
                    );
                }
                None => {
                    sqlx::query(
                        "UPDATE rag_candidates SET status = 'failed', attempts = $3, last_error = $4 \
                         WHERE month = $1 AND hash = $2",
                    )
                    .bind(&month)
                    .bind(&hash)
                    .bind(i32::try_from(attempts).unwrap_or(i32::MAX))
                    .bind(truncate_error(&last_error))
                    .execute(&self.pool)
                    .await?;
                    report.failed += 1;
                    tracing::warn!(
                        hash_prefix = %hash_prefix(&hash),
                        source = %target.source,
                        status = "failed",
                        attempts,
                        error = %truncate_error(&last_error),
                        "rag fetch 失败（重试已用尽）"
                    );
                }
            }
        }

        Ok(report)
    }

    /// 本月已知语料 → 瞬时哈希索引（只在内存里存在）。
    async fn load_known_index(
        &self,
        month: &str,
        salt: &str,
    ) -> Result<HashMap<String, FetchTarget>, FetchPipelineError> {
        let rows = sqlx::query("SELECT identifier, source FROM rag_known_docs WHERE month = $1")
            .bind(month)
            .fetch_all(&self.pool)
            .await?;
        let mut index = HashMap::with_capacity(rows.len());
        for row in rows {
            let identifier: String = row.try_get("identifier")?;
            let source: String = row.try_get("source")?;
            index.insert(
                hash_identifier(salt, &identifier),
                FetchTarget { identifier, source },
            );
        }
        Ok(index)
    }

    /// 带退避重试的抓取。返回 `(尝试次数, 最后一次错误, 成功正文)`。
    async fn try_fetch(&self, target: &FetchTarget) -> (u32, String, Option<String>) {
        let mut attempts = 0u32;
        let mut last_error = String::new();
        while attempts <= self.config.retries {
            attempts += 1;
            self.wait_for_slot().await;
            match self.fetcher.fetch(target).await {
                Ok(text) => return (attempts, String::new(), Some(text)),
                Err(error) => {
                    last_error = error.message;
                    if !error.retryable {
                        break;
                    }
                    if attempts <= self.config.retries && !self.config.backoff.is_zero() {
                        tokio::time::sleep(self.config.backoff * 2u32.saturating_pow(attempts - 1))
                            .await;
                    }
                }
            }
        }
        (attempts, last_error, None)
    }
}

/// 距上次抓取还要等多久（`min_interval` 为 0 时返回 `None`，表示不用等）。
fn remaining_wait(last: std::time::Instant, min_interval: Duration) -> Option<Duration> {
    if min_interval.is_zero() {
        return None;
    }
    let elapsed = last.elapsed();
    (elapsed < min_interval).then(|| min_interval - elapsed)
}

/// 只保留哈希前 8 位 —— **日志里绝不允许出现完整哈希**（否则日志会变成可 join 的字典）。
pub fn hash_prefix(hash: &str) -> String {
    hash.chars().take(HASH_PREFIX_LEN).collect()
}

/// 错误信息裁剪：抓取器的报错可能很长（甚至带全文），只留前 200 个字符。
fn truncate_error(message: &str) -> String {
    message.chars().take(200).collect()
}

/// 规范化标识（契约见模块文档）。返回 `None` 表示畸形输入（空 / 超长 / 归一后为空）。
///
/// # ⚠️ 改规则必须先改向量文件
///
/// 端侧（Flutter）有一份**逐字节等价**的实现，两边靠 `docs/rag_vectors.json` 对齐。
/// 任何对本函数（或 [`hash_identifier`]）的行为修改，都必须**同一次提交**里重新生成
/// 这份向量文件，否则：
///
/// ```text
/// cargo test -p yunong-server rag_vectors -- --ignored   # 重新生成
/// ```
///
/// 守卫用例 `rag_vectors_match_implementation` 会比对文件与当前实现，改漏了 CI 直接红。
/// 没有这道闸门时的后果是"服务端 join 永远 0 命中且不报错"——线上静默失效。
pub fn canonical_identifier(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_IDENTIFIER_BYTES {
        return None;
    }
    let mut value = trimmed.to_lowercase();
    if let Some(index) = value.find('#') {
        value.truncate(index);
    }
    if let Some(index) = value.find('?') {
        value.truncate(index);
    }
    const PREFIXES: [&str; 8] = [
        "https://doi.org/",
        "http://doi.org/",
        "doi:",
        "https://arxiv.org/abs/",
        "http://arxiv.org/abs/",
        "https://arxiv.org/pdf/",
        "http://arxiv.org/pdf/",
        "arxiv:",
    ];
    for prefix in PREFIXES {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest.to_owned();
            break;
        }
    }
    // arXiv 的版本后缀（`2401.01234v3` → `2401.01234`）：
    // 只在"v 后面全是数字，且前面出现过数字"时裁掉，避免误伤普通英文标识。
    if let Some(position) = value.rfind('v') {
        let suffix = &value[position + 1..];
        let looks_like_version = !suffix.is_empty()
            && suffix.chars().all(|c| c.is_ascii_digit())
            && value[..position].chars().any(|c| c.is_ascii_digit());
        if looks_like_version {
            value.truncate(position);
        }
    }
    while value.ends_with('/') {
        value.pop();
    }
    if value.is_empty() {
        return None;
    }
    Some(value)
}

/// 标识 → 月度哈希（客户端上报、服务端索引必须逐字节一致）。
///
/// **这是密钥化摘要，不是 HMAC**：密钥（月盐）公开下发给客户端，
/// 所以它只提供"跨月不可链接"与"客户端算不出别人的哈希"，不提供来源认证。
pub fn hash_identifier(salt: &str, identifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    // 分隔符必须有：否则 salt="a"+id="bc" 与 salt="ab"+id="c" 会撞
    hasher.update(b"\n");
    hasher.update(identifier.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 写一批已知语料到 `rag_known_docs`（索引 CLI 与测试共用）。**不写 hash**。
///
/// `retention_months > 0` 时顺手删掉更老的月份（隐私 + 体积）。
pub async fn index_known_docs(
    pool: &PgPool,
    month: &str,
    docs: &[(String, String)],
    retention_months: u32,
) -> Result<usize, sqlx::Error> {
    let mut written = 0usize;
    for (identifier, source) in docs {
        let Some(canonical) = canonical_identifier(identifier) else {
            tracing::warn!(raw_len = identifier.len(), "rag index：跳过畸形标识");
            continue;
        };
        sqlx::query(
            "INSERT INTO rag_known_docs (month, identifier, source) VALUES ($1, $2, $3) \
             ON CONFLICT (month, identifier) DO UPDATE SET source = EXCLUDED.source",
        )
        .bind(month)
        .bind(&canonical)
        .bind(source)
        .execute(pool)
        .await?;
        written += 1;
    }

    if retention_months > 0 {
        // 只保留最近 N 个月：`YYYY-MM` 是定长字符串，字典序即时间序
        let cutoff = previous_months(month, retention_months);
        sqlx::query("DELETE FROM rag_known_docs WHERE month < $1")
            .bind(&cutoff)
            .execute(pool)
            .await?;
    }
    Ok(written)
}

/// 清理过期的月度盐（ADR-19 决策 2 的保留期要求；与语料保留期**分开配置**）。
///
/// 历史盐是"跨月链接"的唯一钥匙：删掉旧盐后，即使有人拿到旧月份的哈希，
/// 也无法证明"两个月里的同一条哈希是同一个标识"。
pub async fn prune_salts(
    pool: &PgPool,
    current_month: &str,
    retention_months: u32,
) -> Result<u64, sqlx::Error> {
    if retention_months == 0 {
        return Ok(0);
    }
    let cutoff = previous_months(current_month, retention_months);
    let result = sqlx::query("DELETE FROM rag_salts WHERE month < $1")
        .bind(&cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// `month` 往前推 `count` 个月的月份串（用于保留期）。
pub fn previous_months(month: &str, count: u32) -> String {
    let mut parts = month.split('-');
    let (Some(year), Some(month_number)) = (parts.next(), parts.next()) else {
        return month.to_owned();
    };
    let (Ok(year), Ok(month_number)) = (year.parse::<i32>(), month_number.parse::<u32>()) else {
        return month.to_owned();
    };
    let total = year * 12 + (month_number as i32 - 1) - count as i32;
    let (new_year, new_month) = (total.div_euclid(12), total.rem_euclid(12) + 1);
    format!("{new_year}-{new_month:02}")
}

/// 真实抓取器：按 `source` 分派 HTTP 请求。
///
/// **默认不启用**（`RAG_FETCH_ENABLED=false`）：CI 与离线环境都不允许联网抓取。
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    pub fn new(timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            // 与上游网关同样不跟随重定向：避免被带去任意地址
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("yunong-rag/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { client })
    }

    /// `source` + 规范化标识 → URL。未知来源直接判永久失败（不猜 URL）。
    fn url_for(target: &FetchTarget) -> Option<String> {
        match target.source.as_str() {
            "arxiv" => Some(format!(
                "https://export.arxiv.org/api/query?id_list={}",
                target.identifier
            )),
            "doi" => Some(format!("https://doi.org/{}", target.identifier)),
            "web" => target
                .identifier
                .starts_with("https://")
                .then(|| target.identifier.clone()),
            _ => None,
        }
    }
}

impl Fetcher for HttpFetcher {
    fn fetch<'a>(&'a self, target: &'a FetchTarget) -> FetchFuture<'a> {
        Box::pin(async move {
            let Some(url) = Self::url_for(target) else {
                return Err(FetchError::permanent(format!(
                    "未知来源 {}（不猜 URL）",
                    target.source
                )));
            };
            let response = self
                .client
                .get(&url)
                .send()
                .await
                .map_err(|error| FetchError::retryable(format!("请求失败：{error}")))?;
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(FetchError::permanent("404：资源不存在"));
            }
            if !status.is_success() {
                return Err(FetchError::retryable(format!("HTTP {status}")));
            }
            let body = response
                .bytes()
                .await
                .map_err(|error| FetchError::retryable(format!("读响应失败：{error}")))?;
            Ok(String::from_utf8_lossy(&body).into_owned())
        })
    }
}

/// 本轮抓取的"人类可读"摘要（只含数量，不含标识）。
pub fn summarize(report: &FetchReport) -> String {
    format!(
        "processed={} fetched={} failed={} skipped={} contended={}",
        report.processed, report.fetched, report.failed, report.skipped, report.contended
    )
}

/// 当前 UTC 月（给 CLI 用，避免把 `RagService` 传下去）。
pub fn current_month() -> String {
    RagService::month_of(OffsetDateTime::now_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_identifier_normalizes_common_shapes() {
        let cases = [
            (" arXiv:2401.01234v3 ", "2401.01234"),
            ("https://arxiv.org/abs/2401.01234", "2401.01234"),
            ("https://doi.org/10.1234/ABC", "10.1234/abc"),
            ("doi:10.1234/abc", "10.1234/abc"),
            (
                "https://Example.COM/Path/?utm_source=x#frag",
                "https://example.com/path",
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                canonical_identifier(raw).as_deref(),
                Some(expected),
                "规范化不对：{raw}"
            );
        }
        // 畸形输入
        assert_eq!(canonical_identifier("  "), None);
        assert_eq!(
            canonical_identifier(&"x".repeat(MAX_IDENTIFIER_BYTES + 1)),
            None
        );
        assert_eq!(canonical_identifier("#only-fragment"), None);
    }

    #[test]
    fn hash_is_stable_and_salt_bound() {
        let canonical = "2401.01234";
        let a = hash_identifier("salt-a", canonical);
        assert_eq!(
            a,
            hash_identifier("salt-a", canonical),
            "同盐同标识必须稳定"
        );
        assert_ne!(a, hash_identifier("salt-b", canonical), "换盐必须换哈希");
        assert_eq!(a.len(), 64);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // 分隔符必须参与哈希：否则 salt="a"+id="bc" 与 salt="ab"+id="c" 会撞
        assert_ne!(
            hash_identifier("a", "bc"),
            hash_identifier("ab", "c"),
            "必须用分隔符隔开，避免前缀歧义"
        );
    }

    #[test]
    fn hash_prefix_is_eight_chars_and_never_full() {
        let hash = hash_identifier("salt", "some-doc");
        let prefix = hash_prefix(&hash);
        assert_eq!(prefix.len(), HASH_PREFIX_LEN);
        assert!(hash.starts_with(&prefix));
        assert_ne!(prefix, hash, "日志前缀不能等于完整哈希");
        assert_eq!(hash_prefix("abc"), "abc");
    }

    #[test]
    fn previous_months_handles_year_boundaries() {
        assert_eq!(previous_months("2026-11", 1), "2026-10");
        assert_eq!(previous_months("2026-01", 1), "2025-12");
        assert_eq!(previous_months("2026-01", 3), "2025-10");
        assert_eq!(previous_months("bogus", 3), "bogus");
    }

    #[test]
    fn rate_limit_wait_is_bounded_by_min_interval() {
        // 刚抓过 → 需要等满一个间隔
        let now = std::time::Instant::now();
        let wait = remaining_wait(now, Duration::from_millis(500)).expect("应需要等待");
        assert!(wait <= Duration::from_millis(500));
        assert!(wait > Duration::from_millis(400));
        // 间隔为 0 → 不限速
        assert_eq!(remaining_wait(now, Duration::ZERO), None);
        // 已经等够了 → 不用等（用过去的时刻模拟）
        let long_ago = now - Duration::from_secs(10);
        assert_eq!(remaining_wait(long_ago, Duration::from_millis(500)), None);
    }

    #[test]
    fn summarize_reports_only_counts() {
        let report = FetchReport {
            processed: 3,
            fetched: 1,
            failed: 1,
            skipped: 1,
            contended: 0,
        };
        let text = summarize(&report);
        assert!(text.contains("fetched=1"));
        assert!(!text.contains("user"), "摘要里不该出现用户概念");
    }
}

/// **跨语言 RAG 向量的事实源**（端侧 Flutter 的 `canonical()` / `hashIdentifier()` 与这里对齐）。
///
/// 生成：`cargo test -p yunong-server rag_vectors -- --ignored`
/// 守卫：`cargo test -p yunong-server rag_vectors`（CI 跑的就是这条）
#[cfg(test)]
mod rag_vectors_tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    /// 固定测试盐（**不要随机**：向量文件必须是确定性的）。
    ///
    /// 注意它和线上"按月轮换的盐"无关：这里只需要两端用同一串来算同一个哈希。
    const VECTORS_SALT: &str = "2026-07-rag-salt";

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct VectorsFile {
        salt: String,
        version: u32,
        /// 给两个团队看的说明（程序不读）
        note: String,
        /// 能被规范化的用例
        cases: Vec<VectorCase>,
        /// 必须被拒绝的原始输入（`canonical_identifier` 返回 `None`）
        rejects: Vec<String>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct VectorCase {
        raw: String,
        canonical: String,
        /// 64 位小写十六进制
        hash: String,
    }

    /// 仓库根（`crates/server` 往上两级）。
    fn repo_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn vectors_path() -> std::path::PathBuf {
        repo_root().join("docs/rag_vectors.json")
    }

    /// 覆盖契约里每一条规则（含边界）。
    fn accepted_inputs() -> Vec<&'static str> {
        vec![
            // utm_* + fragment
            "https://example.com/article?utm_source=wechat&utm_medium=social#comments",
            // 大小写 + 尾斜杠
            "https://Example.COM/Path/To/Paper/",
            // http:// 保持（**不**归一成 https）
            "http://example.com/paper",
            // arXiv abs + 版本号
            "https://arxiv.org/abs/2401.01234v3",
            // arXiv pdf + 两位版本号（v12）
            "https://arxiv.org/pdf/2401.01234v12",
            // arxiv: 前缀
            "arxiv:2401.01234v2",
            // doi: 前缀
            "doi:10.1145/1234567.7654321",
            // doi.org URL
            "https://doi.org/10.1145/1234567.7654321",
            // http 版 doi.org + 大小写
            "http://doi.org/10.1000/ABC",
            // /v2/ 属于路径，**不该**被当版本号裁掉
            "https://example.com/v2/docs",
            // 只砍 fragment
            "https://example.com/doc#section-2",
            // 多重尾斜杠
            "https://example.com/a/b///",
            // 前后空白
            "   https://example.com/spaced   ",
            // 全大写 arXiv
            "HTTPS://ARXIV.ORG/ABS/2401.01234V3",
            // query 与 fragment 同时存在
            "https://example.com/doc?v=2#top",
            // 带数字的域名 + /v2/：记录**当前**行为（数字在前，会被当版本号），
            // 将来若要修这条规则，守卫会红，提醒同步两端
            "https://site1.example.com/v2/",
            // 裸 DOI 带版本后缀
            "10.1000/xyz123v2",
            // 路径里带数字（不该被当版本号）
            "https://example.com/2024/12/article",
            // 没有尾斜杠的 /v2：当前实现会把它当**版本号**裁掉（"v 前出现过数字"这条规则的副作用，
            // 因为域名里的 site1 含数字）。记录现状是为了让两端行为一致；
            // 将来要修这条规则，守卫会红，提醒同步端侧与本文档。
            "https://site1.example.com/v2",
            // v10 但没有前导数字：不该被当版本号
            "https://example.com/notes/v10",
        ]
    }

    fn rejected_inputs() -> Vec<&'static str> {
        vec![
            "",
            "   ",
            // 只有前缀，归一后为空
            "doi:",
            // 归一后为空
            "#only-fragment",
            "?only-query",
        ]
    }

    fn build_vectors() -> VectorsFile {
        let cases = accepted_inputs()
            .into_iter()
            .map(|raw| {
                let canonical = canonical_identifier(raw)
                    .unwrap_or_else(|| panic!("这条用例不该被拒绝：{raw}"));
                let hash = hash_identifier(VECTORS_SALT, &canonical);
                VectorCase {
                    raw: raw.to_owned(),
                    canonical,
                    hash,
                }
            })
            .collect();
        VectorsFile {
            salt: VECTORS_SALT.to_owned(),
            version: 1,
            note: "由 crates/server 的 canonical_identifier()/hash_identifier() 生成；\
                   端侧 Flutter 实现必须逐条对齐。改规则请重新生成：\
                   cargo test -p yunong-server rag_vectors -- --ignored"
                .to_owned(),
            cases,
            rejects: rejected_inputs().into_iter().map(str::to_owned).collect(),
        }
    }

    /// 生成器（默认 ignore，避免每次跑测试都写文件）。
    #[test]
    #[ignore = "生成器：cargo test -p yunong-server rag_vectors -- --ignored"]
    fn generate_rag_vectors() {
        let vectors = build_vectors();
        let text = serde_json::to_string_pretty(&vectors).expect("序列化失败");
        let path = vectors_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("建 docs 目录失败");
        }
        std::fs::write(&path, format!("{text}\n")).expect("写向量文件失败");
        println!(
            "已生成 {}（{} 条用例 + {} 条拒绝）",
            path.display(),
            vectors.cases.len(),
            vectors.rejects.len()
        );
    }

    /// 守卫：文件必须与当前实现**逐条一致**（服务端改规则忘了同步 → CI 红）。
    #[test]
    fn rag_vectors_match_implementation() {
        let path = vectors_path();
        let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!(
                "读不到 {}：{err}（首次请先跑 generate_rag_vectors 生成）",
                path.display()
            )
        });
        let actual: VectorsFile = serde_json::from_str(&text).expect("向量文件不是合法 JSON");
        let expected = build_vectors();
        assert_eq!(actual.salt, expected.salt, "盐对不上：端侧会整片 join 失败");
        assert_eq!(actual.version, expected.version, "向量文件版本对不上");
        assert_eq!(
            actual.cases.len(),
            expected.cases.len(),
            "用例条数对不上（{} vs {}）",
            actual.cases.len(),
            expected.cases.len()
        );
        for (got, want) in actual.cases.iter().zip(expected.cases.iter()) {
            assert_eq!(got.raw, want.raw);
            assert_eq!(
                got.canonical, want.canonical,
                "规范化结果变了：{:?} → 文件里是 {:?}，实现给出 {:?}。\
                 若这是**有意**改规则，请重新生成 docs/rag_vectors.json 并通知端侧",
                got.raw, got.canonical, want.canonical
            );
            assert_eq!(got.hash, want.hash, "哈希变了：{:?}", got.raw);
        }
        assert_eq!(
            actual.rejects, expected.rejects,
            "拒绝清单对不上（畸形输入的行为变了）"
        );
    }

    /// 哈希必须是 64 位小写十六进制——之前踩过"少一个 hex 字符 → 永远 0 命中"的坑。
    #[test]
    fn every_hash_is_64_lowercase_hex() {
        let vectors = build_vectors();
        assert!(
            vectors.cases.len() >= 15,
            "用例太少：{}（至少要 15 条）",
            vectors.cases.len()
        );
        for case in &vectors.cases {
            assert_eq!(
                case.hash.len(),
                64,
                "哈希长度是 {}，不是 64：{:?}",
                case.hash.len(),
                case.raw
            );
            assert!(
                case.hash
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "哈希含非小写 hex：{}",
                case.hash
            );
        }
        // 拒绝清单：实现确实返回 None
        for raw in rejected_inputs() {
            assert_eq!(
                canonical_identifier(raw),
                None,
                "这条应当被拒绝却通过了：{raw:?}"
            );
        }
    }

    /// 不同盐必须给出不同哈希（salt 参与计算，不是形参摆设）。
    #[test]
    fn vectors_are_salt_specific() {
        let vectors = build_vectors();
        let other = hash_identifier("2026-08-rag-salt", &vectors.cases[0].canonical);
        assert_ne!(other, vectors.cases[0].hash);
    }
}
