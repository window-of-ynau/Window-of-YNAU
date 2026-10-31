//! 鉴权（契约已锁定，实现由 `agent/auth` 在 W2 补齐）。
//!
//! 设计要点（与 `docs/04-ADR.md` ADR-11 一致）：
//! - 密码只存 **Argon2id** 哈希，永不可逆；
//! - **不存学校密码**，也不代登录（本模块与校务凭据无关）；
//! - 令牌是**不透明串**（随机 UUID，DB 里只存 SHA-256），可吊销、可轮换；
//!   刻意不用 JWT：JWT 的吊销需要额外的黑名单，而我们要的正是"登出立刻失效"。
//!
//! 端点契约：
//! - `POST /api/v1/auth/register` → 201 `{ user_id, access_token, expires_in, refresh_token }`
//! - `POST /api/v1/auth/login`    → 200 同上
//! - `POST /api/v1/auth/refresh`  → 200 轮换 refresh
//! - `POST /api/v1/auth/logout`   → 200 `{ ok: true }`
//! - 其余 `/api/v1/**` 一律要求 `Authorization: Bearer <access_token>`，缺失/失效 → 401 信封

/// `Authorization` 头的前缀。
pub const BEARER_PREFIX: &str = "Bearer ";

/// 访问令牌有效期（秒）。
pub const ACCESS_TOKEN_TTL_SECS: i64 = 15 * 60;

/// 刷新令牌有效期（秒）。
pub const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 60 * 60;
