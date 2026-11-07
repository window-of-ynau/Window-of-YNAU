//! RAG 2.0 服务端地基（ADR-19 决策 2/3）：月度盐下发 + 匿名哈希聚合。
//!
//! 边界（ADR-19）：
//! - 上报体里**没有 user_id**，服务端也**不从令牌里取**——所以 [`RagService::record`]
//!   的签名里根本没有用户参数，想"顺手把 user_id 存下来"都做不到；
//! - 审计只记"这次上报了几条"，不记 hash 内容（否则审计日志就成了浏览记录）；
//! - 盐不是秘密（要下发给客户端），但**必须服务端统一下发且同月不可变**，
//!   否则各端算出的哈希对不上，聚合数据会碎片化。
//!
//! 本期只做接口 + 表结构 + 聚合；**抓取、向量化、领域包**都不在这里。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uuid::Uuid;

use crate::config::Config;

/// 月度盐的缓存视图（下发用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SaltView {
    /// `YYYY-MM`
    pub month: String,
    /// 64 位十六进制（32 字节）
    pub salt: String,
    /// 该月盐的失效时刻 = 下月 1 日 00:00 UTC（RFC 3339）
    pub expires_at: String,
}

/// RAG 服务。`AppState` 持有一份。
#[derive(Debug)]
pub struct RagService {
    db: Option<PgPool>,
    /// `db = None`（无数据库模式）时用的进程内盐：重启即变，仅用于本地/测试
    memory_salt: Mutex<Option<(String, String)>>,
    cache: Mutex<Option<CachedSalt>>,
    ttl: Duration,
    max_items_per_request: usize,
    /// 单月单客户端可用的 **count 总和** 上限（R5-F1：不是条数）
    max_items_per_month: u32,
    /// 配额计数：`(month, 客户端标识)` → 已累计条数。进程内、不落库（见 `check_quota`）。
    quota: Mutex<HashMap<(String, String), QuotaSlot>>,
}

#[derive(Debug, Clone)]
struct CachedSalt {
    month: String,
    salt: String,
    expires_at: OffsetDateTime,
    cached_at: Instant,
}

#[derive(Debug, Clone)]
struct QuotaSlot {
    /// 本月已用（**count 总和**，不是条数——见 R5-F1）
    used: u32,
    touched: Instant,
}

/// 上报被拒的原因（映射到错误码与审计原因）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RagError {
    /// 月份不合法（不是当前月，也不是上月宽限期）
    BadMonth,
    /// 单次条数超限
    TooManyItems { limit: usize },
    /// 单月累计超限（`limit` 是 **count 总和**，见 R5-F1）
    MonthlyQuota { limit: u32 },
    /// 上报里有非法哈希
    BadHash,
    /// 没有数据库（无数据库模式不支持上报）
    NoDatabase,
    /// 数据库错误
    Db(String),
}

impl RagError {
    /// 审计/日志里用的稳定原因码（可聚合，别塞自由文本之外的细节）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadMonth => "rag_bad_month",
            Self::TooManyItems { .. } => "rag_too_many_items",
            Self::MonthlyQuota { .. } => "rag_monthly_quota",
            Self::BadHash => "rag_bad_hash",
            Self::NoDatabase => "rag_no_database",
            Self::Db(_) => "rag_db_error",
        }
    }
}

/// 一条上报（`hash` 已校验为 64 位小写十六进制）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferralItem {
    pub hash: String,
    pub count: u32,
}

/// 单个 `(month, hash)` 的聚合硬上限（R5-F1）：真实文档通常个位数，
/// 10 万已远超"整个语料库被反复引用"的量级；到顶就截断，不允许无限增长。
pub const REFERRAL_COUNT_CAP: i64 = 100_000;

impl RagService {
    pub fn new(config: &Config, db: Option<PgPool>) -> Self {
        Self {
            db,
            memory_salt: Mutex::new(None),
            cache: Mutex::new(None),
            ttl: Duration::from_secs(config.rag_salt_ttl_secs),
            max_items_per_request: config.rag_max_items_per_request,
            max_items_per_month: u32::try_from(config.rag_max_items_per_month).unwrap_or(u32::MAX),
            quota: Mutex::new(HashMap::new()),
        }
    }

    pub fn max_items_per_request(&self) -> usize {
        self.max_items_per_request
    }

    /// `YYYY-MM` 的月份串（UTC）。
    pub fn month_of(now: OffsetDateTime) -> String {
        format!("{}-{:02}", now.year(), u8::from(now.month()))
    }

    /// 上一个自然月（`YYYY-MM`）。
    ///
    /// R5-F5：旧实现用 `month_expiry(now) - 1 DAY` 当月，那是**本月最后一天**，
    /// 算出来的"上月"其实还是本月 ⇒ 跨月宽限从来没生效过（12-01 会真实触发）。
    pub fn previous_month_of(now: OffsetDateTime) -> String {
        let (year, month) = (now.year(), u8::from(now.month()));
        let (previous_year, previous_month) = if month == 1 {
            (year - 1, 12u8)
        } else {
            (year, month - 1)
        };
        format!("{previous_year}-{previous_month:02}")
    }

    /// 当前时刻的"上月"（给集成测试/运维用，免得测试里再引 `time`）。
    pub fn previous_month_now() -> String {
        Self::previous_month_of(OffsetDateTime::now_utc())
    }

    /// 下月 1 日 00:00 UTC——盐的失效时刻（也是"每月 1 日自动轮换"的实现：
    /// 到点后 `month_of(now)` 变成新月，`salt()` 自然去取/生成新月的行）。
    pub fn month_expiry(now: OffsetDateTime) -> OffsetDateTime {
        let year = now.year();
        let next_month_number = match u8::from(now.month()) {
            12 => 1u8,
            other => other + 1,
        };
        let next_year = if next_month_number == 1 {
            year + 1
        } else {
            year
        };
        let month = time::Month::try_from(next_month_number).expect("1..=12");
        time::Date::from_calendar_date(next_year, month, 1)
            .expect("每月都有 1 日")
            .midnight()
            .assume_utc()
    }

    /// 当前月份（UTC）。
    pub fn current_month(&self) -> String {
        Self::month_of(OffsetDateTime::now_utc())
    }

    /// 取本月盐（带进程内 TTL 缓存；同月永不变化）。
    pub async fn salt(&self) -> Result<SaltView, RagError> {
        let now = OffsetDateTime::now_utc();
        let month = Self::month_of(now);
        let expires_at = Self::month_expiry(now);

        // 缓存命中：同一个月 + 未过期。跨月时 `month` 不等 → 自动刷新。
        if let Some(cached) = self.lock_cache().as_ref() {
            if cached.month == month && cached.cached_at.elapsed() < self.ttl {
                return Ok(SaltView {
                    month: cached.month.clone(),
                    salt: cached.salt.clone(),
                    expires_at: format_rfc3339(cached.expires_at),
                });
            }
        }

        let salt = match &self.db {
            Some(pool) => self.salt_from_db(pool, &month).await?,
            // 无数据库模式：进程内随机盐（重启即变）——仅用于本地骨架与测试
            None => self.salt_in_memory(&month),
        };

        *self.lock_cache() = Some(CachedSalt {
            month: month.clone(),
            salt: salt.clone(),
            expires_at,
            cached_at: Instant::now(),
        });
        Ok(SaltView {
            month,
            salt,
            expires_at: format_rfc3339(expires_at),
        })
    }

    /// 显式取某个历史月份的盐（测试与运维核对用；不会改动缓存）。
    pub async fn salt_for_month(&self, month: &str) -> Result<String, RagError> {
        match &self.db {
            Some(pool) => self.salt_from_db(pool, month).await,
            None => Err(RagError::NoDatabase),
        }
    }

    /// 落库盐：先查，没有就生成并插入（`ON CONFLICT DO NOTHING` 兜住并发）。
    async fn salt_from_db(&self, pool: &PgPool, month: &str) -> Result<String, RagError> {
        if let Some(existing) =
            sqlx::query_scalar::<_, String>("SELECT salt FROM rag_salts WHERE month = $1")
                .bind(month)
                .fetch_optional(pool)
                .await
                .map_err(|error| RagError::Db(error.to_string()))?
        {
            return Ok(existing);
        }

        // 生成：两次 UUID v4（244 bit 输入）+ 时间戳做 SHA-256，取 64 位十六进制。
        // 盐本身不是秘密，重要的是**同月唯一且不可被客户端各自臆造**。
        let seed = format!(
            "{}:{}:{}",
            Uuid::new_v4(),
            Uuid::new_v4(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        );
        let salt = hex_sha256(seed.as_bytes());

        sqlx::query(
            "INSERT INTO rag_salts (month, salt) VALUES ($1, $2) ON CONFLICT (month) DO NOTHING",
        )
        .bind(month)
        .bind(&salt)
        .execute(pool)
        .await
        .map_err(|error| RagError::Db(error.to_string()))?;

        // 并发下可能是别人先插入的：以库里的为准
        sqlx::query_scalar::<_, String>("SELECT salt FROM rag_salts WHERE month = $1")
            .bind(month)
            .fetch_one(pool)
            .await
            .map_err(|error| RagError::Db(error.to_string()))
    }

    fn salt_in_memory(&self, month: &str) -> String {
        let mut guard = self.lock_memory_salt();
        if let Some((cached_month, salt)) = guard.as_ref() {
            if cached_month == month {
                return salt.clone();
            }
        }
        let salt =
            hex_sha256(format!("{}:{}:{}", Uuid::new_v4(), Uuid::new_v4(), month).as_bytes());
        *guard = Some((month.to_owned(), salt.clone()));
        salt
    }

    /// 校验月份：只接受**当前月**与**上月**（客户端跨月提交的宽限期）。
    ///
    /// 为什么要卡：允许任意 `month` 就等于给了一个"往未来月份无限写"的口子，
    /// 单月配额会被绕开，聚合数据也会被污染。
    pub fn check_month(&self, month: &str) -> Result<(), RagError> {
        let now = OffsetDateTime::now_utc();
        let current = Self::month_of(now);
        if month == current {
            return Ok(());
        }
        // 正确的"上月"（跨年也对）：不能用 `month_expiry - 1 DAY`，那是本月最后一天
        if month == Self::previous_month_of(now) {
            return Ok(());
        }
        Err(RagError::BadMonth)
    }

    /// 配额**预检**（进程内，按 `(month, 客户端标识)` 累计 `sum(count)`）。
    ///
    /// R5-F1：旧实现按"条数"算配额 ⇒ 一次请求（200 条 × 单条 10000）就能给同一 hash
    /// 加 2,000,000，真实文档通常个位数 ⇒ 一个账号一次请求即可霸榜。现在按 **count 总和**算。
    ///
    /// **为什么进程内**：ADR-19 要求上报体里没有 user_id，服务端也不该为了限额
    /// 去建立"账号 → 上报量"的持久化关联。进程内计数既能挡住单机放大，
    /// 又不留可回溯的痕迹；代价是重启清零、多实例不共享（1.0 单实例，可接受）。
    pub fn check_quota(&self, month: &str, client: &str, amount: u32) -> Result<(), RagError> {
        let mut map = self.lock_quota();
        self.evict_if_needed(&mut map, month);
        let used = map
            .get(&(month.to_owned(), client.to_owned()))
            .map(|slot| slot.used)
            .unwrap_or(0);
        if used.saturating_add(amount) > self.max_items_per_month {
            return Err(RagError::MonthlyQuota {
                limit: self.max_items_per_month,
            });
        }
        Ok(())
    }

    /// 落库成功后记账（R5-F8）。
    ///
    /// 先落库、再扣配额：否则 DB 抖动期会出现"配额被扣、数据没落"，客户端被提前封月。
    ///
    /// **放大倍数的真实口径（t81 / M6 更正）**：预检与记账之间的窗口内，
    /// N 个并发请求可以**同时**通过预检 ⇒ 最多超额 `N × 单次请求上限`，
    /// 而不是之前注释里写的"略微"。这里的 N 由 `CONCURRENCY_LIMIT` 与客户端并发度共同决定，
    /// 所以真正的收敛手段是 [`crate::limiter::AuthRoute::RagReport`] 的速率限制（每账号每分钟），
    /// 配额只是"月度总量"的兜底。
    pub fn commit_quota(&self, month: &str, client: &str, amount: u32) {
        let mut map = self.lock_quota();
        let slot = map
            .entry((month.to_owned(), client.to_owned()))
            .or_insert(QuotaSlot {
                used: 0,
                touched: Instant::now(),
            });
        slot.touched = Instant::now();
        slot.used = slot.used.saturating_add(amount);
    }

    /// 只读：某客户端本月已用量（**count 总和**）。测试与排障用，不改变状态。
    pub fn quota_used(&self, month: &str, client: &str) -> u32 {
        self.lock_quota()
            .get(&(month.to_owned(), client.to_owned()))
            .map(|slot| slot.used)
            .unwrap_or(0)
    }

    /// 桶表有界 + 淘汰（R5-F4）：**复用 limiter 的"跳过用满桶"策略**，
    /// 否则攻击者用几千个不同 XFF 各发一条就能把 victim 已用满的桶挤掉、额度重置。
    fn evict_if_needed(&self, map: &mut HashMap<(String, String), QuotaSlot>, month: &str) {
        const MAX_QUOTA_ENTRIES: usize = 4096;
        if map.len() <= MAX_QUOTA_ENTRIES {
            return;
        }
        // 先清掉非本月：跨月后旧月的计数本来就没有意义
        map.retain(|(slot_month, _), _| slot_month == month);
        while map.len() > MAX_QUOTA_ENTRIES {
            let removed = crate::limiter::evict_oldest_unexhausted(
                map,
                |slot| slot.used >= self.max_items_per_month,
                |slot| slot.touched,
            );
            if !removed {
                break;
            }
        }
    }

    /// 聚合上报：累加 `rag_referrals`，同时把候选写进 `rag_candidates`（status=pending）。
    ///
    /// 返回**实际接受的条数**（去重前后都算"条"，与客户端上报的 items 数一致）。
    pub async fn record(&self, month: &str, items: &[ReferralItem]) -> Result<usize, RagError> {
        let Some(pool) = &self.db else {
            return Err(RagError::NoDatabase);
        };
        let mut tx = pool
            .begin()
            .await
            .map_err(|error| RagError::Db(error.to_string()))?;

        for item in items {
            // R5-F1：聚合值在**库侧**也封顶（`LEAST(...)`），多账号长期刷也不会顶到天文数字；
            // 到顶后只记一条 warn（日志里只有 hash 前 8 位），不再无限增长。
            let stored: i64 = sqlx::query_scalar(
                "INSERT INTO rag_referrals (month, hash, count, first_seen, last_seen) \
                 VALUES ($1, $2, $3, now(), now()) \
                 ON CONFLICT (month, hash) DO UPDATE \
                 SET count = LEAST(rag_referrals.count + EXCLUDED.count, $4), last_seen = now() \
                 RETURNING count",
            )
            .bind(month)
            .bind(&item.hash)
            .bind(i64::from(item.count))
            .bind(REFERRAL_COUNT_CAP)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| RagError::Db(error.to_string()))?;
            if stored >= REFERRAL_COUNT_CAP {
                tracing::warn!(
                    month = %month,
                    hash_prefix = %crate::rag_fetch::hash_prefix(&item.hash),
                    cap = REFERRAL_COUNT_CAP,
                    "rag 聚合计数已达上限，本次增量被截断"
                );
            }

            sqlx::query(
                "INSERT INTO rag_candidates (month, hash, count, status) VALUES ($1, $2, $3, 'pending') \
                 ON CONFLICT (month, hash) DO UPDATE \
                 SET count = LEAST(rag_candidates.count + EXCLUDED.count, $4)",
            )
            .bind(month)
            .bind(&item.hash)
            .bind(i64::from(item.count))
            .bind(REFERRAL_COUNT_CAP)
            .execute(&mut *tx)
            .await
            .map_err(|error| RagError::Db(error.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|error| RagError::Db(error.to_string()))?;
        Ok(items.len())
    }

    fn lock_cache(&self) -> MutexGuard<'_, Option<CachedSalt>> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_memory_salt(&self) -> MutexGuard<'_, Option<(String, String)>> {
        self.memory_salt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_quota(&self) -> MutexGuard<'_, HashMap<(String, String), QuotaSlot>> {
        self.quota
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 解析上报体里的哈希：必须是 64 位**小写**十六进制（与 `hex_sha256` 的输出一致）。
pub fn parse_hash(raw: &str) -> Result<String, RagError> {
    let trimmed = raw.trim();
    if trimmed.len() != 64 {
        return Err(RagError::BadHash);
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(RagError::BadHash);
    }
    Ok(trimmed.to_owned())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn format_rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

/// `Arc<RagService>` 的便捷别名（与 `AppState` 里的字段一致）。
pub type SharedRag = Arc<RagService>;

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u8, day: u8) -> OffsetDateTime {
        time::Date::from_calendar_date(year, time::Month::try_from(month).unwrap(), day)
            .unwrap()
            .midnight()
            .assume_utc()
    }

    #[test]
    fn month_string_and_expiry_roll_over_on_the_first() {
        assert_eq!(RagService::month_of(at(2026, 11, 15)), "2026-11");
        assert_eq!(
            RagService::month_expiry(at(2026, 11, 15)),
            at(2026, 12, 1),
            "失效时刻是下月 1 日 00:00 UTC"
        );
        // 12 月要跨年
        assert_eq!(RagService::month_of(at(2026, 12, 31)), "2026-12");
        assert_eq!(RagService::month_expiry(at(2026, 12, 31)), at(2027, 1, 1));
    }

    #[test]
    fn parse_hash_accepts_only_64_lowercase_hex() {
        let good = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(parse_hash(good).unwrap(), good);
        // 大写、长度不符、非十六进制都拒绝
        assert!(parse_hash(&good.to_uppercase()).is_err());
        assert!(parse_hash(&good[..63]).is_err());
        assert!(parse_hash(&format!("{good}0")).is_err());
        assert!(parse_hash(&"z".repeat(64)).is_err());
    }

    #[test]
    fn quota_is_bounded_per_month_and_client() {
        let config = Config::for_tests();
        let service = RagService::new(&config, None);
        let month = "2026-11";
        let limit = u32::try_from(config.rag_max_items_per_month).expect("测试配置");
        let half = limit / 2;
        // R5-F8：预检**不扣减**——连检两次同样的量都该通过
        assert!(service.check_quota(month, "device-a", half).is_ok());
        assert!(
            service.check_quota(month, "device-a", half).is_ok(),
            "预检不该扣减配额（R5-F8 的前提）"
        );
        // commit 之后正好到上限：仍可通过；再多 1 就拒
        service.commit_quota(month, "device-a", half);
        assert!(service.check_quota(month, "device-a", half).is_ok());
        service.commit_quota(month, "device-a", half);
        assert_eq!(
            service.check_quota(month, "device-a", 1).unwrap_err(),
            RagError::MonthlyQuota { limit },
            "累计到上限后再申请应被拒"
        );
        // 另一个客户端不受影响
        assert!(service.check_quota(month, "device-b", limit).is_ok());
    }

    /// R5-F5 的回归：上月必须是**上一个自然月**。
    ///
    /// 旧实现 `month_expiry(now) - 1 DAY` 得到的是**本月最后一天**（月份仍是本月），
    /// 所以这个用例在旧实现下会红——这正是 security 指出的"测试绿得毫无意义"。
    #[test]
    fn previous_month_is_really_the_previous_month() {
        let at = |year: i32, month: u8, day: u8| {
            time::Date::from_calendar_date(year, time::Month::try_from(month).unwrap(), day)
                .unwrap()
                .midnight()
                .assume_utc()
        };
        assert_eq!(RagService::month_of(at(2026, 11, 15)), "2026-11");
        assert_eq!(
            RagService::previous_month_of(at(2026, 11, 15)),
            "2026-10",
            "11 月的上月是 10 月"
        );
        // 1 月要跨年
        assert_eq!(RagService::previous_month_of(at(2026, 1, 1)), "2025-12");
        assert_eq!(RagService::previous_month_of(at(2026, 1, 31)), "2025-12");
        // 任何时刻的上月都不能等于本月（旧实现恒等于本月，必然违反）
        let now = OffsetDateTime::now_utc();
        assert_ne!(
            RagService::previous_month_of(now),
            RagService::month_of(now),
            "上月必须与本月不同（旧实现恒相等）"
        );
        // check_month 要接受真正的上月
        let config = Config::for_tests();
        let service = RagService::new(&config, None);
        assert!(
            service
                .check_month(&RagService::previous_month_of(now))
                .is_ok(),
            "上月宽限必须生效（12-01 会真实触发）"
        );
    }

    /// R5-F4 的回归：灌几千个新键也**不能**把 victim 已用满的配额桶挤掉。
    #[test]
    fn quota_eviction_does_not_reset_exhausted_buckets() {
        let mut config = Config::for_tests();
        config.rag_max_items_per_month = 100;
        let service = RagService::new(&config, None);
        let month = "2026-11";

        // victim 用满
        assert!(service.check_quota(month, "victim", 100).is_ok());
        service.commit_quota(month, "victim", 100);
        assert!(service.check_quota(month, "victim", 1).is_err());

        // 用远超桶表上限（4096）的不同客户端灌表。
        // 注意必须走 commit_quota：条目是在 commit 时才插入的（check 只读）。
        for index in 0..4200 {
            service.commit_quota(month, &format!("flood-{index}"), 1);
        }

        assert_eq!(
            service.quota_used(month, "victim"),
            100,
            "victim 的已用量必须还在（淘汰跳过用满桶，R5-F4）"
        );
        assert!(
            service.check_quota(month, "victim", 1).is_err(),
            "victim 已用满的桶被淘汰了：攻击者灌新键就能重置配额（R5-F4）"
        );
    }

    #[tokio::test]
    async fn in_memory_salt_is_stable_within_the_month() {
        let config = Config::for_tests();
        let service = RagService::new(&config, None);
        let first = service.salt().await.expect("无数据库模式也应能出盐");
        let second = service.salt().await.expect("第二次走缓存");
        assert_eq!(first.month, second.month);
        assert_eq!(first.salt, second.salt, "同月盐必须稳定");
        assert_eq!(first.salt.len(), 64);
        assert!(first.salt.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(
            first.expires_at.contains('T'),
            "应是 RFC3339：{}",
            first.expires_at
        );
    }

    #[test]
    fn month_check_allows_current_and_previous_only() {
        let config = Config::for_tests();
        let service = RagService::new(&config, None);
        let now = OffsetDateTime::now_utc();
        assert!(service.check_month(&RagService::month_of(now)).is_ok());
        let previous = RagService::month_of(RagService::month_expiry(now) - time::Duration::DAY);
        assert!(service.check_month(&previous).is_ok(), "上月宽限应允许");
        // 未来月份与很久以前的月份都拒绝
        let future = RagService::month_of(RagService::month_expiry(now) + time::Duration::DAY * 40);
        assert_eq!(service.check_month(&future), Err(RagError::BadMonth));
        assert_eq!(service.check_month("2020-01"), Err(RagError::BadMonth));
        assert_eq!(service.check_month("2026-13"), Err(RagError::BadMonth));
    }
}
