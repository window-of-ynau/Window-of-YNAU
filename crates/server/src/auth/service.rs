//! 鉴权服务：参数校验、后端选择、令牌签发与吊销。
//!
//! 上层（HTTP handler / 中间件）只跟这里打交道，不碰 SQL，也不碰内存态。

use std::sync::{Mutex, MutexGuard};

use sqlx::PgPool;
use uuid::Uuid;

use time::{Duration, OffsetDateTime};

use crate::auth::{
    error::AuthError,
    password, pg,
    store::{AuthBackend, MemoryState},
    token, AuthSession, Principal,
};

/// 邀请码最长字节数：超过直接判无效（省一次哈希与查库）。
pub const MAX_INVITE_BYTES: usize = 128;

/// 邀请码注册的入参。
///
/// 明文码与客户端指纹都在**这一层**就地哈希：明文不进后端，日志里也不打明文。
pub struct InviteUse<'a> {
    pub code: &'a str,
    pub ip: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

/// 账号鉴权。`AppState` 持有 `Arc<AuthService>`，所有请求共用同一份状态。
pub struct AuthService {
    backend: AuthBackend,
}

impl AuthService {
    /// `db` 为 `None` 时自动落到内存后端：无数据库模式（与离线测试）也能跑。
    pub fn new(db: Option<PgPool>) -> Self {
        Self {
            backend: AuthBackend::from_pool(db),
        }
    }

    /// 当前后端种类。"memory" 意味着重启即丢数据，启动日志里要能看见。
    pub fn backend_kind(&self) -> &'static str {
        self.backend.kind()
    }

    /// 注册：邮箱已存在或密码太短 → 422；成功 → 201 的令牌对。
    pub async fn register(
        &self,
        email: &str,
        raw_password: &str,
    ) -> Result<AuthSession, AuthError> {
        let email =
            normalize_email(email).ok_or_else(|| AuthError::validation("邮箱格式不正确"))?;
        password::validate_new(raw_password)?;
        // Argon2 是 CPU/内存密集的**同步**计算：直接在 async worker 上跑会把 runtime 线程
        // 占住（实测并发登录时 `/healthz` 能被拖到 632ms，探针可能超时）。
        let password_hash = hash_blocking(raw_password.to_owned()).await?;

        match &self.backend {
            AuthBackend::Memory(state) => lock(state).register(email, password_hash),
            AuthBackend::Pg(pool) => pg::register(pool, &email, &password_hash).await,
        }
    }

    /// 邀请制注册（t141 / ADR-11 1.0）：邮箱 + 密码 + **邀请码**。
    ///
    /// 校验顺序（贵的那步放最后）：格式 → 密码策略 → 邀请码形态 → **Argon2 哈希** →
    /// 「原子核销 + 建号 + 核销记录 + 发令牌」一个事务。
    ///
    /// 为什么把 Argon2 放在核销之前：核销要占着行锁，而 Argon2 是 19 MiB / ~50ms 的同步计算，
    /// 放事务里会把行锁与连接一起拖住。代价是"错码也会付一次哈希成本"——注册端点本来就有
    /// 每 IP 10 次/分 的限流，这个放大量可以接受。
    pub async fn register_with_invite(
        &self,
        email: &str,
        raw_password: &str,
        invite: InviteUse<'_>,
    ) -> Result<AuthSession, AuthError> {
        let email =
            normalize_email(email).ok_or_else(|| AuthError::validation("邮箱格式不正确"))?;
        password::validate_new(raw_password)?;

        let code = invite.code.trim();
        if code.is_empty() || code.len() > MAX_INVITE_BYTES {
            // 形态不对与"不存在"用同一个错误：不给枚举留信号
            return Err(AuthError::invite_invalid());
        }

        let password_hash = hash_blocking(raw_password.to_owned()).await?;
        let code_hash = token::hash(code);
        let ip_hash = invite.ip.map(token::hash);
        let ua_hash = invite.user_agent.map(token::hash);

        match &self.backend {
            AuthBackend::Memory(state) => lock(state).register_with_invite(
                email,
                password_hash,
                &code_hash,
                ip_hash.as_deref(),
                ua_hash.as_deref(),
            ),
            AuthBackend::Pg(pool) => {
                pg::register_with_invite(
                    pool,
                    &email,
                    &password_hash,
                    pg::InviteUse {
                        code_hash: &code_hash,
                        ip_hash: ip_hash.as_deref(),
                        ua_hash: ua_hash.as_deref(),
                    },
                )
                .await
            }
        }
    }

    /// 发放一个邀请码（**只落哈希**；调用方负责把明文交给发放对象）。
    ///
    /// 只支持数据库后端：没有库的时候"邀请码"没有任何意义（重启即丢），
    /// 内存后端直接报错，而不是假装成功。
    pub async fn insert_invite(
        &self,
        code: &str,
        batch: &str,
        issued_by: &str,
        expires_at: Option<OffsetDateTime>,
        max_uses: i32,
        note: Option<&str>,
    ) -> Result<(), AuthError> {
        let code = code.trim();
        if code.is_empty() || code.len() > MAX_INVITE_BYTES {
            return Err(AuthError::validation("邀请码长度不合法"));
        }
        if batch.trim().is_empty() || issued_by.trim().is_empty() {
            return Err(AuthError::validation("batch / issued_by 不能为空"));
        }
        match &self.backend {
            AuthBackend::Pg(pool) => {
                pg::insert_invite(
                    pool,
                    &token::hash(code),
                    batch,
                    issued_by,
                    expires_at,
                    max_uses.max(1),
                    note,
                )
                .await
            }
            AuthBackend::Memory(_) => Err(AuthError::internal("邀请码管理需要数据库后端")),
        }
    }

    /// **仅内存后端**：登记一个邀请码（测试与本地调试用）。
    ///
    /// 生产请用 [`Self::insert_invite`]（落库）；这个函数在 PG 后端上是空操作，
    /// 存在只是为了 API 级集成测试能在无数据库模式下走完整注册流程。
    pub fn add_memory_invite_for_tests(
        &self,
        code: &str,
        batch: &str,
        max_uses: u32,
        expires_in: Option<std::time::Duration>,
    ) {
        if let AuthBackend::Memory(state) = &self.backend {
            let expires_at = expires_in
                .map(|ttl| OffsetDateTime::now_utc() + Duration::seconds(ttl.as_secs() as i64));
            lock(state).add_invite(code, batch, max_uses, expires_at);
        }
    }

    /// 按批次撤销。
    pub async fn revoke_invite_batch(&self, batch: &str) -> Result<u64, AuthError> {
        match &self.backend {
            AuthBackend::Pg(pool) => pg::revoke_invite_batch(pool, batch).await,
            AuthBackend::Memory(_) => Err(AuthError::internal("邀请码管理需要数据库后端")),
        }
    }

    /// 某批次用量：`(已用, 总容量, 张数)`。
    pub async fn invite_batch_stats(&self, batch: &str) -> Result<(i64, i64, i64), AuthError> {
        match &self.backend {
            AuthBackend::Pg(pool) => pg::invite_batch_stats(pool, batch).await,
            AuthBackend::Memory(_) => Err(AuthError::internal("邀请码管理需要数据库后端")),
        }
    }

    /// 登录：凭据错误 → 401，且不区分"邮箱不存在"与"密码错"。
    pub async fn login(&self, email: &str, raw_password: &str) -> Result<AuthSession, AuthError> {
        // 邮箱格式非法也走同一个错误：不给账号枚举留缝隙。
        let email = normalize_email(email).ok_or_else(credentials_error)?;

        match &self.backend {
            AuthBackend::Memory(state) => {
                // 注意：**不要**把锁跨过 await 持有（MutexGuard 不是 Send），
                // 所以先取出哈希串、释放锁，再把 Argon2 交给阻塞线程池。
                let found = lock(state).password_hash(&email);
                let Some((user_id, password_hash)) = found else {
                    // 账号不存在也付一次哈希成本，让两条路径耗时接近。
                    let _ = hash_blocking(raw_password.to_owned()).await;
                    return Err(credentials_error());
                };
                if !verify_blocking(raw_password.to_owned(), password_hash).await {
                    return Err(credentials_error());
                }
                Ok(lock(state).issue_session(user_id, None))
            }
            AuthBackend::Pg(pool) => {
                let found = pg::password_hash(pool, &email).await?;
                let Some((user_id, stored)) = found else {
                    let _ = hash_blocking(raw_password.to_owned()).await;
                    return Err(credentials_error());
                };
                let matched = match stored {
                    Some(phc) => verify_blocking(raw_password.to_owned(), phc).await,
                    None => false, // 存在但没有密码（第三方账号）：按凭据错误处理
                };
                if !matched {
                    return Err(credentials_error());
                }
                pg::issue_session(pool, user_id, None).await
            }
        }
    }

    /// 校验 Bearer 访问令牌：格式错/不存在/已吊销 → 401；存在但过期 → `TOKEN_EXPIRED`。
    pub async fn authenticate(&self, access_token: &str) -> Result<Principal, AuthError> {
        if !token::is_well_formed(access_token) {
            return Err(AuthError::unauthorized_with("访问令牌格式不正确"));
        }
        match &self.backend {
            AuthBackend::Memory(state) => lock(state).authenticate(access_token),
            AuthBackend::Pg(pool) => pg::authenticate(pool, access_token).await,
        }
    }

    /// 刷新：返回新的一对令牌，**旧刷新令牌立即作废**。
    pub async fn refresh(&self, refresh_token: &str) -> Result<AuthSession, AuthError> {
        if !token::is_well_formed(refresh_token) {
            return Err(AuthError::unauthorized_with("刷新令牌格式不正确"));
        }
        match &self.backend {
            AuthBackend::Memory(state) => lock(state).rotate_refresh(refresh_token),
            AuthBackend::Pg(pool) => pg::refresh(pool, refresh_token).await,
        }
    }

    /// 停用一个账号（t134 / H1）：标记 + **立刻吊销全部令牌**。
    ///
    /// 返回 `Ok(true)` 表示这次真的把账号从"可用"改成"停用"；`Ok(false)` = 本来就已停用。
    ///
    /// 名字里的"停用"对应库里的 `users.disabled_at`（ADR-11 的毕业降级/封禁共用同一个开关）。
    /// 生产上直接写 SQL 也能达到同样效果：迁移 0008 的触发器会在 `disabled_at` 变非空时
    /// 无条件吊销令牌，所以**不存在"改了库但令牌还能用"的缝隙**。
    pub async fn disable_user(&self, user_id: Uuid) -> Result<bool, AuthError> {
        match &self.backend {
            AuthBackend::Memory(state) => Ok(lock(state).disable_user(user_id)),
            AuthBackend::Pg(pool) => pg::disable_user(pool, user_id).await,
        }
    }

    /// 登出：吊销当前访问令牌，刷新令牌一并吊销（不止当前设备）。
    pub async fn logout(&self, principal: &Principal) -> Result<(), AuthError> {
        match &self.backend {
            AuthBackend::Memory(state) => {
                lock(state).revoke(principal);
                Ok(())
            }
            AuthBackend::Pg(pool) => pg::logout(pool, principal).await,
        }
    }
}

/// 把 Argon2 哈希放到阻塞线程池（不占 async worker，探针不再被拖慢）。
async fn hash_blocking(password: String) -> Result<String, AuthError> {
    tokio::task::spawn_blocking(move || password::hash(&password))
        .await
        .map_err(|error| AuthError::internal(format!("密码哈希任务失败：{error}")))?
}

/// 把 Argon2 校验放到阻塞线程池；任务本身 panic 时按"不匹配"处理（失败即拒绝）。
async fn verify_blocking(password: String, phc: String) -> bool {
    tokio::task::spawn_blocking(move || password::verify(&password, &phc))
        .await
        .unwrap_or(false)
}

/// 登录失败的唯一文案。
fn credentials_error() -> AuthError {
    AuthError::unauthorized_with("邮箱或密码不正确")
}

/// 取内存态锁。中毒只说明上一个持有者 panic 了；内存里没有需要维持的外键，
/// 继续用比把服务全局搞挂更合适。
fn lock(state: &Mutex<MemoryState>) -> MutexGuard<'_, MemoryState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 邮箱规范化：去首尾空白、**剥离 `+tag`**、转小写，并做最低限度的格式检查。
///
/// 小写化与 `users (lower(email))` 唯一索引一致；格式检查只挡明显错误
/// （登录时不区分，注册时返回 422）。
///
/// # 为什么要剥 `+tag`（security t134 / H3）
///
/// `a+1@x.com`、`a+2@x.com` … 在 RFC 里是**同一个收件箱**的不同别名。不剥的话：
/// 1. 一个收件箱可以注册无数账号（2FA 验证码全进同一个邮箱 ⇒ 2FA 等于不存在）；
/// 2. 封禁/限流可以靠换 tag 绕过（账号键不同）。
///
/// 所以账号键统一取"本地部分去掉 `+` 之后"的那一版；**只做规范化，不因此拒绝地址**
/// （`a+tag@x.com` 仍是合法输入，注册后按 `a@x.com` 记账）。
/// 只剥 `+` 不剥点号：`a.b@x.com` 与 `ab@x.com` 在不同邮件系统里**不**等价，剥了会误合并。
fn normalize_email(raw: &str) -> Option<String> {
    let email = raw.trim();
    // R2-F9：账号键用 ASCII 小写（与 `users (lower(email))` 索引一致）。
    // 之前这里用 Unicode `to_lowercase`、限流键用 `to_ascii_lowercase`，两套口径会对不上。
    if email.is_empty() || email.len() > 254 || !email.is_ascii() {
        return None;
    }
    if email.chars().any(char::is_whitespace) {
        return None;
    }
    let (local, domain) = email.split_once('@')?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return None;
    }
    // H3：剥掉 `+tag`。剥完为空（形如 `+tag@x.com`）没有可用的账号键 → 视为格式不对。
    let local = local.split('+').next().unwrap_or_default();
    if local.is_empty() {
        return None;
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return None;
    }
    if domain.split('.').any(str::is_empty) {
        return None;
    }
    Some(format!("{local}@{domain}").to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// t134 / H3：`+tag` 必须被剥掉——否则一个收件箱能注册无数账号（2FA 等于不存在）。
    #[test]
    fn plus_tag_is_stripped_from_local_part() {
        assert_eq!(
            normalize_email("a+1@x.com").as_deref(),
            Some("a@x.com"),
            "+tag 必须剥掉"
        );
        assert_eq!(
            normalize_email("A+anything@X.COM").as_deref(),
            Some("a@x.com"),
            "剥 +tag 与小写化要一起生效"
        );
        // 同一收件箱的不同别名 → 同一个账号键
        assert_eq!(normalize_email("a+1@x.com"), normalize_email("A@x.com"));
        // 多个 `+` 一律从第一个开始剥
        assert_eq!(normalize_email("a+b+c@x.com").as_deref(), Some("a@x.com"));
        // 合法地址不因为带 tag 被拒
        assert!(normalize_email("student+tag@ynau.edu.cn").is_some());
        // 剥完没有本地部分 → 格式不对（畸形输入，不是"合法地址被拒"）
        assert_eq!(normalize_email("+tag@x.com"), None);
        // 域名里的 `+` 不动（只在本地部分剥）
        assert_eq!(
            normalize_email("a+b@x+y.com").as_deref(),
            Some("a@x+y.com"),
            "只削本地部分的 tag"
        );
        // 点号不合并（`a.b@x.com` 与 `ab@x.com` 在多数邮件系统里并不等价）
        assert_eq!(normalize_email("a.b@x.com").as_deref(), Some("a.b@x.com"));
    }

    /// t141：空邀请码在**进入 Argon2 之前**就被拒（不给"错码烧 CPU"留口子）。
    #[tokio::test]
    async fn blank_invite_code_is_rejected_before_hashing() {
        let service = AuthService::new(None);
        let error = service
            .register_with_invite(
                "student@ynau.edu.cn",
                "correct-horse-battery",
                InviteUse {
                    code: "   ",
                    ip: Some("1.2.3.4"),
                    user_agent: None,
                },
            )
            .await
            .expect_err("空邀请码必须被拒");
        assert_eq!(error.code, yunong_shared::ErrorCode::InviteInvalid);
        assert_eq!(error.message, "邀请码无效或已不可用");
    }

    #[test]
    fn email_is_normalized_to_lowercase() {
        assert_eq!(
            normalize_email("  Student@YNAU.edu.cn "),
            Some("student@ynau.edu.cn".to_owned())
        );
    }

    #[test]
    fn invalid_emails_are_rejected() {
        for bad in [
            "",
            "no-at-sign",
            "@ynau.edu.cn",
            "student@",
            "student@ynau",
            "a@b@c.com",
            "a b@ynau.edu.cn",
            "student@.edu.cn",
            "student@ynau..cn",
        ] {
            assert!(normalize_email(bad).is_none(), "不应通过：{bad:?}");
        }
    }
}
