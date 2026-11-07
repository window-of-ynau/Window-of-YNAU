//! 各协议的端点与鉴权头（纯函数，便于单测）。
//!
//! 三家在"怎么带 Key"上不统一：OpenAI 系走 `Authorization: Bearer`，
//! Anthropic 走 `x-api-key` + 必填的 `anthropic-version` 头。这里收敛成一个函数，
//! 免得两个通道各写一遍、将来某家改了只改一处。

use yunong_shared::Protocol;

/// Anthropic 的 API 版本头（官方要求必填）。
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// 完整的上游 URL（BYOK 直连）。
///
/// 规范化与路径映射的**唯一实现**在 `yunong_shared::url`（issue #51）：
/// - `normalize_base_url`：去尾 `/`、去尾部的整个 `/v1` 段（`/v1beta` 不动），保留自定义前缀；
/// - `endpoint_path`：按厂商给路径（DeepSeek 的 `/responses`、Anthropic 兼容层的
///   `/anthropic/v1/messages` 等）。
///
/// 套餐通道（`yunong_server::upstream::upstream_url`）调的是**同一个函数**，
/// 所以"同一个 base_url，一条通道能用、另一条 404"这类问题不会再出现。
pub(crate) fn endpoint(base_url: &str, protocol: Protocol) -> String {
    yunong_shared::provider_url(base_url, protocol)
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

    /// 同一类配置坑：用户把 DeepSeek 文档里的 Anthropic base（`…/anthropic`）整段粘进来，
    /// 不许再补一个 `/anthropic`（否则 `/anthropic/anthropic/v1/messages` → 404）。
    #[test]
    fn anthropic_base_pasted_whole_is_not_doubled() {
        assert_eq!(
            endpoint(
                "https://api.deepseek.com/anthropic",
                Protocol::AnthropicMessages
            ),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        // 带尾斜杠 / 带 /v1 的组合也不能double
        assert_eq!(
            endpoint(
                "https://api.deepseek.com/anthropic/",
                Protocol::AnthropicMessages
            ),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
    }

    /// issue #51：`base_url` 带 `/v1` 时两条通道必须给出**同一个**最终 URL。
    ///
    /// 这里用"自定义网关"（未知 host → 标准路径）做对照，断言 core 的拼接结果
    /// 正好等于 `yunong_shared::endpoint_url`（server 侧用的是同一个函数）。
    #[test]
    fn base_url_with_v1_matches_the_shared_cases() {
        // 与服务端（`yunong_server::upstream::upstream_url`）**断言同一张表**：
        // 两侧调的都是 `yunong_shared::provider_url`，同一个配置必然同一个 URL（#51）。
        for (base, protocol, expected) in yunong_shared::url::ENDPOINT_CASES {
            assert_eq!(
                endpoint(base, *protocol),
                *expected,
                "base={base} protocol={protocol:?}"
            );
            assert!(
                !endpoint(base, *protocol).contains("/v1/v1"),
                "出现了重复 /v1：{base} / {protocol:?}"
            );
        }
        // #51 现场：自定义网关带 /v1，只能出现一个 /v1
        assert_eq!(
            endpoint("https://gw.example.com/v1", Protocol::OpenaiCompletions),
            "https://gw.example.com/v1/chat/completions"
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
