//! 各协议的端点与鉴权头（纯函数，便于单测）。
//!
//! 三家在"怎么带 Key"上不统一：OpenAI 系走 `Authorization: Bearer`，
//! Anthropic 走 `x-api-key` + 必填的 `anthropic-version` 头。这里收敛成一个函数，
//! 免得两个通道各写一遍、将来某家改了只改一处。

use yunong_shared::Protocol;

/// Anthropic 的 API 版本头（官方要求必填）。
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// 端点路径。`base_url` 允许带尾斜杠，也允许已经带了 `/v1`（不会重复拼）。
///
/// 第三种情况是**真实踩过的坑**：用户在设置页把厂商文档里的
/// `https://api.deepseek.com/v1` 原样粘进来，若这里再拼 `/v1/chat/completions`，
/// 请求会打到 `/v1/v1/chat/completions` → 上游 404。所以先把结尾的 `/v1`
/// （以及 `/v1/`）去掉再拼；只认**整个路径段的** `/v1`，`/v1beta` 之类不动。
pub(crate) fn endpoint(base_url: &str, protocol: Protocol) -> String {
    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    let host = crate::protocol_pick::provider_host(base);

    // **各家的路径不一样，不能一律 `/v1/xxx`**（t118）。
    //
    // 依据（owner 提供的官方文档，原文已核对）：
    // - DeepSeek：Responses 用 `base_url = https://api.deepseek.com`（SDK 自己拼 `/responses`）；
    //   Anthropic 兼容层的 `base_url` 是 `https://api.deepseek.com/anthropic`
    //   → 端点 `/anthropic/v1/messages`（**不是** `/v1/messages`）；
    //   Completions 仍是 OpenAI 兼容的 `/chat/completions`（`/v1/chat/completions` 也是兼容别名）。
    // - Anthropic 官方：`/v1/messages`。
    // - OpenAI 官方：`/v1/responses`、`/v1/chat/completions`。
    // - 其它（自建/中转）：保持原来的"base + 标准 `/v1` 后缀"，不猜。
    //
    // 说明：本轮**没能**用真实 Key 实测（`api.deepseek.com` 在当前网络策略下探不通，
    // curl/Invoke-WebRequest 都是 000）；不写死"某路径一定可用"的断言，按文档拼接，
    // 端到端验证交给 lead-client 用真 Key 跑一次。
    let is_deepseek = host.ends_with("deepseek.com");
    let is_anthropic = host.ends_with("anthropic.com");

    let path = match (is_deepseek, is_anthropic, protocol) {
        (true, _, Protocol::OpenaiCompletions) => "/chat/completions",
        (true, _, Protocol::OpenaiResponses) => "/responses",
        (true, _, Protocol::AnthropicMessages) => "/anthropic/v1/messages",
        (_, true, Protocol::AnthropicMessages) => "/v1/messages",
        (_, _, Protocol::OpenaiCompletions) => "/v1/chat/completions",
        (_, _, Protocol::OpenaiResponses) => "/v1/responses",
        (_, _, Protocol::AnthropicMessages) => "/v1/messages",
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
    fn deepseek_paths_follow_its_own_layout() {
        // 依据官方文档（owner 提供）：Responses 用 /responses；Anthropic 兼容层在
        // /anthropic/v1/messages；Completions 是 /chat/completions。
        let base = "https://api.deepseek.com";
        assert_eq!(
            endpoint(base, Protocol::OpenaiResponses),
            "https://api.deepseek.com/responses"
        );
        assert_eq!(
            endpoint(base, Protocol::AnthropicMessages),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        assert_eq!(
            endpoint(base, Protocol::OpenaiCompletions),
            "https://api.deepseek.com/chat/completions"
        );
        // 用户把 SDK 里的 `/v1` 也粘进来时先去掉再拼
        assert_eq!(
            endpoint("https://api.deepseek.com/v1", Protocol::OpenaiResponses),
            "https://api.deepseek.com/responses"
        );
    }

    #[test]
    fn anthropic_and_openai_use_standard_paths() {
        assert_eq!(
            endpoint("https://api.anthropic.com", Protocol::AnthropicMessages),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint("https://api.openai.com", Protocol::OpenaiResponses),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            endpoint("https://api.openai.com/v1", Protocol::OpenaiCompletions),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn custom_gateway_keeps_standard_suffixes() {
        let base = "http://10.0.0.5:8080";
        assert_eq!(
            endpoint(base, Protocol::OpenaiCompletions),
            "http://10.0.0.5:8080/v1/chat/completions"
        );
        assert_eq!(
            endpoint(base, Protocol::OpenaiResponses),
            "http://10.0.0.5:8080/v1/responses"
        );
        assert_eq!(
            endpoint(base, Protocol::AnthropicMessages),
            "http://10.0.0.5:8080/v1/messages"
        );
    }

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

    /// 用户把厂商文档里的 `…/v1` 原样粘进来时，不能再拼一次 `/v1`
    /// （否则 `/v1/v1/chat/completions` → 上游 404）。
    #[test]
    fn base_already_ending_with_v1_does_not_double() {
        for protocol in [
            Protocol::OpenaiCompletions,
            Protocol::OpenaiResponses,
            Protocol::AnthropicMessages,
        ] {
            let with_v1 = endpoint("https://api.example.com/v1", protocol);
            let plain = endpoint("https://api.example.com", protocol);
            assert_eq!(with_v1, plain, "{protocol:?} 带 /v1 时不能重复拼");
            assert!(!with_v1.contains("/v1/v1"), "{with_v1}");
        }
        // 尾斜杠 + /v1 也要吃掉
        assert_eq!(
            endpoint("https://api.example.com/v1/", Protocol::OpenaiCompletions),
            "https://api.example.com/v1/chat/completions"
        );
        // 只有整个路径段是 /v1 才去重；/v1beta 这类保持原样（不强猜）
        assert_eq!(
            endpoint(
                "https://proxy.example.com/v1beta",
                Protocol::OpenaiCompletions
            ),
            "https://proxy.example.com/v1beta/v1/chat/completions"
        );
        // 带子路径的网关：只去尾部 /v1
        assert_eq!(
            endpoint(
                "https://gw.example.com/openai/v1",
                Protocol::OpenaiCompletions
            ),
            "https://gw.example.com/openai/v1/chat/completions"
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
