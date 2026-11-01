//! 请求体上限（security 复核 F3）。
//!
//! 为什么不用 `DefaultBodyLimit`：它触发的 413 是 axum 自带的 `text/plain`
//! （`Failed to buffer the request body...`），客户端按契约解析时走不到统一错误分支
//! ——"云端错误必须可被程序识别"（ADR-02）在这里断了。
//!
//! 这层中间件做两件事：
//! 1. `Content-Length` 先挡：超限直接 413，连读都不读；
//! 2. 读 body 时带上限：分块传输（无 `Content-Length`）同样在超限处停手。
//!
//! 注意它必须挂在请求 ID 中间件**内侧**，这样 413 的信封里能带上同一个 `request_id`。

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header::CONTENT_LENGTH, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use yunong_shared::{ApiError, ErrorCode};

use crate::api::request_id::ReqId;
use crate::state::AppState;

pub async fn enforce(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let limit = state.config.body_limit_bytes;
    let request_id = request
        .extensions()
        .get::<ReqId>()
        .map(|id| id.0.clone())
        .unwrap_or_else(|| "req_unknown".to_owned());

    if let Some(declared) = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    {
        if declared > limit {
            return too_large(limit, &request_id);
        }
    }

    let (parts, body) = request.into_parts();
    match to_bytes(body, limit).await {
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(_) => too_large(limit, &request_id),
    }
}

/// 413 + 我们的信封（`code` 与其它 413 路径一致：`MESSAGE_TOO_LARGE`）。
fn too_large(limit: usize, request_id: &str) -> Response {
    let body = ApiError::new(
        ErrorCode::MessageTooLarge,
        format!("请求体超过上限 {} KiB", limit / 1024),
    )
    .into_envelope(request_id.to_owned());
    (StatusCode::PAYLOAD_TOO_LARGE, Json(body)).into_response()
}
