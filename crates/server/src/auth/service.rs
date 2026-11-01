//! 鉴权服务：参数校验、后端选择、令牌签发与吊销。
//!
//! 上层（HTTP handler / 中间件）只跟这里打交道，不碰 SQL，也不碰内存态。

use std::sync::{Mutex, MutexGuard};

use sqlx::PgPool;

use crate::auth::{
    error::AuthError,
    password, pg,
    store::{AuthBackend, MemoryState},
    token, AuthSession, Principal,
};

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

/// 邮箱规范化：去首尾空白、转小写，并做最低限度的格式检查。
///
/// 小写化与 `users (lower(email))` 唯一索引一致；格式检查只挡明显错误
/// （登录时不区分，注册时返回 422）。
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
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return None;
    }
    if domain.split('.').any(str::is_empty) {
        return None;
    }
    Some(email.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

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
