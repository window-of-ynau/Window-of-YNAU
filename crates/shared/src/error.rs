//! 错误码与错误信封：全系统唯一的一套。

use serde::{Deserialize, Serialize};

/// 全局错误码。与 `docs/03-接口文档.md` §1.3 一一对应。
///
/// 注意：序列化是 `SCREAMING_SNAKE_CASE`，客户端按字符串分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// 无 token / token 不可解析
    Unauthorized,
    /// 已过期，可用 refresh 换新
    TokenExpired,
    /// 越权
    Forbidden,
    /// 资源不存在
    NotFound,
    /// 参数非法（含超长 message）
    ValidationError,
    /// 超过单轮输入上限
    MessageTooLarge,
    /// 同一 turn 已有在途请求
    ConcurrencyLimit,
    /// 频率超限
    RateLimited,
    /// 套餐到期 / 无授权
    QuotaExceeded,
    /// Auto Review 拒绝
    ReviewRejected,
    /// 上游超时（可重试一次）
    UpstreamTimeout,
    /// 上游错误（可重试）
    UpstreamError,
    /// 尚未实现（骨架阶段的占位）
    NotImplemented,
    /// 未捕获异常
    Internal,
}

impl ErrorCode {
    /// 稳定的字符串形式（日志、指标标签、客户端分支都用它）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "UNAUTHORIZED",
            Self::TokenExpired => "TOKEN_EXPIRED",
            Self::Forbidden => "FORBIDDEN",
            Self::NotFound => "NOT_FOUND",
            Self::ValidationError => "VALIDATION_ERROR",
            Self::MessageTooLarge => "MESSAGE_TOO_LARGE",
            Self::ConcurrencyLimit => "CONCURRENCY_LIMIT",
            Self::RateLimited => "RATE_LIMITED",
            Self::QuotaExceeded => "QUOTA_EXCEEDED",
            Self::ReviewRejected => "REVIEW_REJECTED",
            Self::UpstreamTimeout => "UPSTREAM_TIMEOUT",
            Self::UpstreamError => "UPSTREAM_ERROR",
            Self::NotImplemented => "NOT_IMPLEMENTED",
            Self::Internal => "INTERNAL",
        }
    }

    /// 对应的 HTTP 状态码。
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Unauthorized | Self::TokenExpired => 401,
            Self::Forbidden | Self::ReviewRejected => 403,
            Self::NotFound => 404,
            Self::ValidationError => 422,
            Self::MessageTooLarge => 413,
            Self::ConcurrencyLimit | Self::RateLimited => 429,
            Self::QuotaExceeded => 402,
            Self::UpstreamTimeout => 504,
            Self::UpstreamError => 502,
            Self::NotImplemented => 501,
            Self::Internal => 500,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 错误信封里的 `error` 字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// 统一失败响应：`{ "ok": false, "error": { ... } }`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub ok: bool,
    pub error: ErrorBody,
}

/// 业务错误：可以被 `IntoResponse` 直接转成响应。
#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ValidationError, message)
    }

    pub fn not_implemented(what: impl std::fmt::Display) -> Self {
        Self::new(
            ErrorCode::NotImplemented,
            format!("{what} 尚未实现（v0.1 骨架阶段）"),
        )
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// 转成对外信封。`request_id` 由中间件注入。
    pub fn into_envelope(self, request_id: impl Into<String>) -> ErrorEnvelope {
        ErrorEnvelope {
            ok: false,
            error: ErrorBody {
                code: self.code,
                message: self.message,
                request_id: request_id.into(),
                details: self.details,
            },
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_upper_snake() {
        let s = serde_json::to_string(&ErrorCode::QuotaExceeded).unwrap();
        assert_eq!(s, "\"QUOTA_EXCEEDED\"");
    }

    #[test]
    fn status_mapping_is_stable() {
        assert_eq!(ErrorCode::Unauthorized.http_status(), 401);
        assert_eq!(ErrorCode::QuotaExceeded.http_status(), 402);
        assert_eq!(ErrorCode::MessageTooLarge.http_status(), 413);
        assert_eq!(ErrorCode::ConcurrencyLimit.http_status(), 429);
        assert_eq!(ErrorCode::UpstreamTimeout.http_status(), 504);
    }

    #[test]
    fn envelope_shape_matches_contract() {
        let env = ApiError::validation("bad").into_envelope("req_1");
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR");
        assert_eq!(v["error"]["request_id"], "req_1");
        // details 为空时必须省略
        assert!(v["error"].get("details").is_none());
    }
}
