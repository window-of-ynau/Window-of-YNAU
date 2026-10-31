//! 鉴权（W2：Argon2id 密码 + 不透明令牌 + 端点与保护范围）。
//!
//! 设计要点（与 `docs/04-ADR.md` ADR-11 一致）：
//! - 密码只存 **Argon2id** 哈希，永不可逆；
//! - **不存学校密码**，也不代登录（本模块与校务凭据无关）；
//! - 令牌是**不透明串**（两段 UUID v4，64 个 hex 字符），DB 里只存 SHA-256 摘要，
//!   可吊销、可轮换；刻意不用 JWT：JWT 的吊销需要额外的黑名单，
//!   而我们要的正是"登出立刻失效"。
//!
//! 端到端流程：
//! - 注册/登录 → 访问令牌（15 分钟）+ 刷新令牌（30 天）；
//! - 刷新 → 轮换：发新的一对，旧刷新令牌立即作废；
//! - 登出 → 当前访问令牌与刷新令牌一起吊销；
//! - 其余 `/api/v1/**` 由 `api::auth_guard` 统一要求 `Authorization: Bearer <access_token>`。
//!
//! 存储有两个后端（见 [`AuthBackend`]）：生产用 PostgreSQL，
//! 无数据库模式与离线集成测试用进程内内存态。

mod error;
mod password;
mod pg;
mod service;
mod store;
mod token;

pub use error::AuthError;
pub use service::AuthService;
pub use store::{AuthBackend, MemoryState};

use uuid::Uuid;

/// `Authorization` 头的前缀。
pub const BEARER_PREFIX: &str = "Bearer ";

/// 访问令牌有效期（秒）。
pub const ACCESS_TOKEN_TTL_SECS: i64 = 15 * 60;

/// 刷新令牌有效期（秒）。
pub const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// 一次注册/登录/刷新返回的令牌对。明文字符串只在响应里出现这一次。
#[derive(Debug, Clone)]
pub struct AuthSession {
    pub user_id: Uuid,
    pub access_token: String,
    pub refresh_token: String,
    /// 访问令牌有效期（秒）；客户端据此安排刷新，不必猜。
    pub expires_in: i64,
}

/// 已通过 Bearer 鉴权的调用者，由中间件放进请求扩展。
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: Uuid,
    /// 登录时若绑定了设备则非空；W2 的注册/登录还没有设备维度，故通常为 `None`。
    pub device_id: Option<Uuid>,
    /// 当前访问令牌的摘要：登出时按它精确吊销，避免再哈希一次。
    pub token_hash: String,
}
