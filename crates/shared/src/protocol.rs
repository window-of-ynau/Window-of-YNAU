//! 对话协议：三协议归一化后的统一结构（ADR-13）。
//!
//! 上层（编排、UI、记账）只看这些类型，**永远不看** Completions / Responses /
//! Messages 的原始字段。

use serde::{Deserialize, Serialize};

use crate::{entitlement::Channel, error::ErrorCode};

/// 三种上游协议。新增厂商只新增适配器，不动上层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// OpenAI `/v1/chat/completions`
    OpenaiCompletions,
    /// OpenAI `/v1/responses`
    OpenaiResponses,
    /// Anthropic `/v1/messages`
    AnthropicMessages,
}

/// 消息角色。`Tool` 只在多轮工具循环里出现（ADR-05）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 一条消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// 工具结果消息必须带 `tool_call_id`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 工具名（可选，便于日志与调试）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(call_id.into()),
            name: Some(name.into()),
        }
    }
}

/// 工具声明。执行永远在端侧（ADR-05）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema（由端侧提供，服务端原样转发）
    pub parameters: serde_json::Value,
}

/// 一次请求（一轮）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// 端侧生成，云端不落库
    pub session_id: String,
    /// **一次用户提问**的唯一 id：多轮工具循环共享
    pub turn_id: String,
    /// 第几轮，1..=MAX_ROUNDS
    pub round: u8,
    pub model: String,
    pub protocol: Protocol,
    /// 端侧装配好的完整上下文；**必须 append-only**（保 prompt cache）
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default = "default_max_output")]
    pub max_output_tokens: u32,
    #[serde(default)]
    pub stream: bool,
    /// 端侧估算，用于"先拒后转"
    #[serde(default)]
    pub estimated_input_tokens: u32,
}

const fn default_max_output() -> u32 {
    1024
}

impl ChatRequest {
    /// 请求自检。返回 `Err((code, message))` 便于直接映射成 413/422。
    ///
    /// 这里只做**结构性**校验；鉴权、配额、审核在中间件与领域层。
    pub fn validate(&self) -> Result<(), (ErrorCode, String)> {
        if self.session_id.trim().is_empty() {
            return Err((ErrorCode::ValidationError, "session_id 不能为空".into()));
        }
        if self.turn_id.trim().is_empty() {
            return Err((ErrorCode::ValidationError, "turn_id 不能为空".into()));
        }
        if self.round == 0 || self.round > crate::MAX_ROUNDS {
            return Err((
                ErrorCode::ValidationError,
                format!("round 必须在 1..={} 之间", crate::MAX_ROUNDS),
            ));
        }
        if self.messages.is_empty() {
            return Err((ErrorCode::ValidationError, "messages 不能为空".into()));
        }
        if self.model.trim().is_empty() {
            return Err((ErrorCode::ValidationError, "model 不能为空".into()));
        }
        if self.estimated_input_tokens > crate::MAX_INPUT_TOKENS {
            return Err((
                ErrorCode::MessageTooLarge,
                format!(
                    "输入约 {} tokens，超过上限 {}（请压缩上下文或分次提问）",
                    self.estimated_input_tokens,
                    crate::MAX_INPUT_TOKENS
                ),
            ));
        }
        if self.max_output_tokens == 0 {
            return Err((
                ErrorCode::ValidationError,
                "max_output_tokens 必须大于 0".into(),
            ));
        }
        Ok(())
    }

    /// 是否为本轮追加了工具结果（第 2 轮及以后必须为真）。
    pub fn has_tool_result(&self) -> bool {
        self.messages.iter().any(|m| m.role == Role::Tool)
    }
}

/// 流式归一化事件。对应 SSE 的 `event:` 类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Chunk {
    /// 必首
    Meta {
        request_id: String,
        model: String,
        channel: Channel,
    },
    /// 文本增量
    Delta { text: String },
    /// 模型要求执行工具（端侧执行）
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// 端侧回填的工具结果（第 2 轮请求里出现）
    ToolResult {
        id: String,
        ok: bool,
        content: String,
    },
    /// token 用量（抽样，不用于计费）
    Usage {
        tokens_in: u32,
        tokens_out: u32,
        estimated: bool,
    },
    /// 必末且唯一
    Done { finish: FinishReason },
    /// 错误（随后关闭连接）
    Error { code: ErrorCode, message: String },
    /// 心跳
    Ping,
}

/// 结束原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    /// 达到 MAX_ROUNDS，强制收尾
    MaxRounds,
    Error,
}

/// 用量（上游返回优先，缺失时端侧估算并标记 `estimated`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Usage {
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub estimated: bool,
}

impl Chunk {
    /// SSE 的 `event:` 名称。
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::Meta { .. } => "meta",
            Self::Delta { .. } => "delta",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::Usage { .. } => "usage",
            Self::Done { .. } => "done",
            Self::Error { .. } => "error",
            Self::Ping => "ping",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> ChatRequest {
        ChatRequest {
            session_id: "s_1".into(),
            turn_id: "t_1".into(),
            round: 1,
            model: "deepseek-chat".into(),
            protocol: Protocol::OpenaiCompletions,
            messages: vec![ChatMessage::user("下周三下午有课吗")],
            tools: vec![],
            max_output_tokens: 512,
            stream: true,
            estimated_input_tokens: 3120,
        }
    }

    #[test]
    fn valid_request_passes() {
        assert!(req().validate().is_ok());
    }

    #[test]
    fn round_bounds_are_enforced() {
        let mut r = req();
        r.round = 0;
        assert_eq!(r.validate().unwrap_err().0, ErrorCode::ValidationError);
        r.round = crate::MAX_ROUNDS + 1;
        assert_eq!(r.validate().unwrap_err().0, ErrorCode::ValidationError);
    }

    #[test]
    fn oversized_input_maps_to_413() {
        let mut r = req();
        r.estimated_input_tokens = crate::MAX_INPUT_TOKENS + 1;
        let (code, _) = r.validate().unwrap_err();
        assert_eq!(code, ErrorCode::MessageTooLarge);
        assert_eq!(code.http_status(), 413);
    }

    #[test]
    fn chunk_roundtrips_with_event_names() {
        let c = Chunk::Done {
            finish: FinishReason::Stop,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, "{\"type\":\"done\",\"finish\":\"stop\"}");
        assert_eq!(c.event_name(), "done");

        let meta = Chunk::Meta {
            request_id: "req_1".into(),
            model: "m".into(),
            channel: Channel::Plan,
        };
        let back: Chunk = serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn tool_result_detection() {
        let mut r = req();
        assert!(!r.has_tool_result());
        r.messages
            .push(ChatMessage::tool_result("c1", "get_courses", "{}"));
        assert!(r.has_tool_result());
    }
}
