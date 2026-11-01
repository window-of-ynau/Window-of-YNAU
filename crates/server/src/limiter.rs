//! 未鉴权端点的限流（红队预演 H1）。
//!
//! 为什么需要：`/auth/register`、`/auth/login`、`/auth/refresh` 在鉴权层之外，匿名可达；
//! 而密码哈希用 Argon2id（m=19 MiB），**一次请求就是一次 19 MiB 的内存分配**。
//! 没有限制时，匿名并发 N 个登录 ≈ N×19 MiB，既能把进程打 OOM，又能低成本撞库。
//!
//! 两道闸，针对的是两种不同的放大方式：
//!
//! 1. **并发闸**（`hashing` 信号量）：把同时进行的哈希数钉死（默认 8 → 峰值 ≈ 152 MiB）。
//!    按次数限流挡不住"50 个并发同时进哈希"这一瞬间的内存峰值，所以必须有这一道；
//! 2. **速率闸**（令牌桶）：按"路由 + 键"限次数，键取能标识攻击者的最小信息
//!    （登录用规范化后的邮箱，注册/刷新用全局键）。
//!
//! 两者都**不阻塞请求路径**：拿不到名额立刻回 429 + `Retry-After`，让客户端退避。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use yunong_shared::ErrorCode;

use crate::api::error::AppError;

/// 三条未鉴权路由的分类（限流键与阈值都按它取）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthRoute {
    /// 注册：建账号 + 一次哈希
    Register,
    /// 登录：一次校验哈希（最常被撞库）
    Login,
    /// 刷新：只有数据库读写，不哈希
    Refresh,
}

impl AuthRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Login => "login",
            Self::Refresh => "refresh",
        }
    }
}

/// 桶表容量上限（security 复核 F1）。1024 条已经能覆盖"同一时刻被撞库的账号数"，
/// 每条最多 `MAX_KEY_BYTES`，常驻内存上界 ≈ 1024 × (128 + 48) ≈ 176 KiB。
const MAX_BUCKETS: usize = 1024;

/// 单个键的最大字节数。超长键会被摘要成固定长度的十六进制串，
/// **不允许**把攻击者给的长串原样放进 map（那是 F1 的内存放大器）。
const MAX_KEY_BYTES: usize = 128;

/// 把键收敛到 `MAX_KEY_BYTES` 以内。
///
/// 短键原样使用（可读、便于排查）；超长键取 SHA-256 摘要——同一长串仍然映射到同一个桶
/// （限流语义不变），但内存占用固定。
fn bounded_key(key: &str) -> String {
    if key.len() <= MAX_KEY_BYTES {
        return key.to_owned();
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(key.as_bytes());
    format!("sha256:{digest:x}")
}

/// 一个令牌桶。
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

/// 哈希名额：拿到就代表"现在允许做一次 Argon2"，`Drop` 即归还。
#[derive(Debug)]
pub struct HashSlot {
    _permit: OwnedSemaphorePermit,
}

/// 未鉴权端点的限流器。`AppState` 持有一份，所有请求共用。
#[derive(Debug, Clone)]
pub struct AuthLimiter {
    hashing: Arc<Semaphore>,
    buckets: Arc<Mutex<HashMap<(AuthRoute, String), Bucket>>>,
    login_per_min: u32,
    register_per_min: u32,
    refresh_per_min: u32,
}

impl AuthLimiter {
    pub fn new(
        hashing_concurrency: usize,
        login_per_min: u32,
        register_per_min: u32,
        refresh_per_min: u32,
    ) -> Self {
        Self {
            // 至少给 1 个名额，否则登录永远不可用（配置写错时宁可慢，不可死）
            hashing: Arc::new(Semaphore::new(hashing_concurrency.max(1))),
            buckets: Arc::new(Mutex::new(HashMap::new())),
            login_per_min,
            register_per_min,
            refresh_per_min,
        }
    }

    /// 该路由每分钟允许的次数；0 表示"不限次数"（并发闸仍然生效）。
    fn limit_per_min(&self, route: AuthRoute) -> u32 {
        match route {
            AuthRoute::Register => self.register_per_min,
            AuthRoute::Login => self.login_per_min,
            AuthRoute::Refresh => self.refresh_per_min,
        }
    }

    /// 速率闸：超出返回 429（`Retry-After` 按剩余额度估算，至少 1 秒）。
    ///
    /// `key` 取"能标识攻击者的最小信息"：登录用规范化邮箱（同一账号被撞库时能立刻收口），
    /// 注册/刷新用全局键（这两个接口本来就是低频的）。
    pub fn check_rate(&self, route: AuthRoute, key: &str) -> Result<(), AppError> {
        let limit = self.limit_per_min(route);
        if limit == 0 {
            return Ok(()); // 显式关闭：只保留并发闸
        }
        let now = Instant::now();
        let mut buckets = self.lock_buckets();
        let key = bounded_key(key);

        // 桶表上限 + **LRU 淘汰**（security 复核 F1）：
        // 键来自攻击者可控的邮箱，若不清空/不淘汰，匿名请求能把 map 撑到几十 GiB 常驻。
        // 不能用 `clear()`：那等于给攻击者一个"重置全部限流"的按钮。淘汰最旧的一条，
        // 命中率下降只影响被淘汰的那个 key（它立刻又能被限流一次，上限是 1 次）。
        if buckets.len() >= MAX_BUCKETS && !buckets.contains_key(&(route, key.clone())) {
            let oldest = buckets
                .iter()
                .min_by_key(|(_, bucket)| bucket.updated)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                buckets.remove(&oldest);
            }
        }

        let burst = f64::from(limit.max(1));
        let refill_per_sec = burst / 60.0;
        let bucket = buckets.entry((route, key)).or_insert_with(|| Bucket {
            tokens: burst,
            updated: now,
        });

        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(burst);
        bucket.updated = now;

        if bucket.tokens < 1.0 {
            return Err(rate_limited(route, 1));
        }
        bucket.tokens -= 1.0;
        Ok(())
    }

    /// 并发闸：拿一个哈希名额；`wait` 内拿不到就 429。
    ///
    /// 这是**内存**的硬保险：哈希并发数被钉死，攻击者再并发也只能占用这么多内存。
    pub async fn acquire_hash_slot(&self, wait: Duration) -> Result<HashSlot, AppError> {
        let permit = tokio::time::timeout(wait, self.hashing.clone().acquire_owned())
            .await
            .map_err(|_| rate_limited(AuthRoute::Login, 1))?
            .map_err(|_| rate_limited(AuthRoute::Login, 1))?;
        Ok(HashSlot { _permit: permit })
    }

    /// 只读快照：给 `/readyz` 或测试用（当前未对外暴露，先留一个口子）。
    pub fn hashing_permits_available(&self) -> usize {
        self.hashing.available_permits()
    }

    /// 当前桶数量（security 复核 F1 的回归断言用：超长键不得让它增长）。
    pub fn bucket_count(&self) -> usize {
        self.lock_buckets().len()
    }

    fn lock_buckets(&self) -> MutexGuard<'_, HashMap<(AuthRoute, String), Bucket>> {
        // 中毒说明上一个持有者 panic 了：桶数据没有一致性要求，继续用比全局挂掉好
        self.buckets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 429 信封 + `Retry-After`（秒）。
pub fn rate_limited(route: AuthRoute, retry_after_secs: u64) -> AppError {
    let mut error = AppError::new(
        ErrorCode::RateLimited,
        format!(
            "请求过于频繁（{} 接口），请 {} 秒后重试",
            route.as_str(),
            retry_after_secs.max(1)
        ),
        "",
    );
    error.set_retry_after(retry_after_secs.max(1));
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(login_per_min: u32, concurrency: usize) -> AuthLimiter {
        AuthLimiter::new(concurrency, login_per_min, 30, 120)
    }

    #[test]
    fn rate_limit_rejects_after_quota_exhausted() {
        let limiter = limiter(2, 8);
        assert!(limiter.check_rate(AuthRoute::Login, "a@x.cn").is_ok());
        assert!(limiter.check_rate(AuthRoute::Login, "a@x.cn").is_ok());
        let error = limiter
            .check_rate(AuthRoute::Login, "a@x.cn")
            .expect_err("第 3 次必须被拒");
        assert_eq!(error.code(), ErrorCode::RateLimited);
        // 不同键互不影响：换一个邮箱仍然可用
        assert!(limiter.check_rate(AuthRoute::Login, "b@x.cn").is_ok());
    }

    /// security 复核 F1：桶表必须有上界，且淘汰是 **LRU** 而不是 `clear()`。
    ///
    /// `clear()` 会把所有桶一下清空——攻击者只要灌满表，就能顺带重置自己的额度。
    #[test]
    fn rate_limiter_buckets_are_bounded_and_evicted_lru() {
        let limiter = limiter(60, 8);
        for index in 0..(MAX_BUCKETS * 3) {
            let _ = limiter.check_rate(AuthRoute::Login, &format!("user-{index}@x.cn"));
        }
        let buckets = limiter.bucket_count();
        assert!(
            buckets <= MAX_BUCKETS,
            "桶表必须有上界，实际 {buckets} 条（上限 {MAX_BUCKETS}）"
        );
        assert!(
            buckets > MAX_BUCKETS / 2,
            "淘汰应是逐条 LRU，而不是 clear() 清空（实际只剩 {buckets} 条）"
        );
    }

    /// security 复核 F1：超长键被摘要收敛，不会把攻击者的长串原样常驻；
    /// 同一长键仍然映射到同一桶（限流语义不变）。
    #[test]
    fn rate_limiter_hashes_oversized_keys() {
        let limiter = limiter(2, 8); // 2 次/分钟
        let long_key = "x".repeat(4096);
        assert!(limiter.check_rate(AuthRoute::Login, &long_key).is_ok());
        assert!(limiter.check_rate(AuthRoute::Login, &long_key).is_ok());
        // 第 3 次超额：说明前两次确实落在**同一个**桶（摘要稳定）
        assert!(limiter.check_rate(AuthRoute::Login, &long_key).is_err());
        assert_eq!(limiter.bucket_count(), 1, "长键只应占一条桶");
    }

    #[test]
    fn rate_limit_zero_means_unlimited() {
        let limiter = limiter(2, 8);
        let unlimited = AuthLimiter::new(8, 0, 0, 0);
        assert!(unlimited.check_rate(AuthRoute::Login, "a@x.cn").is_ok());
        for _ in 0..100 {
            assert!(unlimited.check_rate(AuthRoute::Login, "a@x.cn").is_ok());
        }
        // 对照：有额度时第 3 次就拒
        assert!(limiter.check_rate(AuthRoute::Login, "a@x.cn").is_ok());
        assert!(limiter.check_rate(AuthRoute::Login, "a@x.cn").is_ok());
        assert!(limiter.check_rate(AuthRoute::Login, "a@x.cn").is_err());
    }

    #[tokio::test]
    async fn hash_slots_are_bounded_by_concurrency() {
        // 并发上限 1：拿住第一个名额后，第二个必须立刻被拒（内存放大器被钉死）
        let limiter = limiter(60, 1);
        let first = limiter
            .acquire_hash_slot(Duration::from_millis(1))
            .await
            .expect("第一个名额应拿到");
        let second = limiter.acquire_hash_slot(Duration::from_millis(1)).await;
        assert!(second.is_err(), "并发闸满时必须拒绝而不是排队堆积内存");
        drop(first);
        // 归还之后又能拿到
        assert!(limiter
            .acquire_hash_slot(Duration::from_millis(50))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn zero_concurrency_still_yields_one_permit() {
        // 配置写 0 也不能把登录打死：构造时兜底到 1
        let limiter = limiter(60, 0);
        assert_eq!(limiter.hashing_permits_available(), 1);
        assert!(limiter
            .acquire_hash_slot(Duration::from_millis(50))
            .await
            .is_ok());
    }
}
