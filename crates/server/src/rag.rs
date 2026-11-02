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
    max_items_per_month: usize,
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
    used: usize,
    touched: Instant,
}

/// 上报被拒的原因（映射到错误码与审计原因）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RagError {
    /// 月份不合法（不是当前月，也不是上月宽限期）
    BadMonth,
    /// 单次条数超限
    TooManyItems { limit: usize },
    /// 单月累计超限
    MonthlyQuota { limit: usize },
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

impl RagService {
    pub fn new(config: &Config, db: Option<PgPool>) -> Self {
        Self {
            db,
            memory_salt: Mutex::new(None),
            cache: Mutex::new(None),
            ttl: Duration::from_secs(config.rag_salt_ttl_secs),
            max_items_per_request: config.rag_max_items_per_request,
            max_items_per_month: config.rag_max_items_per_month,
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
        let previous = Self::month_of(Self::month_expiry(now) - time::Duration::DAY);
        if month == previous {
            return Ok(());
        }
        Err(RagError::BadMonth)
    }

    /// 配额闸（进程内，按 `(month, 客户端标识)` 累计）。
    ///
    /// **为什么进程内**：ADR-19 要求上报体里没有 user_id，服务端也不该为了限额
    /// 去建立"账号 → 上报量"的持久化关联。进程内计数既能挡住单机放大，
    /// 又不留可回溯的痕迹；代价是重启清零、多实例不共享（1.0 单实例，可接受）。
    pub fn check_quota(&self, month: &str, client: &str, items: usize) -> Result<(), RagError> {
        if items > self.max_items_per_request {
            return Err(RagError::TooManyItems {
                limit: self.max_items_per_request,
            });
        }
        let mut map = self.lock_quota();
        if map.len() > 4096 {
            // 桶表有界：先清掉非本月的条目，再不行就按最久未用淘汰（参考 H1 的教训）
            map.retain(|(slot_month, _), _| slot_month == month);
            while map.len() > 4096 {
                let Some(victim) = map
                    .iter()
                    .min_by_key(|(_, slot)| slot.touched)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                map.remove(&victim);
            }
        }
        let slot = map
            .entry((month.to_owned(), client.to_owned()))
            .or_insert(QuotaSlot {
                used: 0,
                touched: Instant::now(),
            });
        slot.touched = Instant::now();
        if slot.used + items > self.max_items_per_month {
            return Err(RagError::MonthlyQuota {
                limit: self.max_items_per_month,
            });
        }
        slot.used += items;
        Ok(())
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
            sqlx::query(
                "INSERT INTO rag_referrals (month, hash, count, first_seen, last_seen) \
                 VALUES ($1, $2, $3, now(), now()) \
                 ON CONFLICT (month, hash) DO UPDATE \
                 SET count = rag_referrals.count + EXCLUDED.count, last_seen = now()",
            )
            .bind(month)
            .bind(&item.hash)
            .bind(i64::from(item.count))
            .execute(&mut *tx)
            .await
            .map_err(|error| RagError::Db(error.to_string()))?;

            sqlx::query(
                "INSERT INTO rag_candidates (month, hash, count, status) VALUES ($1, $2, $3, 'pending') \
                 ON CONFLICT (month, hash) DO UPDATE \
                 SET count = rag_candidates.count + EXCLUDED.count",
            )
            .bind(month)
            .bind(&item.hash)
            .bind(i64::from(item.count))
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
        let limit = config.rag_max_items_per_month;
        let per_request = config.rag_max_items_per_request;
        // 单次有上限，所以分批发完整个月的额度
        let mut left = limit;
        while left > 0 {
            let batch = left.min(per_request);
            assert!(
                service.check_quota(month, "device-a", batch).is_ok(),
                "第 {batch} 批应通过"
            );
            left -= batch;
        }
        assert_eq!(
            service.check_quota(month, "device-a", 1).unwrap_err(),
            RagError::MonthlyQuota { limit }
        );
        // 另一个客户端不受影响（单次仍受 per-request 上限约束）
        assert!(service.check_quota(month, "device-b", per_request).is_ok());
        // 单次超限单独报错
        assert_eq!(
            service
                .check_quota(month, "device-c", config.rag_max_items_per_request + 1)
                .unwrap_err(),
            RagError::TooManyItems {
                limit: config.rag_max_items_per_request
            }
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
