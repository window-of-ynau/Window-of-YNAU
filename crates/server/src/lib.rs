//! 云农之窗 · 服务端（控制面 + 过路面）
//!
//! 边界（`docs/04-ADR.md` ADR-02）：
//! - 转发内容，但**不落库**；
//! - 不持有学校凭据，也不持有用户 Key；
//! - 只存账号、授权、知识、抽样用量、审计。

#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
pub mod config;
pub mod state;

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

/// 组装路由。`main` 与集成测试共用同一个入口，保证测的是同一套东西。
pub fn app(state: state::AppState) -> Router {
    // 公开：探针 + 注册/登录/刷新（最后这三个正是"还没有令牌"时要用的接口）。
    let public = Router::new()
        .route("/healthz", get(api::health::healthz))
        .route("/readyz", get(api::health::readyz))
        .route("/api/v1/auth/register", post(api::auth::register))
        .route("/api/v1/auth/login", post(api::auth::login))
        .route("/api/v1/auth/refresh", post(api::auth::refresh));

    // 需要 Bearer：登出 + 其余全部业务接口。
    // 用 `route_layer`（不是 `layer`）：它只包路由、不包 fallback，
    // 未知路径才能继续是 404，而不是被鉴权改写成 401。
    let protected = Router::new()
        .route("/api/v1/auth/logout", post(api::auth::logout))
        .route("/api/v1/me", get(api::me::me))
        .route("/api/v1/chat/stream", post(api::chat::chat_stream))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api::auth_guard::require_bearer,
        ));

    public
        .merge(protected)
        // 最外层：请求 ID（错误信封需要它）；内层：访问日志
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(api::request_id::attach))
        .with_state(state)
}

/// 服务版本。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
