//! 鉴权数据的两个后端。
//!
//! 刻意用**枚举**而不是 trait：`async fn` 在 trait 里要么引入 `async-trait`
//! 依赖，要么写 boxed future，都超出 W2 的范围。两个分支各自是同步/异步的
//! 普通函数，编译期就能把两条路径都检查一遍。
//!
//! - [`AuthBackend::Pg`]：生产路径（PostgreSQL，见 `auth::pg`）；
//! - [`AuthBackend::Memory`]：无数据库模式（本地骨架 + 离线集成测试），
//!   进程重启即丢，**仅用于测试与演示**。

use std::collections::HashMap;
use std::sync::Mutex;

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::{
    error::AuthError, token, AuthSession, Principal, ACCESS_TOKEN_TTL_SECS, REFRESH_TOKEN_TTL_SECS,
};

/// 鉴权数据后端。由 [`crate::auth::AuthService`] 持有。
pub enum AuthBackend {
    /// 进程内内存态：无数据库时自动启用，保证测试与本地骨架离线可跑。
    Memory(Mutex<MemoryState>),
    /// 生产后端。
    Pg(PgPool),
}

impl AuthBackend {
    /// `db` 为 `None`（没配 `DATABASE_URL` 或连接失败降级）时落到内存后端。
    pub fn from_pool(db: Option<PgPool>) -> Self {
        match db {
            Some(pool) => Self::Pg(pool),
            None => Self::Memory(Mutex::new(MemoryState::default())),
        }
    }

    /// 后端种类，启动日志里打一行，免得"为什么重启后登录态全没了"。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Memory(_) => "memory",
            Self::Pg(_) => "postgres",
        }
    }
}

/// 内存态。只存**摘要**与哈希，不存任何明文令牌或密码。
#[derive(Debug, Default)]
pub struct MemoryState {
    users: HashMap<Uuid, MemoryUser>,
    /// 小写邮箱 → 用户 ID（对应 `ux_users_email_lower`）
    emails: HashMap<String, Uuid>,
    /// 令牌摘要 → 访问令牌
    access_tokens: HashMap<String, MemoryAccessToken>,
    /// 令牌摘要 → 刷新令牌
    refresh_tokens: HashMap<String, MemoryRefreshToken>,
}

#[derive(Debug)]
struct MemoryUser {
    /// Argon2id 的 PHC 字符串
    password_hash: String,
}

#[derive(Debug)]
struct MemoryAccessToken {
    user_id: Uuid,
    device_id: Option<Uuid>,
    expires_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
}

#[derive(Debug)]
struct MemoryRefreshToken {
    user_id: Uuid,
    device_id: Option<Uuid>,
    expires_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
}

impl MemoryState {
    /// 注册：邮箱（已规范化、小写）重复 → 422。
    pub(crate) fn register(
        &mut self,
        email: String,
        password_hash: String,
    ) -> Result<AuthSession, AuthError> {
        if self.emails.contains_key(&email) {
            return Err(AuthError::validation("该邮箱已注册"));
        }
        let user_id = Uuid::new_v4();
        self.users.insert(user_id, MemoryUser { password_hash });
        self.emails.insert(email, user_id);
        Ok(self.issue_session(user_id, None))
    }

    /// 按邮箱取用户与 PHC 哈希（登录用）。
    pub(crate) fn password_hash(&self, email: &str) -> Option<(Uuid, String)> {
        let user_id = *self.emails.get(email)?;
        let user = self.users.get(&user_id)?;
        Some((user_id, user.password_hash.clone()))
    }

    /// 签发一对新令牌（访问 + 刷新），库里只落摘要。
    pub(crate) fn issue_session(&mut self, user_id: Uuid, device_id: Option<Uuid>) -> AuthSession {
        let now = OffsetDateTime::now_utc();
        let access_token = token::generate();
        let refresh_token = token::generate();

        self.access_tokens.insert(
            token::hash(&access_token),
            MemoryAccessToken {
                user_id,
                device_id,
                expires_at: now + Duration::seconds(ACCESS_TOKEN_TTL_SECS),
                revoked_at: None,
            },
        );
        self.refresh_tokens.insert(
            token::hash(&refresh_token),
            MemoryRefreshToken {
                user_id,
                device_id,
                expires_at: now + Duration::seconds(REFRESH_TOKEN_TTL_SECS),
                revoked_at: None,
            },
        );

        AuthSession {
            user_id,
            access_token,
            refresh_token,
            expires_in: ACCESS_TOKEN_TTL_SECS,
        }
    }

    /// 校验访问令牌：不存在/已吊销 → 401；存在但过期 → `TOKEN_EXPIRED`。
    pub(crate) fn authenticate(&self, access_token: &str) -> Result<Principal, AuthError> {
        let token_hash = token::hash(access_token);
        let Some(record) = self.access_tokens.get(&token_hash) else {
            return Err(AuthError::unauthorized_with("访问令牌无效"));
        };
        if record.revoked_at.is_some() {
            return Err(AuthError::unauthorized_with("访问令牌已吊销"));
        }
        if record.expires_at <= OffsetDateTime::now_utc() {
            return Err(AuthError::token_expired());
        }
        Ok(Principal {
            user_id: record.user_id,
            device_id: record.device_id,
            token_hash,
        })
    }

    /// 轮换刷新令牌：成功即把旧的那一枚标记为已吊销。
    pub(crate) fn rotate_refresh(&mut self, refresh_token: &str) -> Result<AuthSession, AuthError> {
        let token_hash = token::hash(refresh_token);
        let now = OffsetDateTime::now_utc();

        let (user_id, device_id) = {
            let Some(record) = self.refresh_tokens.get(&token_hash) else {
                return Err(AuthError::unauthorized_with("刷新令牌无效"));
            };
            if record.revoked_at.is_some() {
                return Err(AuthError::unauthorized_with("刷新令牌已失效，请重新登录"));
            }
            if record.expires_at <= now {
                return Err(AuthError::unauthorized_with("刷新令牌已过期，请重新登录"));
            }
            (record.user_id, record.device_id)
        };

        if let Some(record) = self.refresh_tokens.get_mut(&token_hash) {
            record.revoked_at = Some(now);
        }
        Ok(self.issue_session(user_id, device_id))
    }

    /// 登出：吊销当前访问令牌，并把这个账号所有未失效的刷新令牌一并吊销。
    pub(crate) fn revoke(&mut self, principal: &Principal) {
        let now = OffsetDateTime::now_utc();
        if let Some(record) = self.access_tokens.get_mut(&principal.token_hash) {
            record.revoked_at = Some(now);
        }
        for record in self.refresh_tokens.values_mut() {
            if record.user_id == principal.user_id && record.revoked_at.is_none() {
                record.revoked_at = Some(now);
            }
        }
    }

    /// 测试专用：把某个访问令牌的过期时间拉到过去（避免真等 15 分钟）。
    #[cfg(test)]
    pub(crate) fn expire_access_token(&mut self, access_token: &str) {
        let token_hash = token::hash(access_token);
        if let Some(record) = self.access_tokens.get_mut(&token_hash) {
            record.expires_at = OffsetDateTime::now_utc() - Duration::seconds(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yunong_shared::ErrorCode;

    const FAKE_PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

    fn state_with_user(email: &str) -> (MemoryState, AuthSession) {
        let mut state = MemoryState::default();
        let session = state
            .register(email.to_owned(), FAKE_PHC.to_owned())
            .expect("首次注册应成功");
        (state, session)
    }

    #[test]
    fn duplicate_email_is_rejected() {
        let (mut state, _) = state_with_user("student@ynau.edu.cn");
        let err = state
            .register("student@ynau.edu.cn".into(), FAKE_PHC.into())
            .expect_err("重复邮箱必须拒绝");
        assert_eq!(err.code, ErrorCode::ValidationError);
    }

    #[test]
    fn tokens_are_stored_as_sha256_digests_only() {
        let (state, session) = state_with_user("student@ynau.edu.cn");
        let access_hash = token::hash(&session.access_token);
        assert!(state.access_tokens.contains_key(&access_hash));
        assert!(
            !state.access_tokens.contains_key(&session.access_token),
            "明文访问令牌绝不能出现在存储里"
        );
        assert!(!state.refresh_tokens.contains_key(&session.refresh_token));
        assert_eq!(state.access_tokens[&access_hash].user_id, session.user_id);
    }

    #[test]
    fn expired_access_token_maps_to_token_expired() {
        let (mut state, session) = state_with_user("student@ynau.edu.cn");
        state.expire_access_token(&session.access_token);
        let err = state
            .authenticate(&session.access_token)
            .expect_err("过期令牌必须拒绝");
        assert_eq!(err.code, ErrorCode::TokenExpired);
    }

    #[test]
    fn unknown_and_revoked_access_tokens_are_unauthorized() {
        let (mut state, session) = state_with_user("student@ynau.edu.cn");
        let principal = state
            .authenticate(&session.access_token)
            .expect("新令牌应可用");
        assert_eq!(principal.user_id, session.user_id);
        assert_eq!(principal.device_id, None);

        state.revoke(&principal);
        let err = state
            .authenticate(&session.access_token)
            .expect_err("登出后必须失效");
        assert_eq!(err.code, ErrorCode::Unauthorized);

        let err = state
            .authenticate(&token::generate())
            .expect_err("不存在的令牌必须拒绝");
        assert_eq!(err.code, ErrorCode::Unauthorized);
    }

    #[test]
    fn refresh_rotates_and_old_refresh_token_dies() {
        let (mut state, session) = state_with_user("student@ynau.edu.cn");

        let rotated = state
            .rotate_refresh(&session.refresh_token)
            .expect("首次刷新应成功");
        assert_ne!(rotated.refresh_token, session.refresh_token);
        assert_ne!(rotated.access_token, session.access_token);
        assert_eq!(rotated.user_id, session.user_id);

        let err = state
            .rotate_refresh(&session.refresh_token)
            .expect_err("旧刷新令牌必须作废");
        assert_eq!(err.code, ErrorCode::Unauthorized);
        assert!(state.rotate_refresh(&rotated.refresh_token).is_ok());
    }

    #[test]
    fn logout_also_revokes_refresh_tokens() {
        let (mut state, session) = state_with_user("student@ynau.edu.cn");
        let principal = state
            .authenticate(&session.access_token)
            .expect("新令牌应可用");
        state.revoke(&principal);
        assert!(
            state.rotate_refresh(&session.refresh_token).is_err(),
            "登出后刷新令牌不应还能换新"
        );
    }

    #[test]
    fn password_hash_lookup_uses_normalized_email() {
        let (state, session) = state_with_user("student@ynau.edu.cn");
        let (user_id, phc) = state
            .password_hash("student@ynau.edu.cn")
            .expect("应能查到用户");
        assert_eq!(user_id, session.user_id);
        assert_eq!(phc, FAKE_PHC);
        assert!(state.password_hash("nobody@ynau.edu.cn").is_none());
    }
}
