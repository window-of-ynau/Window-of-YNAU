//! 客户端 IP 提取（R2-F6：注册按 IP 分桶）。
//!
//! 顺序：`ConnectInfo`（真实对端地址）→ 若开启信任代理再看 `X-Forwarded-For` 第一段 → `unknown`。
//!
//! **默认不信任 XFF**：没有反向代理时它是完全由攻击者填的，按它分桶等于没有限流。
//! 部署在 Caddy/Nginx 后面时设 `YUNONG_TRUST_PROXY=1`。

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Request, State},
    http::header::HeaderName,
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

/// 提取出来的客户端标识（放进请求 extension，handler 按需取）。
#[derive(Debug, Clone)]
pub struct ClientIp(pub String);

static X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// 中间件：把客户端 IP 放进 extension。拿不到时写 `unknown`（所有匿名请求共用一个桶，
/// 比"每请求一个桶"更安全——后者等于没有限流，还会撑桶表）。
pub async fn inject(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip().to_string());
    let forwarded = if state.config.trust_proxy {
        request
            .headers()
            .get(&X_FORWARDED_FOR)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && value.len() <= 64)
    } else {
        None
    };
    let ip = forwarded.or(peer).unwrap_or_else(|| "unknown".to_owned());
    request.extensions_mut().insert(ClientIp(ip));
    next.run(request).await
}
