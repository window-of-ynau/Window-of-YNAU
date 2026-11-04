//! `tool_choice`：把客户端的"要不要强制调用工具"透传到上游（t124）。
//!
//! # 取值
//!
//! | 客户端值 | 含义 |
//! |---|---|
//! | `auto`（默认） | 模型自己决定 |
//! | `none` | 本次不许调用工具 |
//! | `required` | **强制**调用至少一个工具（诊断弹窗的"强制工具调用"开关） |
//!
//! 认不出的值按 `auto` 处理并返回一条 warn（不静默改语义）。
//!
//! # 三协议的差异（重要，别假设它们等价）
//!
//! | 协议 | `required` 的写法 | 是否真的强制 |
//! |---|---|---|
//! | `openai_completions` | `"tool_choice": "required"` | ✅ 原生支持 |
//! | `openai_responses` | `"tool_choice": "required"` | ✅（DeepSeek 文档明确支持） |
//! | `anthropic_messages` | `{"type": "any"}` | ⚠️ **不保证**：DeepSeek 的 Anthropic 兼容层把 `any` 按
//!   `auto` 处理，所以这条路径下"强制"可能不生效 |
//!
//! 因此对 `messages` 协议，`effective` 会报成 `"any"`（而不是 `required`），
//! 客户端据此知道"这次没被真正强制"。若模型最终没调用工具，`done` 帧里会有
//! `tool_choice_honored: false`（见 `chat_ffi`）。
//!
//! # 只对本次请求生效
//!
//! 它**不是**持久配置：只随这一次用户提问的首轮请求发出去（工具循环的第 2 轮起
//! 不再带 `required`，否则会逼模型反复调工具直到轮次上限）。

use serde_json::Value;
use yunong_shared::Protocol;

/// 客户端要的"工具选择"策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
}

impl ToolChoice {
    /// 客户端原始值 → 策略。返回值第二项是 warn（认不出的值）。
    pub fn parse(raw: Option<&str>) -> (Self, Option<String>) {
        let Some(raw) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
            return (Self::Auto, None);
        };
        match raw.to_ascii_lowercase().as_str() {
            "auto" => (Self::Auto, None),
            "none" => (Self::None, None),
            "required" | "any" => (Self::Required, None),
            other => (
                Self::Auto,
                Some(format!(
                    "未知的 tool_choice「{other}」，已按 auto 处理（可用：auto / none / required）"
                )),
            ),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Required => "required",
        }
    }
}

/// 发往上游的 `tool_choice` 字段值（`None` = 不写这个字段）。
///
/// `auto` 不写：各家默认就是 auto，少发字段少一分兼容风险。
pub fn wire_value(protocol: Protocol, choice: ToolChoice) -> Option<Value> {
    match (protocol, choice) {
        (_, ToolChoice::Auto) => None,
        // OpenAI 两套都是字符串
        (Protocol::OpenaiCompletions | Protocol::OpenaiResponses, ToolChoice::Required) => {
            Some(Value::String("required".to_owned()))
        }
        (Protocol::OpenaiCompletions | Protocol::OpenaiResponses, ToolChoice::None) => {
            Some(Value::String("none".to_owned()))
        }
        // Anthropic 是对象；`any` 在 DeepSeek 兼容层里**不强制**（见模块文档）
        (Protocol::AnthropicMessages, ToolChoice::Required) => {
            Some(serde_json::json!({ "type": "any" }))
        }
        (Protocol::AnthropicMessages, ToolChoice::None) => {
            Some(serde_json::json!({ "type": "none" }))
        }
    }
}

/// 实际生效的取值（写进 meta 的 `tool_choice_effective`）。
///
/// 与 [`wire_value`] 一一对应，但用**可读字符串**：`"required"` / `"any"` / `"none"` / `"auto"`。
pub fn effective_label(protocol: Protocol, choice: ToolChoice) -> &'static str {
    match (protocol, choice) {
        (_, ToolChoice::Auto) => "auto",
        (_, ToolChoice::None) => "none",
        (Protocol::AnthropicMessages, ToolChoice::Required) => "any",
        (_, ToolChoice::Required) => "required",
    }
}

/// 这个组合下"强制调用"是否**真的**被上游强制执行。
pub fn is_enforced(protocol: Protocol, choice: ToolChoice) -> bool {
    matches!(choice, ToolChoice::Required) && protocol != Protocol::AnthropicMessages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_values_and_defaults_to_auto() {
        assert_eq!(ToolChoice::parse(None), (ToolChoice::Auto, None));
        assert_eq!(ToolChoice::parse(Some("")), (ToolChoice::Auto, None));
        assert_eq!(ToolChoice::parse(Some("auto")), (ToolChoice::Auto, None));
        assert_eq!(ToolChoice::parse(Some("NONE")), (ToolChoice::None, None));
        assert_eq!(
            ToolChoice::parse(Some("required")),
            (ToolChoice::Required, None)
        );
        // 客户端如果直接传 Anthropic 的 `any`，也当"强制"
        assert_eq!(ToolChoice::parse(Some("any")), (ToolChoice::Required, None));
    }

    #[test]
    fn unknown_value_falls_back_to_auto_with_a_warning() {
        let (choice, warn) = ToolChoice::parse(Some("must"));
        assert_eq!(choice, ToolChoice::Auto);
        assert!(warn.as_deref().unwrap().contains("未知的 tool_choice"));
    }

    #[test]
    fn wire_values_per_protocol() {
        // completions / responses：字符串
        assert_eq!(
            wire_value(Protocol::OpenaiCompletions, ToolChoice::Required),
            Some(serde_json::json!("required"))
        );
        assert_eq!(
            wire_value(Protocol::OpenaiResponses, ToolChoice::Required),
            Some(serde_json::json!("required"))
        );
        // messages：对象，required 映射成 any（且**不保证**强制）
        assert_eq!(
            wire_value(Protocol::AnthropicMessages, ToolChoice::Required),
            Some(serde_json::json!({ "type": "any" }))
        );
        assert_eq!(
            effective_label(Protocol::AnthropicMessages, ToolChoice::Required),
            "any"
        );
        assert!(!is_enforced(
            Protocol::AnthropicMessages,
            ToolChoice::Required
        ));
        assert!(is_enforced(
            Protocol::OpenaiCompletions,
            ToolChoice::Required
        ));
        // auto 一律不发字段
        for protocol in [
            Protocol::OpenaiCompletions,
            Protocol::OpenaiResponses,
            Protocol::AnthropicMessages,
        ] {
            assert_eq!(wire_value(protocol, ToolChoice::Auto), None);
        }
    }
}
