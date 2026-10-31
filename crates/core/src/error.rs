//! 核心错误类型。

use thiserror::Error;

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
}
