//! 错误 → 响应：统一信封 + 正确状态码（ADR-02 的"可被程序化识别"）。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use yunong_shared::{ApiError, ErrorCode};

/// HTTP 层错误。`request_id` 由中间件注入，保证与响应头一致。
#[derive(Debug)]
pub struct AppError {
    inner: ApiError,
    request_id: String,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>, request_id: &str) -> Self {
        Self {
            inner: ApiError::new(code, message),
            request_id: request_id.to_owned(),
        }
    }

    pub fn from_api(err: ApiError, request_id: &str) -> Self {
        Self {
            inner: err,
            request_id: request_id.to_owned(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        self.inner.code
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.inner.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = self.inner.into_envelope(self.request_id);
        (status, Json(body)).into_response()
    }
}

impl From<(ErrorCode, String)> for AppError {
    fn from((code, message): (ErrorCode, String)) -> Self {
        Self {
            inner: ApiError::new(code, message),
            request_id: "req_unknown".into(),
        }
    }
}
