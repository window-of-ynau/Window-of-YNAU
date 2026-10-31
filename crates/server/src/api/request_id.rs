//! 请求 ID：错误信封、日志、响应头三处必须一致。

use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};
use uuid::Uuid;

pub const HEADER: &str = "x-request-id";

/// 请求作用域内的 ID（handler 用 `Extension<ReqId>` 取）。
#[derive(Clone, Debug)]
pub struct ReqId(pub String);

impl ReqId {
    pub fn new() -> Self {
        Self(format!("req_{}", Uuid::new_v4().simple()))
    }
}

impl Default for ReqId {
    fn default() -> Self {
        Self::new()
    }
}

/// 中间件：优先复用调用方传入的 `x-request-id`，否则生成；并回写到响应头。
pub async fn attach(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .map(str::to_owned)
        .unwrap_or_else(|| ReqId::new().0);

    req.extensions_mut().insert(ReqId(id.clone()));
    let mut res = next.run(req).await;
    match HeaderValue::from_str(&id) {
        Ok(v) => {
            res.headers_mut().insert(HEADER, v);
        }
        Err(_) => {
            tracing::warn!(%id, "请求 ID 非法，未能写入响应头");
        }
    }
    res
}
