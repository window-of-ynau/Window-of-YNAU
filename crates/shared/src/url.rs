//! 上游地址规范化：**全系统唯一实现**（修 issue #51）。
//!
//! # 为什么要有这个函数
//!
//! 用户在设置页常常把厂商文档里的 `base_url` **原样**粘进来，例如
//! `https://api.deepseek.com/v1`。两条通道对它的处理一度不一致：
//!
//! - BYOK 直连（`yunong_core::gateway::wire`）会剥掉尾部 `/v1` → 正常；
//! - 套餐通道（`yunong_server::upstream`）不剥 → 拼出 `/v1/v1/chat/completions` → **404**。
//!
//! 同一个配置，一条通道能用、另一条 404，是最难排查的一类问题。所以规则只写一遍，
//! 两条通道都必须调用这里。
//!
//! # 规则（顺序固定）
//!
//! 1. 去掉**所有**尾部 `/`（`…/v1//` → `…/v1`）；
//! 2. 去掉**整个路径段**的尾部 `/v1`（`…/v1` → `…`）；`/v1beta`、`/v10` 之类**不动**；
//! 3. 其余自定义路径原样保留（`https://gw.example.com/openai` 保持不动，
//!    反向代理经常就是这种前缀）。
//!
//! 空串、只有 `/` 的输入返回空串（调用方自己决定怎么报错）。

/// 规范化上游 `base_url`：去尾 `/`，再去尾部的整个 `/v1` 路径段。
///
/// 返回值借用入参（不分配），方便热路径调用。
///
/// ```
/// use yunong_shared::normalize_base_url;
/// assert_eq!(normalize_base_url("https://api.deepseek.com/v1"), "https://api.deepseek.com");
/// assert_eq!(normalize_base_url("https://api.deepseek.com/v1/"), "https://api.deepseek.com");
/// assert_eq!(normalize_base_url("https://api.deepseek.com/"), "https://api.deepseek.com");
/// assert_eq!(normalize_base_url("https://gw.example.com/openai"), "https://gw.example.com/openai");
/// assert_eq!(normalize_base_url("https://x.com/v1beta"), "https://x.com/v1beta");
/// ```
pub fn normalize_base_url(base_url: &str) -> &str {
    let base = base_url.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base)
}

/// 把 `base_url` 与端点路径拼成完整 URL：`normalize_base_url(base) + path`。
///
/// `path` 必须以 `/` 开头（各协议的端点常量都是）。
pub fn endpoint_url(base_url: &str, path: &str) -> String {
    let base = normalize_base_url(base_url);
    if base.is_empty() {
        return path.trim_end_matches('/').to_owned();
    }
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// 两条通道**必须都通过**的规范化用例（测试共用，保证行为不漂移）。
///
/// 第一项是用户配置，第二项是规范化后的 base。
#[doc(hidden)]
pub const NORMALIZATION_CASES: &[(&str, &str)] = &[
    // 厂商文档里最常见的形式
    ("https://api.deepseek.com", "https://api.deepseek.com"),
    // 用户把 SDK 里的 /v1 也粘进来（#51 的现场）
    ("https://api.deepseek.com/v1", "https://api.deepseek.com"),
    // 带尾斜杠
    ("https://api.deepseek.com/", "https://api.deepseek.com"),
    ("https://api.deepseek.com/v1/", "https://api.deepseek.com"),
    // 自建/中转的自定义前缀：必须保留
    (
        "https://gw.example.com/openai",
        "https://gw.example.com/openai",
    ),
    (
        "https://gw.example.com/openai/v1",
        "https://gw.example.com/openai",
    ),
    // /v1beta、/v10 不是 `/v1` 段，不许动
    ("https://x.com/v1beta", "https://x.com/v1beta"),
    ("http://127.0.0.1:8080/v10", "http://127.0.0.1:8080/v10"),
    // Anthropic 兼容层的前缀：保留（路径由端侧协议端点负责拼）
    (
        "https://api.deepseek.com/anthropic",
        "https://api.deepseek.com/anthropic",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_cases_hold() {
        for (input, expected) in NORMALIZATION_CASES {
            assert_eq!(normalize_base_url(input), *expected, "input={input}");
        }
    }

    #[test]
    fn empty_and_root_inputs_degrade_gracefully() {
        assert_eq!(normalize_base_url(""), "");
        assert_eq!(normalize_base_url("/"), "");
        assert_eq!(normalize_base_url("https://x.com//"), "https://x.com");
    }

    #[test]
    fn endpoint_url_joins_without_double_v1() {
        // #51 的现场：不允许出现 /v1/v1
        for (input, _) in NORMALIZATION_CASES {
            for path in ["/v1/chat/completions", "/v1/responses", "/v1/messages"] {
                let url = endpoint_url(input, path);
                assert!(
                    !url.contains("/v1/v1"),
                    "拼接出现重复 /v1：{input} + {path} = {url}"
                );
                assert!(url.ends_with(path), "路径被吃掉：{url}");
            }
        }
        assert_eq!(
            endpoint_url("https://api.deepseek.com/v1", "/v1/chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }
}

// ---------------------------------------------------------------------------
// 端点路径：**也必须是单一实现**（#51 的另一半）
// ---------------------------------------------------------------------------
//
// 只把 base 规范化统一还不够：两条通道若各写各的路径，仍然会分叉。
// 例如 DeepSeek 的 Responses 端点是 `/responses`（不是 `/v1/responses`），
// Anthropic 兼容层是 `/anthropic/v1/messages`。所以路径映射也放在这里。

use crate::Protocol;

/// 厂商分支（按 host 判断；认不出就是 `Other`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Deepseek,
    Anthropic,
    Other,
}

/// 按 `base_url` 的 host 判断厂商。
pub fn provider_kind(base_url: &str) -> ProviderKind {
    let host = host_of(base_url);
    if host == "deepseek.com" || host.ends_with(".deepseek.com") {
        ProviderKind::Deepseek
    } else if host == "anthropic.com" || host.ends_with(".anthropic.com") {
        ProviderKind::Anthropic
    } else {
        ProviderKind::Other
    }
}

fn host_of(base_url: &str) -> String {
    let without_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    // 去掉 user:pass@ 与端口
    let host = host.rsplit_once('@').map_or(host, |(_, rest)| rest);
    host.split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// 协议在某家厂商上的端点路径（**唯一实现**，BYOK 与套餐通道共用）。
///
/// 依据（owner 提供的官方文档，2026-11-04 核对）：
/// - DeepSeek：Responses 用 `/responses`；Anthropic 兼容层 `/anthropic/v1/messages`
///   （base 若已经是 `…/anthropic`，只补 `/v1/messages`）；Completions `/chat/completions`；
/// - Anthropic 官方：`/v1/messages`；
/// - OpenAI / 自建中转：标准 `/v1/...`（不猜自定义前缀）。
pub fn endpoint_path(base_url: &str, protocol: Protocol) -> &'static str {
    let base = normalize_base_url(base_url);
    match (provider_kind(base), protocol) {
        (ProviderKind::Deepseek, Protocol::OpenaiCompletions) => "/chat/completions",
        (ProviderKind::Deepseek, Protocol::OpenaiResponses) => "/responses",
        (ProviderKind::Deepseek, Protocol::AnthropicMessages) if base.ends_with("/anthropic") => {
            "/v1/messages"
        }
        (ProviderKind::Deepseek, Protocol::AnthropicMessages) => "/anthropic/v1/messages",
        (ProviderKind::Anthropic, Protocol::AnthropicMessages) => "/v1/messages",
        (_, Protocol::OpenaiCompletions) => "/v1/chat/completions",
        (_, Protocol::OpenaiResponses) => "/v1/responses",
        (_, Protocol::AnthropicMessages) => "/v1/messages",
    }
}

/// 完整的上游 URL：规范化 base + 厂商相关路径。
///
/// **这就是两条通道必须调用的唯一入口。**
pub fn provider_url(base_url: &str, protocol: Protocol) -> String {
    endpoint_url(base_url, endpoint_path(base_url, protocol))
}

/// 两条通道**必须都通过**的最终 URL 用例（输入配置 → 期望 URL）。
#[doc(hidden)]
pub const ENDPOINT_CASES: &[(&str, Protocol, &str)] = &[
    // #51 的现场：带 /v1 的 DeepSeek base
    (
        "https://api.deepseek.com/v1",
        Protocol::OpenaiCompletions,
        // 规范化会剥掉 `/v1`，DeepSeek 的 completions 路径是 `/chat/completions`
        "https://api.deepseek.com/chat/completions",
    ),
    (
        "https://api.deepseek.com",
        Protocol::OpenaiCompletions,
        "https://api.deepseek.com/chat/completions",
    ),
    (
        "https://api.deepseek.com",
        Protocol::OpenaiResponses,
        "https://api.deepseek.com/responses",
    ),
    (
        "https://api.deepseek.com/anthropic",
        Protocol::AnthropicMessages,
        "https://api.deepseek.com/anthropic/v1/messages",
    ),
    (
        "https://api.anthropic.com",
        Protocol::AnthropicMessages,
        "https://api.anthropic.com/v1/messages",
    ),
    // 自建/中转：保留自定义前缀 + 标准路径
    (
        "https://gw.example.com/openai/v1",
        Protocol::OpenaiCompletions,
        "https://gw.example.com/openai/v1/chat/completions",
    ),
    (
        "http://127.0.0.1:8080",
        Protocol::OpenaiResponses,
        "http://127.0.0.1:8080/v1/responses",
    ),
];

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn endpoint_cases_hold() {
        for (base, protocol, expected) in ENDPOINT_CASES {
            assert_eq!(
                provider_url(base, *protocol),
                *expected,
                "base={base} protocol={protocol:?}"
            );
        }
    }

    #[test]
    fn provider_kind_is_host_based() {
        assert_eq!(
            provider_kind("https://api.deepseek.com/v1"),
            ProviderKind::Deepseek
        );
        assert_eq!(
            provider_kind("https://api.anthropic.com"),
            ProviderKind::Anthropic
        );
        assert_eq!(
            provider_kind("https://gw.example.com/openai"),
            ProviderKind::Other
        );
        assert_eq!(provider_kind("http://127.0.0.1:8080"), ProviderKind::Other);
        // 端口、userinfo、大写都不能影响判断
        assert_eq!(
            provider_kind("https://API.DeepSeek.com:443/v1"),
            ProviderKind::Deepseek
        );
        assert_eq!(
            provider_kind("https://user:pw@api.deepseek.com"),
            ProviderKind::Deepseek
        );
    }
}
