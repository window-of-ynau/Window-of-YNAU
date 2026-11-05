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
///
/// `Memory` 比 `Pg` 大得多（内存态里有四张表），但整个进程只构造**一个**后端实例，
/// 这点体积差异换不来任何收益；为了它把 `MemoryState` 装箱只会给每次取锁加一层间接。
#[allow(clippy::large_enum_variant)]
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
    /// 邀请码哈希 → 邀请码（t141；与 PG 的 `invite_codes` 同语义）
    invite_codes: HashMap<String, MemoryInvite>,
    /// 核销记录（(user_id, code_hash)）：内存后端只留最小追溯信息
    redemptions: Vec<(Uuid, String)>,
}

/// 内存态邀请码。**只存哈希**（键）与用量，不存明文。
#[derive(Debug)]
struct MemoryInvite {
    #[allow(dead_code)]
    batch: String,
    max_uses: u32,
    used_count: u32,
    expires_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
}

#[derive(Debug)]
struct MemoryUser {
    /// Argon2id 的 PHC 字符串
    password_hash: String,
    /// 停用时间（对应 PG 的 `users.disabled_at`）。非空 = 该账号一律不可用。
    disabled_at: Option<OffsetDateTime>,
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
        self.users.insert(
            user_id,
            MemoryUser {
                password_hash,
                disabled_at: None,
            },
        );
        self.emails.insert(email, user_id);
        Ok(self.issue_session(user_id, None))
    }

    /// 登记一个邀请码（测试与本地调试用；键是**明文码的哈希**）。
    pub(crate) fn add_invite(
        &mut self,
        code: &str,
        batch: &str,
        max_uses: u32,
        expires_at: Option<OffsetDateTime>,
    ) {
        self.invite_codes.insert(
            token::hash(code),
            MemoryInvite {
                batch: batch.to_owned(),
                max_uses: max_uses.max(1),
                used_count: 0,
                expires_at,
                revoked_at: None,
            },
        );
    }

    /// 按批次撤销（与 PG 的 `UPDATE ... SET revoked_at` 同语义）。**仅供测试**（生产走 PG）。
    #[allow(dead_code)]
    pub(crate) fn revoke_invites_by_batch(&mut self, batch: &str) -> usize {
        let now = OffsetDateTime::now_utc();
        let mut revoked = 0;
        for invite in self.invite_codes.values_mut() {
            if invite.batch == batch && invite.revoked_at.is_none() {
                invite.revoked_at = Some(now);
                revoked += 1;
            }
        }
        revoked
    }

    /// 带邀请码注册：检查 + 自增 + 建号 + 记账，全在一把锁里（内存后端的"事务"）。
    pub(crate) fn register_with_invite(
        &mut self,
        email: String,
        password_hash: String,
        code_hash: &str,
        ip_hash: Option<&str>,
        ua_hash: Option<&str>,
    ) -> Result<AuthSession, AuthError> {
        let _ = (ip_hash, ua_hash); // 内存后端不落指纹，仅为与 PG 签名对齐
        if self.emails.contains_key(&email) {
            return Err(AuthError::validation("该邮箱已注册"));
        }
        let now = OffsetDateTime::now_utc();
        let Some(invite) = self.invite_codes.get(code_hash) else {
            return Err(AuthError::invite_invalid());
        };
        if invite.revoked_at.is_some()
            || invite
                .expires_at
                .is_some_and(|expires_at| expires_at <= now)
            || invite.used_count >= invite.max_uses
        {
            return Err(AuthError::invite_invalid());
        }
        if let Some(invite) = self.invite_codes.get_mut(code_hash) {
            invite.used_count += 1;
        }

        let user_id = Uuid::new_v4();
        self.users.insert(
            user_id,
            MemoryUser {
                password_hash,
                disabled_at: None,
            },
        );
        self.emails.insert(email, user_id);
        self.redemptions.push((user_id, code_hash.to_owned()));
        Ok(self.issue_session(user_id, None))
    }

    /// 某批次的（已用, 上限, 张数）——**仅供测试**（生产走 `pg::invite_batch_stats`）。
    #[allow(dead_code)]
    pub(crate) fn invite_batch_stats(&self, batch: &str) -> (u32, u32, usize) {
        let mut used = 0;
        let mut capacity = 0;
        let mut count = 0;
        for invite in self
            .invite_codes
            .values()
            .filter(|item| item.batch == batch)
        {
            used += invite.used_count;
            capacity += invite.max_uses;
            count += 1;
        }
        (used, capacity, count)
    }

    /// 按邮箱取用户与 PHC 哈希（登录用）。
    ///
    /// 停用账号按"不存在"处理（与 PG 的 `disabled_at IS NULL` 过滤同语义）：
    /// 登录路径不能因为"这个账号存在但被停用"而泄露任何信息。
    pub(crate) fn password_hash(&self, email: &str) -> Option<(Uuid, String)> {
        let user_id = *self.emails.get(email)?;
        let user = self.users.get(&user_id)?;
        if user.disabled_at.is_some() {
            return None;
        }
        Some((user_id, user.password_hash.clone()))
    }

    /// 停用一个账号：标记 + **立刻吊销它的所有令牌**（H1）。
    pub(crate) fn disable_user(&mut self, user_id: Uuid) -> bool {
        let now = OffsetDateTime::now_utc();
        let Some(user) = self.users.get_mut(&user_id) else {
            return false;
        };
        if user.disabled_at.is_some() {
            return false;
        }
        user.disabled_at = Some(now);
        for record in self.access_tokens.values_mut() {
            if record.user_id == user_id && record.revoked_at.is_none() {
                record.revoked_at = Some(now);
            }
        }
        for record in self.refresh_tokens.values_mut() {
            if record.user_id == user_id && record.revoked_at.is_none() {
                record.revoked_at = Some(now);
            }
        }
        true
    }

    /// 该用户是否已被停用（测试与诊断用）。
    fn is_disabled(&self, user_id: Uuid) -> bool {
        self.users
            .get(&user_id)
            .is_some_and(|user| user.disabled_at.is_some())
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
        // H1：禁用账号即使令牌还没被吊销（理论上不该发生）也不放行
        if self.is_disabled(record.user_id) {
            return Err(AuthError::unauthorized_with("账号已停用"));
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
            // H1：禁用账号不能靠 refresh 续命
            if self.is_disabled(record.user_id) {
                return Err(AuthError::unauthorized_with("账号已停用"));
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

    /// t134 / H1：禁用账号后，access / refresh / 登录全部失效（内存后端与 PG 同语义）。
    #[test]
    fn disabled_user_cannot_authenticate_or_refresh() {
        let (mut state, session) = state_with_user("student@ynau.edu.cn");
        assert!(state.authenticate(&session.access_token).is_ok());

        assert!(state.disable_user(session.user_id), "第一次停用应真的改动");
        assert!(!state.disable_user(session.user_id), "重复停用应返回 false");

        // 1) 访问令牌立刻不可用（禁用即吊销）
        let error = state
            .authenticate(&session.access_token)
            .expect_err("停用后访问令牌必须失效");
        assert_eq!(error.code, ErrorCode::Unauthorized);
        // 2) 刷新令牌也不能续命
        let error = state
            .rotate_refresh(&session.refresh_token)
            .expect_err("停用后刷新令牌必须失效");
        assert_eq!(error.code, ErrorCode::Unauthorized);
        // 3) 登录也查不到（按"不存在"处理，不泄露账号状态）
        assert!(state.password_hash("student@ynau.edu.cn").is_none());
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

// ============ t141：邀请码（内存后端） ============

#[cfg(test)]
mod invite_tests {
    use super::*;
    use yunong_shared::ErrorCode;

    const FAKE_PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

    fn redeem(state: &mut MemoryState, email: &str, code: &str) -> Result<AuthSession, AuthError> {
        let code_hash = token::hash(code);
        state.register_with_invite(
            email.to_owned(),
            FAKE_PHC.to_owned(),
            &code_hash,
            Some("ip-hash"),
            Some("ua-hash"),
        )
    }

    /// 一次性码：第二次核销必须失败，且错误与"码不存在"完全一致（不泄露原因）。
    #[test]
    fn invite_is_single_use_and_failure_is_indistinguishable() {
        let mut state = MemoryState::default();
        state.add_invite("ynau-once", "b1", 1, None);

        assert!(redeem(&mut state, "a@x.com", "ynau-once").is_ok());
        let used_again = redeem(&mut state, "b@x.com", "ynau-once").expect_err("第二次必须失败");
        let unknown = redeem(&mut state, "c@x.com", "ynau-nope").expect_err("不存在的码必须失败");

        assert_eq!(used_again.code, ErrorCode::Forbidden);
        assert_eq!(used_again.message, unknown.message, "失败原因不能区分");
        // 已用 1 / 容量 1 / 1 张
        assert_eq!(state.invite_batch_stats("b1"), (1, 1, 1));
    }

    /// 按批次撤销后不能再核销；撤销计数只算"本次真的改动的"。
    #[test]
    fn revoked_batch_cannot_be_redeemed() {
        let mut state = MemoryState::default();
        state.add_invite("ynau-a", "b1", 1, None);
        state.add_invite("ynau-b", "b1", 1, None);
        state.add_invite("ynau-c", "b2", 1, None);

        assert_eq!(state.revoke_invites_by_batch("b1"), 2);
        assert_eq!(
            state.revoke_invites_by_batch("b1"),
            0,
            "重复撤销不该重复计数"
        );

        assert!(redeem(&mut state, "a@x.com", "ynau-a").is_err());
        assert!(redeem(&mut state, "b@x.com", "ynau-b").is_err());
        assert!(
            redeem(&mut state, "c@x.com", "ynau-c").is_ok(),
            "别的批次不受影响"
        );
    }

    /// 过期即失效；一码多用按 max_uses 放行。
    #[test]
    fn expiry_and_multi_use() {
        let mut state = MemoryState::default();
        state.add_invite(
            "ynau-expired",
            "b1",
            5,
            Some(OffsetDateTime::now_utc() - Duration::seconds(1)),
        );
        assert!(redeem(&mut state, "a@x.com", "ynau-expired").is_err());

        state.add_invite("ynau-multi", "b2", 2, None);
        assert!(redeem(&mut state, "a@x.com", "ynau-multi").is_ok());
        assert!(redeem(&mut state, "b@x.com", "ynau-multi").is_ok());
        assert!(
            redeem(&mut state, "c@x.com", "ynau-multi").is_err(),
            "超过 max_uses"
        );
    }

    /// 邮箱冲突不消耗邀请码（与 PG 的事务回滚同语义）。
    #[test]
    fn email_conflict_does_not_consume_invite() {
        let mut state = MemoryState::default();
        state.add_invite("ynau-1", "b1", 1, None);
        assert!(redeem(&mut state, "a@x.com", "ynau-1").is_ok());

        state.add_invite("ynau-2", "b1", 1, None);
        let error = redeem(&mut state, "a@x.com", "ynau-2").expect_err("重复邮箱必须拒绝");
        assert_eq!(error.code, ErrorCode::ValidationError);
        assert_eq!(
            state.invite_batch_stats("b1"),
            (1, 2, 2),
            "第二张码不能被白吃"
        );
    }
}
