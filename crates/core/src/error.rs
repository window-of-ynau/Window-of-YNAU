//! 核心错误类型。

use std::time::Duration;

use thiserror::Error;
use yunong_shared::ErrorCode;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("存储不可用：{0}")]
    Storage(String),
    #[error("密钥不存在：{0}")]
    SecretMissing(&'static str),
    #[error("参数非法：{0}")]
    Invalid(String),
    #[error("未实现：{0}")]
    NotImplemented(String),
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
    /// 远端（上游厂商或我们的服务端）明确返回了失败。
    ///
    /// `code` 用契约层的 [`ErrorCode`]，401/402/429 这些语义在传输层不会被压扁；
    /// `message` **已截断到 200 字**（上游错误体可能回显 prompt，不能整段进日志/界面）。
    #[error("远端返回 {code}：{message}")]
    Remote {
        code: ErrorCode,
        message: String,
        /// `Retry-After` 提示（秒）。调用方自行决定是否退避重试
        retry_after: Option<Duration>,
    },
    /// 本地计时器判死（建连 / 首事件 / 事件间空闲，见 `gateway::Timeouts`）
    #[error("超时：{0}")]
    Timeout(String),
    /// 传输层失败（连接重置、TLS、协议错误、DNS……）
    #[error("网络错误：{0}")]
    Network(String),
    /// 调用方主动取消（流式回调返回 `Flow::Stop`，或 future 被 drop）
    #[error("已取消：{0}")]
    Cancelled(String),
}

impl CoreError {
    /// 服务端建议的重试等待（只有 [`CoreError::Remote`] 带这个信息）。
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Remote { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// 可以安全重试吗？（超时、限流、上游 5xx 可以；参数/鉴权错误重试也没用）
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Timeout(_) | Self::Network(_) => true,
            Self::Remote { code, .. } => matches!(
                code,
                ErrorCode::RateLimited
                    | ErrorCode::ConcurrencyLimit
                    | ErrorCode::UpstreamTimeout
                    | ErrorCode::UpstreamError
                    | ErrorCode::Internal
            ),
            _ => false,
        }
    }
}

pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render_chinese_messages() {
        let e = CoreError::SecretMissing("ApiKey");
        assert!(e.to_string().contains("ApiKey"));
        let e = CoreError::NotImplemented("BYOK".into());
        assert!(e.to_string().contains("未实现"));
    }

    #[test]
    fn remote_renders_code_and_retry_hint_separately() {
        let e = CoreError::Remote {
            code: ErrorCode::RateLimited,
            message: "HTTP 429：slow down".into(),
            retry_after: Some(Duration::from_secs(3)),
        };
        let rendered = e.to_string();
        assert!(rendered.contains("RATE_LIMITED"), "{rendered}");
        assert!(rendered.contains("slow down"), "{rendered}");
        assert_eq!(e.retry_after(), Some(Duration::from_secs(3)));
        assert!(e.is_retryable());
    }

    #[test]
    fn retryability_matches_semantics() {
        assert!(CoreError::Timeout("x".into()).is_retryable());
        assert!(CoreError::Network("x".into()).is_retryable());
        assert!(!CoreError::Invalid("x".into()).is_retryable());
        assert!(!CoreError::Remote {
            code: ErrorCode::Unauthorized,
            message: "bad key".into(),
            retry_after: None,
        }
        .is_retryable());
        assert!(!CoreError::Cancelled("user".into()).is_retryable());
    }
}
