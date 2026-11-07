//! provider 维度的协议选择与回退（t114）。
//!
//! 背景：设置页里用户只能选 **provider**（DeepSeek / Anthropic / 自建…），
//! 但每个 provider 支持哪套协议并不一样：
//!
//! | host | 支持 | 端点路径（见 `gateway::wire::endpoint`） |
//! |---|---|---|
//! | `api.deepseek.com` | responses + messages + completions | `/responses`、`/anthropic/v1/messages`、`/chat/completions` |
//! | `api.anthropic.com` | messages | `/v1/messages` |
//! | `api.openai.com` | responses + completions | `/v1/responses`、`/v1/chat/completions` |
//! | 其它（自建/局域网/中转） | 全支持 | base_url + 标准 `/v1` 后缀 |
//!
//! 依据是 owner 提供的官方文档（DeepSeek 三种 API 都支持，只是 **base_url/路径不同**）。
//! 早先"deepseek 只支持 completions"的静态映射是**错的**，已撤销（t118）。
//!
//! 回退顺序（owner 指定）：`openai_responses` → `anthropic_messages` → `openai_completions`。
//!
//! # 观测
//!
//! 回退**绝不静默**：meta 帧里带 `requested_protocol` / `effective_protocol`，
//! 并且发生回退时单独发一条 `protocol_fallback` 事件（含原因），客户端可以直接显示
//! "该 provider 不支持 X，已自动改用 Y"。
//!
//! # 已知限制
//!
//! 这里是**静态**判定（按 host 表 + 用户偏好）。真正"上游说 404 再换一个协议重试"的
//! 运行时回退还没做——见 [`resolve`] 的说明与 `docs/` 里的后续计划。

use yunong_shared::Protocol;

/// 回退顺序（owner 指定）。
pub const FALLBACK_ORDER: [Protocol; 3] = [
    Protocol::OpenaiResponses,
    Protocol::AnthropicMessages,
    Protocol::OpenaiCompletions,
];

/// 协议在 wire/日志里的稳定名字。
pub const fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiCompletions => "openai_completions",
        Protocol::OpenaiResponses => "openai_responses",
        Protocol::AnthropicMessages => "anthropic_messages",
    }
}

/// 宽进严出地解析协议名（客户端可能写 `responses` / `messages` / `openai_responses`…）。
///
/// 返回 `Err` 表示**认不出来**——调用方默认按 completions 处理，但**必须**在观测里说清楚，
/// 不能像以前那样静默当成 completions（t114 的 194 条事件就是这么产生的）。
pub fn parse_protocol_name(raw: &str) -> Result<Protocol, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "openai_completions" | "completions" | "openai-chat" | "chat_completions" => {
            Ok(Protocol::OpenaiCompletions)
        }
        "openai_responses" | "responses" | "openai-responses" => Ok(Protocol::OpenaiResponses),
        "anthropic_messages" | "messages" | "anthropic" | "anthropic-messages" => {
            Ok(Protocol::AnthropicMessages)
        }
        other => Err(format!("认不出的协议名：{other}")),
    }
}

/// 一次解析的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// 客户端/设置里给的**原始字符串**（观测用，`requested_protocol` 用的就是它）
    pub requested_name: String,
    /// 用户/设置里要的协议
    pub requested: Protocol,
    /// 实际会用的协议（== requested 时表示没回退）
    pub effective: Protocol,
    /// 从 `base_url` 解析出的 host（空串 = 没配 base_url）
    pub provider: String,
    /// 回退原因（None = 没回退）
    pub fallback_reason: Option<String>,
}

impl Choice {
    pub fn fallback(&self) -> bool {
        self.requested != self.effective
    }
}

/// 从 `base_url` 里取 host（小写）。没有 scheme 也能取（例如 `api.deepseek.com/v1`）。
pub fn provider_host(base_url: &str) -> String {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim();
    // 去掉端口（`localhost:8080` → `localhost`），但保留 IPv6 的方括号主体
    let host = match host.rsplit_once(':') {
        Some((head, tail)) if tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => host,
    };
    host.to_ascii_lowercase()
}

/// 这个 host 支持哪些协议（按 [`FALLBACK_ORDER`] 的顺序）。
///
/// **未知 host 视为全都支持**：自建网关/中转可能支持任意一套，替用户擅自降级更糟。
pub fn supported_protocols(host: &str) -> Vec<Protocol> {
    let host = host.trim().to_ascii_lowercase();
    let matches = |suffix: &str| host == suffix || host.ends_with(&format!(".{suffix}"));
    if matches("api.deepseek.com") || matches("deepseek.com") {
        // DeepSeek 三种协议都支持（Responses / Anthropic 兼容层 / OpenAI 兼容），
        // 只是**端点路径不同**——见 `gateway::wire::endpoint`。
        return FALLBACK_ORDER.to_vec();
    }
    if matches("api.anthropic.com") || matches("anthropic.com") {
        return vec![Protocol::AnthropicMessages];
    }
    if matches("api.openai.com") || matches("openai.com") {
        return vec![Protocol::OpenaiResponses, Protocol::OpenaiCompletions];
    }
    FALLBACK_ORDER.to_vec()
}

/// 解析"该用哪个协议"。
///
/// 规则：
/// 1. `requested` 在 supported 里 → 直接用（不回退）；
/// 2. 否则按 [`FALLBACK_ORDER`] 取第一个 supported 的；
/// 3. 一个都不支持（理论上不会）→ 退回 `requested`，让上游如实报错。
pub fn resolve(base_url: &str, requested: Protocol) -> Choice {
    resolve_named(base_url, protocol_name(requested), requested).0
}

/// 同 [`resolve`]，但以**客户端给的原始字符串**为准（未知名字会降级并如实报告）。
pub fn resolve_raw(base_url: &str, raw_protocol: &str) -> Choice {
    match parse_protocol_name(raw_protocol) {
        Ok(requested) => resolve_named(base_url, raw_protocol, requested).0,
        Err(reason) => {
            // 认不出协议名：按 completions 处理，但把原因说出来
            let mut choice = resolve_named(base_url, raw_protocol, Protocol::OpenaiCompletions).0;
            choice.fallback_reason = Some(format!(
                "{reason}；已按 openai_completions 处理（设置里请选 openai_responses / anthropic_messages / openai_completions）"
            ));
            choice
        }
    }
}

fn resolve_named(base_url: &str, requested_name: &str, requested: Protocol) -> (Choice, ()) {
    let provider = provider_host(base_url);
    let supported = supported_protocols(&provider);
    if supported.contains(&requested) {
        return (
            Choice {
                requested_name: requested_name.to_owned(),
                requested,
                effective: requested,
                provider,
                fallback_reason: None,
            },
            (),
        );
    }
    let effective = FALLBACK_ORDER
        .iter()
        .copied()
        .find(|protocol| supported.contains(protocol))
        .unwrap_or(requested);
    let reason = format!(
        "该 provider（{}）不支持 {}，已自动改用 {}",
        if provider.is_empty() {
            "未配置 base_url"
        } else {
            &provider
        },
        protocol_name(requested),
        protocol_name(effective)
    );
    (
        Choice {
            requested_name: requested_name.to_owned(),
            requested,
            effective,
            provider,
            fallback_reason: Some(reason),
        },
        (),
    )
}

/// `protocol_fallback` 事件（wire 形状，紧跟 meta 之后发一条）。
///
/// ```json
/// {"type":"protocol_fallback","requested_protocol":"openai_responses",
///  "effective_protocol":"anthropic_messages","provider":"api.deepseek.com",
///  "reason":"该 provider（api.deepseek.com）不支持 openai_completions，已自动改用 anthropic_messages"}
/// ```
pub fn fallback_event_json(choice: &Choice, reason: &str) -> String {
    serde_json::json!({
        "type": "protocol_fallback",
        "requested_protocol": protocol_name(choice.requested),
        "effective_protocol": protocol_name(choice.effective),
        "provider": choice.provider,
        "reason": reason,
    })
    .to_string()
}

/// 上游是不是在明确说"这套协议/端点不存在"？返回原因（`None` = 不该回退）。
///
/// 判定规则（严格，宁可少回退）：
/// - **只看 [`crate::error::CoreError::Remote`]**：传输层/超时/取消都不算；
/// - 401/403/429/402/超时 → **绝不回退**（换协议也没用）；
/// - message 以 `HTTP 404` / `HTTP 405` 开头（`gateway::remote_error` 的固定前缀）→ 回退；
/// - 4xx 且正文明确提到协议/端点不支持（`unsupported protocol` 等）→ 回退；
/// - 5xx 一律不回退（服务端故障，换协议同样是错的）。
pub fn unsupported_signal(err: &crate::error::CoreError) -> Option<String> {
    use yunong_shared::ErrorCode;

    let crate::error::CoreError::Remote { code, message, .. } = err else {
        return None;
    };
    if matches!(
        code,
        ErrorCode::Unauthorized
            | ErrorCode::Forbidden
            | ErrorCode::RateLimited
            | ErrorCode::QuotaExceeded
            | ErrorCode::TokenExpired
            | ErrorCode::UpstreamTimeout
    ) {
        return None;
    }
    let lower = message.to_lowercase();
    if lower.starts_with("http 404") || lower.starts_with("http 405") {
        return Some(format!(
            "上游返回 {}（端点不存在，可能不支持该协议）",
            if lower.starts_with("http 404") {
                "HTTP 404"
            } else {
                "HTTP 405"
            }
        ));
    }
    // 只有 4xx 才考虑"正文说协议不支持"
    if lower.starts_with("http 4")
        && (lower.contains("unsupported protocol")
            || lower.contains("protocol not supported")
            || lower.contains("no such endpoint")
            || lower.contains("unknown endpoint")
            || lower.contains("not supported"))
    {
        return Some(format!("上游明确表示不支持该协议：{}", clip(message, 120)));
    }
    None
}

/// 运行时回退的下一个候选（不含已试过的协议；未知 host 也能回退）。
pub fn next_runtime_candidate(
    provider: &str,
    failed: Protocol,
    tried: &[Protocol],
) -> Option<Protocol> {
    let supported = supported_protocols(provider);
    FALLBACK_ORDER.iter().copied().find(|protocol| {
        supported.contains(protocol) && *protocol != failed && !tried.contains(protocol)
    })
}

/// 运行时回退事件（在 `protocol_fallback` 上补 `retried` / `attempt`）。
pub fn retried_event_json(
    requested_name: &str,
    provider: &str,
    effective: Protocol,
    reason: &str,
    attempt: u32,
) -> String {
    serde_json::json!({
        "type": "protocol_fallback",
        "requested_protocol": requested_name,
        "effective_protocol": protocol_name(effective),
        "provider": provider,
        "reason": reason,
        "retried": true,
        "attempt": attempt,
    })
    .to_string()
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_host_handles_scheme_port_and_bare_host() {
        assert_eq!(
            provider_host("https://api.deepseek.com/v1"),
            "api.deepseek.com"
        );
        assert_eq!(
            provider_host("https://api.deepseek.com"),
            "api.deepseek.com"
        );
        assert_eq!(provider_host("api.deepseek.com/v1"), "api.deepseek.com");
        assert_eq!(provider_host("http://localhost:8080/v1"), "localhost");
        assert_eq!(provider_host(""), "");
        assert_eq!(provider_host("HTTPS://API.OPENAI.COM/v1"), "api.openai.com");
    }

    #[test]
    fn known_providers_support_what_we_think() {
        // DeepSeek 三种都支持（t118 更正）
        assert_eq!(
            supported_protocols("api.deepseek.com"),
            FALLBACK_ORDER.to_vec()
        );
        assert_eq!(
            supported_protocols("api.anthropic.com"),
            vec![Protocol::AnthropicMessages]
        );
        assert_eq!(
            supported_protocols("api.openai.com"),
            vec![Protocol::OpenaiResponses, Protocol::OpenaiCompletions]
        );
        // 未知 host：全支持（不擅自降级）
        assert_eq!(
            supported_protocols("llm.internal.example"),
            FALLBACK_ORDER.to_vec()
        );
    }

    #[test]
    fn deepseek_keeps_all_three_protocols() {
        // t118：DeepSeek 三种协议都支持，**不应**再被静态降级
        let choice = resolve("https://api.deepseek.com", Protocol::OpenaiResponses);
        assert!(!choice.fallback(), "{choice:?}");
        assert_eq!(choice.effective, Protocol::OpenaiResponses);
        assert_eq!(choice.provider, "api.deepseek.com");
    }

    #[test]
    fn openai_keeps_responses_and_anthropic_keeps_messages() {
        let openai = resolve("https://api.openai.com/v1", Protocol::OpenaiResponses);
        assert!(!openai.fallback());
        let anthropic = resolve("https://api.anthropic.com", Protocol::AnthropicMessages);
        assert!(!anthropic.fallback());
        // Anthropic + completions → 回退到 messages
        let wrong = resolve("https://api.anthropic.com", Protocol::OpenaiCompletions);
        assert_eq!(wrong.effective, Protocol::AnthropicMessages);
        assert!(wrong.fallback());
    }

    #[test]
    fn unknown_provider_keeps_whatever_user_picked() {
        let choice = resolve("https://llm.internal.example/v1", Protocol::OpenaiResponses);
        assert!(!choice.fallback(), "{choice:?}");
        assert_eq!(choice.effective, Protocol::OpenaiResponses);
    }

    #[test]
    fn fallback_event_has_the_contract_fields() {
        // 用一个**静态表里不支持**的组合触发回退：Anthropic 主机 + completions
        let choice = resolve("https://api.anthropic.com", Protocol::OpenaiCompletions);
        assert!(choice.fallback(), "{choice:?}");
        let event: serde_json::Value =
            serde_json::from_str(&fallback_event_json(&choice, "原因")).unwrap();
        assert_eq!(event["type"], "protocol_fallback");
        assert_eq!(event["requested_protocol"], "openai_completions");
        assert_eq!(event["effective_protocol"], "anthropic_messages");
        assert_eq!(event["provider"], "api.anthropic.com");
        assert_eq!(event["reason"], "原因");
    }
}
