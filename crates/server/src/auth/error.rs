//! 鉴权层的内部错误。
//!
//! 这里**故意不带 `request_id`**：ID 是 HTTP 层的事，由 `api::error::AppError`
//! 从请求扩展里取，保证错误信封与响应头始终一致。

use yunong_shared::ErrorCode;

/// 鉴权失败。
///
/// 只有四种语义（对应契约里的错误码）：
/// - [`AuthError::validation`] → `VALIDATION_ERROR`（422）
/// - [`AuthError::unauthorized_with`] → `UNAUTHORIZED`（401）
/// - [`AuthError::token_expired`] → `TOKEN_EXPIRED`（401，客户端应走 refresh）
/// - [`AuthError::internal`] → `INTERNAL`（500）
#[derive(Debug, Clone)]
pub struct AuthError {
    pub code: ErrorCode,
    pub message: String,
}

impl AuthError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 注册参数不合法（邮箱格式、密码长度、邮箱已存在）。
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ValidationError, message)
    }

    /// 令牌不存在 / 已吊销 / 格式错，或凭据不匹配。
    pub fn unauthorized_with(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    /// 访问令牌**存在但已过期**：客户端应当用 refresh_token 换新，而不是重新登录。
    pub fn token_expired() -> Self {
        Self::new(
            ErrorCode::TokenExpired,
            "访问令牌已过期，请用 refresh_token 换新",
        )
    }

    /// 邀请码不可用（不存在 / 已撤销 / 已过期 / 用量已满）。
    ///
    /// **四类原因共用一个错误码与一句文案**（t141）：邀请码是 8~32 位的低熵字符串，
    /// 一旦按原因区分（"已过期" vs "不存在"），就成了可枚举的预言机。
    /// 用 403 而不是 422：邀请码是**授权凭据**，不是参数格式问题；客户端据此展示
    /// "邀请码无效，请联系管理员"。
    ///
    /// t147：错误码从 `FORBIDDEN` 改成了专用的 **`INVITE_INVALID`**（HTTP 仍是 403）。
    /// 这样客户端能把"邀请码错误"和其它 403 区分开，给出针对性提示；
    /// 四类原因**仍然**共用一个码 + 一句文案，枚举风险不变。
    pub fn invite_invalid() -> Self {
        Self::new(ErrorCode::InviteInvalid, "邀请码无效或已不可用")
    }

    /// 内部故障（数据库、哈希等）。不把细节暴露给客户端之外的日志。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AuthError {}
