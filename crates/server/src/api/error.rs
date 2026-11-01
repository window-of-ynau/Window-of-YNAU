//! 错误 → 响应：统一信封 + 正确状态码（ADR-02 的"可被程序化识别"）。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use yunong_shared::{ApiError, ErrorCode};

use crate::auth::AuthError;

/// HTTP 层错误。`request_id` 由中间件注入，保证与响应头一致。
#[derive(Debug)]
pub struct AppError {
    inner: ApiError,
    request_id: String,
    /// 429 时的 `Retry-After`（秒）。限流器用它告诉客户端退避多久。
    retry_after: Option<u64>,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>, request_id: &str) -> Self {
        Self {
            inner: ApiError::new(code, message),
            request_id: request_id.to_owned(),
            retry_after: None,
        }
    }

    pub fn from_api(err: ApiError, request_id: &str) -> Self {
        Self {
            inner: err,
            request_id: request_id.to_owned(),
            retry_after: None,
        }
    }

    /// 鉴权层的错误 → HTTP 错误信封（错误码与状态码都来自契约层）。
    pub fn from_auth(err: AuthError, request_id: &str) -> Self {
        Self::new(err.code, err.message, request_id)
    }

    /// 给 429 补一个 `Retry-After` 响应头（秒）。
    pub fn set_retry_after(&mut self, seconds: u64) {
        self.retry_after = Some(seconds.max(1));
    }

    /// 把 `request_id` 定稿：限流器在 handler 入口构造错误时还拿不到它。
    pub fn with_request_id(mut self, request_id: &str) -> Self {
        self.request_id = request_id.to_owned();
        self
    }

    pub fn code(&self) -> ErrorCode {
        self.inner.code
    }

    /// 429 的 `Retry-After`（秒）。测试用它断言"按额度算"而不是恒 1s（R2-F7）。
    pub fn retry_after(&self) -> Option<u64> {
        self.retry_after
    }

    pub fn message(&self) -> &str {
        &self.inner.message
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.inner.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let retry_after = self.retry_after;
        let body = self.inner.into_envelope(self.request_id);
        let mut response = (status, Json(body)).into_response();
        if let Some(seconds) = retry_after {
            if let Ok(value) = axum::http::HeaderValue::from_str(&seconds.to_string()) {
                response
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
        }
        response
    }
}

impl From<(ErrorCode, String)> for AppError {
    fn from((code, message): (ErrorCode, String)) -> Self {
        Self {
            inner: ApiError::new(code, message),
            request_id: "req_unknown".into(),
            retry_after: None,
        }
    }
}
