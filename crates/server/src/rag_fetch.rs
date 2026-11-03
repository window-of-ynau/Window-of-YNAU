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

/// 单条标识的最大长度（规范化之前），按 **UTF-8 字节** 计。
///
/// ⚠️ 跨语言陷阱（t77 顺带查出来的）：Rust 的 `str::len()` 是 UTF-8 字节数，
/// 而 Dart 的 `String.length` 是 **UTF-16 码元**数 —— 171 个汉字是 513 字节但只有 171 个码元。
/// 端侧必须用 `utf8.encode(s).length` 来判这条上限，否则"171 个汉字"在两端会有不同结论。
/// 向量文件里有一条 513 字节的拒绝用例来钉住这条。
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

/// 抓取结果（t81 / M4）：正文 + 归一化 Content-Type。
///
/// 为什么不再返回裸 `String`：没有 Content-Type 就无法判断"这是 HTML、Atom 还是 PDF"，
/// 只能把原始字节当文本存进 `doc_text`；PDF 更是直接变成垃圾。类型判断放在抓取器里
/// （它能看到响应头），清洗/落库判断放在 [`extract_text`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedDoc {
    /// 归一化后的 MIME（小写、去掉 `; charset=…` 参数）
    pub content_type: String,
    /// 原始字节；**字节上限在抓取器里就卡住**（`FetchConfig::max_body_bytes`）
    pub body: Vec<u8>,
}

/// 抓取器的盒子 future（不引入 `async-trait` 依赖）。
pub type FetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FetchedDoc, FetchError>> + Send + 'a>>;

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
    /// `fetching` 占位的过期阈值（t81 / M1）：早于它才允许被回收
    pub stale_after: Duration,
    /// 正文字节上限（t81 / M4）
    pub max_body_bytes: usize,
    /// DOI 重定向白名单（t81 / M3）
    pub doi_allowed_hosts: Vec<String>,
}

impl FetchConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            top_n: config.rag_fetch_top_n.max(1),
            retries: config.rag_fetch_retries,
            backoff: Duration::from_millis(config.rag_fetch_backoff_ms),
            // M8：运维必须有手段重试失败项（旧实现把 false 写死在代码里）
            retry_failed: config.rag_fetch_retry_failed,
            min_interval: Duration::from_millis(config.rag_fetch_min_interval_ms),
            stale_after: Duration::from_secs(config.rag_fetch_stale_after_secs.max(1)),
            max_body_bytes: config.rag_fetch_max_body_bytes.max(1),
            doi_allowed_hosts: config.rag_fetch_doi_allowed_hosts.clone(),
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

/// 落库结果（t81 / M5）：两步写必须能被区分，不能"看起来成功"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// 事务提交成功
    Stored,
    /// `rag_known_docs` 里没有对应行 → 回滚（否则候选行会带一个**悬空** `doc_id`）
    MissingKnownDoc,
    /// 候选行已不是 `fetching`（别的 worker 回收/改掉了）→ 回滚
    Contended,
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

        // 崩溃恢复（t81 / M1）：**只回收超时未更新的 fetching**。
        //
        // 旧实现无条件 `UPDATE … WHERE status='fetching'`：多 worker 同时跑时，后启动的那个
        // 会把前一个**正在抓**的占位抢回 pending，于是同一批候选被重复抓取、结果互相覆盖。
        // 现在按 `claimed_at` 判过期；`claimed_at IS NULL`（老库升级前的历史行）视为最老、必回收。
        let stale_secs = i64::try_from(self.config.stale_after.as_secs()).unwrap_or(i64::MAX);
        sqlx::query(
            "UPDATE rag_candidates SET status = 'pending', claimed_at = NULL \
             WHERE month = $1 AND status = 'fetching' \
               AND (claimed_at IS NULL OR claimed_at < now() - make_interval(secs => $2))",
        )
        .bind(&month)
        .bind(stale_secs as f64)
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
                "UPDATE rag_candidates SET status = 'fetching', last_error = NULL, claimed_at = now() \
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
                    "UPDATE rag_candidates SET status = 'skipped', last_error = 'no_known_doc', \
                         claimed_at = NULL \
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
                    // M5：两步写包进**同一个事务**并检查影响行数
                    match self
                        .store_fetch_result(&month, &hash, &target.identifier, &text, attempts)
                        .await?
                    {
                        StoreOutcome::Stored => {
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
                        StoreOutcome::MissingKnownDoc => {
                            // 已知语料行在 join 之后被删了：宁可失败也不要写悬空 doc_id
                            report.failed += 1;
                            tracing::warn!(
                                hash_prefix = %hash_prefix(&hash),
                                source = %target.source,
                                status = "failed",
                                error = "known_doc_missing",
                                "rag fetch：已知语料行已不存在，已回滚（不写悬空 doc_id）"
                            );
                        }
                        StoreOutcome::Contended => {
                            report.contended += 1;
                            tracing::warn!(
                                hash_prefix = %hash_prefix(&hash),
                                status = "contended",
                                "rag fetch：落库时候选行已不是 fetching（被别的 worker 回收？），已回滚"
                            );
                        }
                    }
                }
                None => {
                    sqlx::query(
                        "UPDATE rag_candidates SET status = 'failed', attempts = $3, last_error = $4, \
                             claimed_at = NULL \
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

    /// 落抓取结果（t81 / M5）：两步写**同一事务** + 检查 `rows_affected`。
    ///
    /// 旧实现是两次独立 `UPDATE` 且不查影响行数：如果 `rag_known_docs` 那行已被清理，
    /// 候选行仍会被标成 `fetched` 并写入 `doc_id` —— 库里留下一个**悬空引用**，
    /// 下游按 `doc_id` 反查正文时会拿到 NULL，且没有任何告警。
    pub async fn store_fetch_result(
        &self,
        month: &str,
        hash: &str,
        identifier: &str,
        text: &str,
        attempts: u32,
    ) -> Result<StoreOutcome, FetchPipelineError> {
        let mut tx = self.pool.begin().await?;
        let docs = sqlx::query(
            "UPDATE rag_known_docs SET fetched_at = now(), doc_text = $3 \
             WHERE month = $1 AND identifier = $2",
        )
        .bind(month)
        .bind(identifier)
        .bind(text)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if docs == 0 {
            tx.rollback().await.ok();
            return Ok(StoreOutcome::MissingKnownDoc);
        }

        let candidates = sqlx::query(
            "UPDATE rag_candidates \
             SET status = 'fetched', attempts = $3, last_error = NULL, \
                 fetched_at = now(), doc_id = $4, claimed_at = NULL \
             WHERE month = $1 AND hash = $2 AND status = 'fetching'",
        )
        .bind(month)
        .bind(hash)
        .bind(i32::try_from(attempts).unwrap_or(i32::MAX))
        .bind(identifier)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if candidates != 1 {
            tx.rollback().await.ok();
            return Ok(StoreOutcome::Contended);
        }

        tx.commit().await?;
        Ok(StoreOutcome::Stored)
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
                Ok(doc) => match extract_text(&doc.content_type, &doc.body) {
                    Ok(text) => return (attempts, String::new(), Some(text)),
                    Err(error) => {
                        // 类型不支持 / 清洗后为空：**永久失败**，重试没有意义
                        last_error = error.message;
                        if !error.retryable {
                            break;
                        }
                    }
                },
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

/// **固定 ASCII 空白集合**（t77 的跨语言契约）。
///
/// 只有这 6 个字符会被裁掉；`U+FEFF`（BOM）、`U+00A0`（NBSP）、`U+2028` 等都**不裁**——
/// 它们在不同语言的 `trim()` 里行为不一致（Dart 裁、Rust 不裁），正是 t77 的根因。
const ASCII_WHITESPACE: [char; 6] = [' ', '\t', '\n', '\r', '\x0c', '\x0b'];

/// 只裁 [`ASCII_WHITESPACE`] 里的字符（两端）。
///
/// 不能直接用 `str::trim()`：它是 Unicode White_Space 语义，但**不含** U+FEFF；
/// 而 Dart 的 `String.trim()` **含** U+FEFF —— 两端就此分叉。
fn trim_ascii_whitespace(raw: &str) -> &str {
    raw.trim_matches(|ch: char| ASCII_WHITESPACE.contains(&ch))
}

/// 规范化标识（契约见模块文档）。返回 `None` 表示畸形输入（空 / 超长 / 归一后为空）。
///
/// # 规则是 **ASCII-only**，不是 Unicode 语义（t77）
///
/// 标识本来就是 URL / DOI / arXiv 这类 ASCII 文本，不需要 Unicode case folding。
/// 用 Unicode 语义会**跨语言不一致**，实测两类：
///
/// - `İ`（U+0130）：Rust 的 `to_lowercase()` 走完整映射 → `i` + U+0307（2 个码点），
///   Dart 的 `toLowerCase()` → 单个 `i` ⇒ 两端哈希不同；
/// - U+FEFF（BOM）：Dart 的 `trim()` 把它当空白裁掉，Rust 的 `str::trim()` 不裁
///   （BOM 不属于 Unicode White_Space）⇒ 粘贴带 BOM 的引用文本就会 0 命中。
///
/// 所以这里只做两件**逐字节确定**的事：
/// 1. 裁**固定 ASCII 空白集合**（空格 / `\t` / `\n` / `\r` / `\x0c` / `\x0b`），
///    其它任何字符（含 BOM、NBSP、U+2028）都**原样保留**；
/// 2. 只把 ASCII `A–Z` 转小写（`to_ascii_lowercase`），非 ASCII 字符大小写**不动**；
/// 3. 不做 NFC/NFKC 归一化、不做 Unicode case folding。
///
/// 端侧（Dart）必须实现同一套规则：`String.trim()` 是 Unicode 感知的，**不能直接用它**。
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
    let trimmed = trim_ascii_whitespace(raw);
    if trimmed.is_empty() || trimmed.len() > MAX_IDENTIFIER_BYTES {
        return None;
    }
    // 只转 ASCII：非 ASCII（İ/Ü/П/全角…）一律原样保留，保证两端逐字节一致
    let mut value = trimmed.to_ascii_lowercase();
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

/// 落库正文的字符上限（t81 / M4）：200k 字符 ≈ 教科书一章，足够 RAG 用。
pub const MAX_DOC_TEXT_CHARS: usize = 200_000;

/// DOI 跟随重定向的最大跳数。
const MAX_DOI_REDIRECTS: usize = 5;

/// 允许落库的 Content-Type（t81 / M4）。
///
/// **PDF 不在白名单**：我们不解析 PDF，存字节进去只会让 `doc_text` 变成二进制垃圾；
/// 判永久失败比"存进去看起来成功"诚实。
fn is_supported_content_type(mime: &str) -> bool {
    matches!(
        mime,
        "text/plain"
            | "text/html"
            | "application/xhtml+xml"
            | "application/atom+xml"
            | "application/xml"
            | "text/xml"
    )
}

/// 是不是私网/回环/链路本地地址（t81 / M3：重定向白名单之外的第二道闸）。
///
/// 白名单按域名匹配，但攻击者可以控制一个**白名单域名的子域**指向内网；
/// 所以即使域名匹配，只要解析出来是字面量 IP 且属于私网段，也一律拒绝。
fn is_private_host(host: &str) -> bool {
    let lower = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }
    match lower.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            let segments = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (segments[0] & 0xfe00) == 0xfc00 // fc00::/7 唯一本地地址
                || (segments[0] & 0xffc0) == 0xfe80 // fe80::/10 链路本地
        }
        Err(_) => false,
    }
}

/// 重定向目标是否在白名单内（支持 `*.example.com` 通配）。
///
/// 纯函数，便于单测；网络路径上由 [`HttpFetcher`] 的 redirect policy 调用。
pub fn is_allowed_doi_host(host: &str, allowed: &[String]) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || is_private_host(&host) {
        return false;
    }
    allowed.iter().any(|pattern| {
        let pattern = pattern.trim().to_ascii_lowercase();
        if pattern.is_empty() {
            return false;
        }
        match pattern.strip_prefix("*.") {
            Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
            None => host == pattern,
        }
    })
}

/// 真实抓取器：按 `source` 分派 HTTP 请求。
///
/// **默认不启用**（`RAG_FETCH_ENABLED=false`）：CI 与离线环境都不允许联网抓取。
///
/// # 为什么要两个 client（t81 / M3）
///
/// 这与上游模型 API 的 `Policy::none()`（M2 / SSRF 修复）**不冲突，是两套风险模型**：
/// - 上游模型 API：地址是我们**自己配置**的，跟随重定向只是给攻击者一个"把我们带去内网"的机会，
///   收益为零 → 一律不跟；
/// - 抓取器：抓的是**外网公开资源**，doi.org 的 302 跳到出版商是它的正常工作方式，
///   不跟等于 DOI 源 100% 不可用（旧实现的真实状态）。
///
/// 所以这里对 DOI 开"**有限 + 白名单**"的跟随：最多 5 跳、只允许 https、
/// 目标域名必须在 `RAG_FETCH_DOI_ALLOWED_HOSTS` 内、且不能是私网/回环地址。
/// arxiv / web 仍然完全不跟（URL 由标识直接构造，重定向没有正当理由）。
pub struct HttpFetcher {
    /// arxiv / web：不跟随重定向
    plain: reqwest::Client,
    /// doi：白名单内的有限跟随
    doi: reqwest::Client,
    max_body_bytes: usize,
}

impl HttpFetcher {
    pub fn new(
        timeout: Duration,
        max_body_bytes: usize,
        doi_allowed_hosts: Vec<String>,
    ) -> Result<Self, reqwest::Error> {
        let user_agent = concat!("yunong-rag/", env!("CARGO_PKG_VERSION"));
        let plain = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            // 与上游网关同样不跟随重定向：避免被带去任意地址
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(user_agent)
            .build()?;

        let allowed = std::sync::Arc::new(doi_allowed_hosts);
        let doi = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_DOI_REDIRECTS {
                    return attempt.error("重定向次数过多");
                }
                if attempt.url().scheme() != "https" {
                    return attempt.error("只允许 https 重定向");
                }
                let host = attempt.url().host_str().unwrap_or_default().to_owned();
                if !is_allowed_doi_host(&host, &allowed) {
                    return attempt.error(format!("重定向目标不在白名单：{host}"));
                }
                attempt.follow()
            }))
            .user_agent(user_agent)
            .build()?;

        Ok(Self {
            plain,
            doi,
            max_body_bytes,
        })
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
            let client = if target.source == "doi" {
                &self.doi
            } else {
                &self.plain
            };
            let response = client
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

            // M4：类型不在白名单就判永久失败（重试也不会变）
            let mime = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let mime = if mime.is_empty() {
                "text/plain".to_owned()
            } else {
                mime
            };
            if !is_supported_content_type(&mime) {
                return Err(FetchError::permanent(format!(
                    "不支持的 Content-Type：{mime}（不落原始二进制）"
                )));
            }

            // M4：先看 Content-Length，再在读完后再核一次（有的服务端不报长度）
            if let Some(length) = response.content_length() {
                if length > self.max_body_bytes as u64 {
                    return Err(FetchError::permanent(format!(
                        "正文过大：{length} 字节 > 上限 {}",
                        self.max_body_bytes
                    )));
                }
            }
            let body = response
                .bytes()
                .await
                .map_err(|error| FetchError::retryable(format!("读响应失败：{error}")))?;
            if body.len() > self.max_body_bytes {
                return Err(FetchError::permanent(format!(
                    "正文过大：{} 字节 > 上限 {}",
                    body.len(),
                    self.max_body_bytes
                )));
            }
            Ok(FetchedDoc {
                content_type: mime,
                body: body.to_vec(),
            })
        })
    }
}

/// 把响应正文清洗成可落库的纯文本（t81 / M4）。
///
/// 这是**粗糙清洗**，正式清洗管线（去导航/去参考文献/分段）另排期；
/// 现阶段的目标只有一个：**不要把原始 HTML/Atom 直接塞进 `doc_text`**。
pub fn extract_text(content_type: &str, body: &[u8]) -> Result<String, FetchError> {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let raw = String::from_utf8_lossy(body);
    let text = match mime.as_str() {
        "" | "text/plain" => raw.into_owned(),
        "text/html" | "application/xhtml+xml" => strip_markup(&raw, true),
        "application/atom+xml" | "application/xml" | "text/xml" => strip_markup(&raw, false),
        other => {
            return Err(FetchError::permanent(format!(
                "不支持的 Content-Type：{other}"
            )))
        }
    };
    let cleaned = collapse_whitespace(&text);
    if cleaned.is_empty() {
        return Err(FetchError::permanent("清洗后正文为空"));
    }
    Ok(cleaned.chars().take(MAX_DOC_TEXT_CHARS).collect())
}

/// 去标签 + 解实体（`strip_scripts` 时连 `<script>/<style>` 的内容一起丢掉）。
fn strip_markup(raw: &str, strip_scripts: bool) -> String {
    let mut out = String::with_capacity(raw.len() / 2);
    let mut chars = raw.char_indices().peekable();
    let mut in_script_until: Option<&'static str> = None;
    while let Some((index, ch)) = chars.next() {
        if in_script_until.is_some() {
            if ch == '<' {
                // 用**字节**切片比较：`&str[..n]` 会在非 ASCII 字符中间 panic
                let rest = raw.as_bytes().get(index..).unwrap_or_default();
                let closing = in_script_until.unwrap_or("</");
                // 注意：`closing` 本身就带 `</`，所以比较长度就是 `closing.len()`
                if rest
                    .get(..closing.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(closing.as_bytes()))
                {
                    in_script_until = None;
                    // 把收尾的 `>` 也吃掉，避免 `</script>` 的残渣混进正文
                    for (_, inner) in chars.by_ref() {
                        if inner == '>' {
                            break;
                        }
                    }
                }
            }
            continue;
        }
        if ch == '<' {
            let rest = raw.as_bytes().get(index..).unwrap_or_default();
            if strip_scripts {
                if rest
                    .get(..7)
                    .is_some_and(|head| head.eq_ignore_ascii_case(b"<script"))
                {
                    in_script_until = Some("</script");
                } else if rest
                    .get(..6)
                    .is_some_and(|head| head.eq_ignore_ascii_case(b"<style"))
                {
                    in_script_until = Some("</style");
                }
            }
            // 跳过整个标签
            for (_, inner) in chars.by_ref() {
                if inner == '>' {
                    break;
                }
            }
            out.push(' ');
            continue;
        }
        out.push(ch);
    }
    decode_entities(&out)
}

/// 解常见 HTML 实体（够用即可，不做完整实体表）。
fn decode_entities(raw: &str) -> String {
    raw.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// 折叠空白：连续空白（含换行）压成一个空格，两端裁掉。
fn collapse_whitespace(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out.trim_end().to_owned()
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

    /// t77：ASCII 空白才裁，Unicode 空白与 BOM **原样保留**。
    #[test]
    fn ascii_only_trimming_keeps_bom_and_nbsp() {
        // 6 个 ASCII 空白都被裁
        for raw in [
            "\thttps://example.com/a\n",
            "\r\x0c\x0bhttps://example.com/a \t\n",
        ] {
            assert_eq!(
                canonical_identifier(raw).as_deref(),
                Some("https://example.com/a"),
                "ASCII 空白应被裁：{raw:?}"
            );
        }
        // BOM / NBSP / U+2028 不是 ASCII 空白 → 必须原样保留
        for (raw, expected) in [
            (
                "\u{feff}https://example.com/z",
                "\u{feff}https://example.com/z",
            ),
            (
                "https://example.com/z\u{feff}",
                "https://example.com/z\u{feff}",
            ),
            (
                "\u{00a0}https://example.com/z",
                "\u{00a0}https://example.com/z",
            ),
            (
                "\u{2028}https://example.com/z",
                "\u{2028}https://example.com/z",
            ),
        ] {
            assert_eq!(
                canonical_identifier(raw).as_deref(),
                Some(expected),
                "非 ASCII 空白不得被裁（Dart/Rust trim 语义不同，t77）：{raw:?}"
            );
        }
        // 只有 ASCII 空白的输入 → 拒绝
        assert_eq!(canonical_identifier(" \t\n\r\x0c\x0b "), None);
        // 只有 BOM 的输入不是"空"（BOM 原样保留）
        assert_eq!(
            canonical_identifier("\u{feff}").as_deref(),
            Some("\u{feff}")
        );
    }

    /// t77：只做 ASCII 小写；非 ASCII 字符的大小写与组合形式**一律不动**。
    #[test]
    fn ascii_only_lowercasing_keeps_unicode_intact() {
        let cases = [
            // U+0130（İ）：Rust to_lowercase() 会产出 i + U+0307，Dart 产出 i —— 都不该发生
            ("İSTANBUL-Üniversite", "İstanbul-Üniversite"),
            // 拉丁扩展：Ü 保持大写
            ("https://example.com/Über", "https://example.com/Über"),
            // Cyrillic 保持大写
            ("https://example.com/ПРИВЕТ", "https://example.com/ПРИВЕТ"),
            // 全角字母不是 ASCII，不做小写
            ("https://example.com/ＡＢＣ", "https://example.com/ＡＢＣ"),
            // 组合符原样保留（不做 NFC）
            (
                "https://example.com/e\u{0301}",
                "https://example.com/e\u{0301}",
            ),
            // emoji 原样
            ("https://example.com/🚀", "https://example.com/🚀"),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                canonical_identifier(raw).as_deref(),
                Some(expected),
                "ASCII-only 规则不对：{raw:?}"
            );
        }
        // 关键：İ 的规范化结果里**不能**出现 U+0307（那是 to_lowercase 的产物）
        let canonical = canonical_identifier("İSTANBUL").expect("能规范化");
        assert!(
            !canonical.contains('\u{0307}'),
            "出现了 Unicode 分解产物（to_lowercase 的副作用）：{canonical:?}"
        );
    }

    /// t77：长度上限按 **UTF-8 字节** 计（不是字符数，也不是 UTF-16 码元数）。
    #[test]
    fn identifier_limit_is_utf8_bytes() {
        // 170 个汉字 = 510 字节 → 通过
        assert!(canonical_identifier(&"论".repeat(170)).is_some());
        // 171 个汉字 = 513 字节 → 拒绝（字符数只有 171，按字符算会误放行）
        assert_eq!(canonical_identifier(&"论".repeat(171)), None);
        // 512 个 ASCII 字符 = 512 字节 → 通过；513 → 拒绝
        assert!(canonical_identifier(&"a".repeat(512)).is_some());
        assert_eq!(canonical_identifier(&"a".repeat(513)), None);
    }

    /// t81 / M3：DOI 重定向白名单（纯函数，不联网）。
    #[test]
    fn doi_redirect_allowlist_is_strict() {
        let allowed: Vec<String> = vec![
            "doi.org".to_owned(),
            "*.springer.com".to_owned(),
            "ieeexplore.ieee.org".to_owned(),
        ];
        // 精确匹配 + 通配子域
        assert!(is_allowed_doi_host("doi.org", &allowed));
        assert!(is_allowed_doi_host("DOI.ORG", &allowed), "大小写不敏感");
        assert!(is_allowed_doi_host("link.springer.com", &allowed));
        assert!(
            is_allowed_doi_host("springer.com", &allowed),
            "通配也要匹配裸域"
        );
        assert!(is_allowed_doi_host("ieeexplore.ieee.org", &allowed));

        // 不在白名单
        assert!(!is_allowed_doi_host("evil.example.com", &allowed));
        assert!(!is_allowed_doi_host("springer.com.evil.example", &allowed));
        assert!(!is_allowed_doi_host("", &allowed));

        // 私网/回环/链路本地：即使在白名单里也一律拒（白名单按域名匹配挡不住 IP 直连）
        for host in [
            "localhost",
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.1",
            "169.254.169.254",
            "::1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(
                !is_allowed_doi_host(host, &allowed),
                "私网/回环地址不允许跟随重定向：{host}"
            );
        }
        // 白名单为空 → 谁都不跟（安全默认）
        assert!(!is_allowed_doi_host("doi.org", &[]));
    }

    /// t81 / M4：Content-Type 白名单（PDF 不在里面）。
    #[test]
    fn content_type_allowlist_excludes_binary() {
        for mime in [
            "text/plain",
            "text/html",
            "application/xhtml+xml",
            "application/atom+xml",
            "application/xml",
            "text/xml",
        ] {
            assert!(is_supported_content_type(mime), "应支持：{mime}");
        }
        for mime in ["application/pdf", "image/png", "application/octet-stream"] {
            assert!(!is_supported_content_type(mime), "不该落库：{mime}");
        }
    }

    /// t81 / M4：抓到的 HTML/Atom 必须先清洗成纯文本，**不能原样落库**。
    #[test]
    fn extract_text_strips_markup_and_scripts() {
        let html = r#"<html><head><style>body{color:red}</style>
<script>alert('x')</script></head>
<body><h1>论文标题</h1><p>摘要 &amp; 结论 &lt;重要&gt;</p></body></html>"#
            .as_bytes();
        let text = extract_text("text/html; charset=utf-8", html).expect("能清洗");
        assert!(text.contains("论文标题"), "{text}");
        assert!(text.contains("摘要 & 结论 <重要>"), "实体要解开：{text}");
        // 标签与脚本内容都不该出现
        for forbidden in ["<html", "<p>", "alert(", "color:red", "<h1"] {
            assert!(!text.contains(forbidden), "残留标记：{forbidden} / {text}");
        }

        // Atom（arxiv 的响应就是 atom+xml）
        let atom = r#"<?xml version="1.0"?><feed><entry><title>标题</title>
<summary>摘要正文</summary></entry></feed>"#
            .as_bytes();
        let text = extract_text("application/atom+xml", atom).expect("能清洗");
        assert!(text.contains("摘要正文"), "{text}");
        assert!(!text.contains("<summary>"), "{text}");

        // 纯文本原样保留
        assert_eq!(
            extract_text("text/plain", "  纯文本  正文 ".as_bytes()).expect("能清洗"),
            "纯文本 正文"
        );
    }

    /// t81 / M4：不支持的类型 / 清洗后为空 → **永久失败**（重试没意义）。
    #[test]
    fn extract_text_rejects_unsupported_and_empty() {
        let pdf = extract_text("application/pdf", b"%PDF-1.7 ...").expect_err("PDF 不该被接受");
        assert!(!pdf.retryable, "类型不支持是永久失败：{pdf:?}");

        let empty =
            extract_text("text/html", b"<html><body>   </body></html>").expect_err("空正文");
        assert!(!empty.retryable, "空正文是永久失败：{empty:?}");
    }

    /// t81 / M4：落库正文有字符上限（防止一篇文章把行撑爆）。
    #[test]
    fn extract_text_truncates_to_limit() {
        let long = "字".repeat(MAX_DOC_TEXT_CHARS + 500);
        let text = extract_text("text/plain", long.as_bytes()).expect("能清洗");
        assert_eq!(text.chars().count(), MAX_DOC_TEXT_CHARS);
    }

    /// t81 / M8：`retry_failed` 必须能由配置打开（旧实现写死 false，运维无手段）。
    #[test]
    fn retry_failed_comes_from_config() {
        let mut config = Config::for_tests();
        assert!(
            !FetchConfig::from_config(&config).retry_failed,
            "默认不重试"
        );
        config.rag_fetch_retry_failed = true;
        assert!(
            FetchConfig::from_config(&config).retry_failed,
            "RAG_FETCH_RETRY_FAILED=true 时本轮要纳入 failed"
        );
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
            // ——— t77：跨语言 Unicode 语料（规则：**非 ASCII 原样保留**）———
            // U+0130（İ）：Rust 的 to_lowercase() 会产出 i + U+0307、Dart 的 toLowerCase() 产 i，
            // 两端就此分叉；ASCII-only 规则下它必须原样保留
            "İSTANBUL-Üniversite",
            // U+FEFF（BOM）：Dart 的 trim() 会裁、Rust 的 trim() 不裁 —— 现在两端都不裁
            "\u{feff}https://example.com/z",
            "https://example.com/z\u{feff}",
            // U+00A0（NBSP）：不是 ASCII 空白，不裁
            "\u{00a0}https://example.com/nbsp\u{00a0}",
            // U+2028（行分隔符）：同上
            "\u{2028}https://example.com/ls",
            // CJK 路径原样
            "https://example.com/论文",
            // 拉丁扩展：只做 ASCII 小写，Ü 保持大写
            "https://example.com/Über",
            // Cyrillic 保持大写
            "https://example.com/ПРИВЕТ",
            // 全角字母不是 ASCII，不做小写
            "https://example.com/ＡＢＣ",
            // 组合符：不做 NFC 归一化
            "https://example.com/e\u{0301}",
            // emoji 原样
            "https://example.com/🚀",
            // U+0130 在路径中间
            "https://example.com/İstanbul/İ",
        ]
    }

    fn rejected_inputs() -> Vec<String> {
        vec![
            "",
            "   ",
            // 只有前缀，归一后为空
            "doi:",
            // 归一后为空
            "#only-fragment",
            "?only-query",
            // t77：全是 ASCII 空白 → 裁完为空（注意：含 BOM 的输入**不**算空）
            " \t\n\r\x0c\x0b ",
        ]
        .into_iter()
        .map(str::to_owned)
        // t77：171 个汉字 = 513 UTF-8 **字节**（> 512）但只有 171 个 UTF-16 码元。
        // 这条用例钉住"上限按字节算"；端侧若用 String.length 会算成 171 而误接受。
        .chain(std::iter::once("论".repeat(171)))
        .collect()
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
            // t77：规则从"Unicode 语义"改为"ASCII-only"，属于**破坏性变更** → 版本号 +1。
            // 端侧（Dart）必须同步实现 ASCII-only 规则，否则 join 永远 0 命中。
            version: 2,
            note: "由 crates/server 的 canonical_identifier()/hash_identifier() 生成；\
                   端侧 Flutter 实现必须逐条对齐。改规则请重新生成：\
                   cargo test -p yunong-server rag_vectors -- --ignored"
                .to_owned(),
            cases,
            rejects: rejected_inputs(),
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
                canonical_identifier(&raw),
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
