//! 各协议的端点与鉴权头（纯函数，便于单测）。
//!
//! 三家在"怎么带 Key"上不统一：OpenAI 系走 `Authorization: Bearer`，
//! Anthropic 走 `x-api-key` + 必填的 `anthropic-version` 头。这里收敛成一个函数，
//! 免得两个通道各写一遍、将来某家改了只改一处。

use yunong_shared::Protocol;

/// Anthropic 的 API 版本头（官方要求必填）。
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// 端点路径。`base_url` 允许带尾斜杠，也允许已经带了 `/v1`（不会重复拼）。
pub(crate) fn endpoint(base_url: &str, protocol: Protocol) -> String {
    let base = base_url.trim_end_matches('/');
    let path = match protocol {
        Protocol::OpenaiCompletions => "/v1/chat/completions",
        Protocol::OpenaiResponses => "/v1/responses",
        Protocol::AnthropicMessages => "/v1/messages",
    };
    format!("{base}{path}")
}

/// 鉴权头。
pub(crate) fn auth_headers(protocol: Protocol, api_key: &str) -> Vec<(&'static str, String)> {
    match protocol {
        Protocol::OpenaiCompletions | Protocol::OpenaiResponses => {
            vec![("authorization", format!("Bearer {api_key}"))]
        }
        Protocol::AnthropicMessages => vec![
            ("x-api-key", api_key.to_owned()),
            ("anthropic-version", ANTHROPIC_VERSION.to_owned()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_per_protocol() {
        let base = "https://api.example.com";
        assert_eq!(
            endpoint(base, Protocol::OpenaiCompletions),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint(base, Protocol::OpenaiResponses),
            "https://api.example.com/v1/responses"
        );
        assert_eq!(
            endpoint(base, Protocol::AnthropicMessages),
            "https://api.example.com/v1/messages"
        );
    }

    #[test]
    fn trailing_slash_does_not_double() {
        assert_eq!(
            endpoint("https://api.example.com/", Protocol::OpenaiResponses),
            "https://api.example.com/v1/responses"
        );
    }

    #[test]
    fn openai_uses_bearer_and_anthropic_uses_api_key() {
        let openai = auth_headers(Protocol::OpenaiCompletions, "sk-1");
        assert_eq!(openai, vec![("authorization", "Bearer sk-1".to_owned())]);
        // Responses 与 Completions 同一套鉴权
        assert_eq!(
            auth_headers(Protocol::OpenaiResponses, "sk-1"),
            vec![("authorization", "Bearer sk-1".to_owned())]
        );

        let anthropic = auth_headers(Protocol::AnthropicMessages, "sk-ant");
        assert_eq!(
            anthropic,
            vec![
                ("x-api-key", "sk-ant".to_owned()),
                ("anthropic-version", ANTHROPIC_VERSION.to_owned()),
            ]
        );
        // Anthropic 不能同时带 Bearer（否则上游可能拿错凭据）
        assert!(!anthropic.iter().any(|(name, _)| *name == "authorization"));
    }
}
