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
    Router::new()
        .route("/healthz", get(api::health::healthz))
        .route("/readyz", get(api::health::readyz))
        .route("/api/v1/chat/stream", post(api::chat::chat_stream))
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
