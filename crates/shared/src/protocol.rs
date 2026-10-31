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

/// 一次工具调用的引用（契约缺口 C1）。
///
/// 与 [`Chunk::ToolCall`] 同构（`id` / `name` / `arguments`）：前者是"**下一轮请求里**
/// 要调什么"，后者是"**流式下发时**要调什么"。**刻意不合并成一个类型**——
/// `Chunk` 是对外 SSE 的 wire 形状（`{"type":"tool_call","id":…}`），
/// `ChatMessage` 是内部上下文形状，两者独立演进，谁都不能为了"类型整洁"去改对方的字段。
///
/// `arguments` 永远是**合法 JSON 值**（缺口 C5）：完整调用应是对象；聚合过程中的未完成
/// 片段允许是 `Value::String(原始片段)`（由 `gateway` 按 `index` 拼接后整体解析）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRef {
    /// 调用 id：与工具结果消息的 `tool_call_id` 一一对应
    pub id: String,
    /// 工具名（必须是端侧已注册的能力，见 `crates/core/src/tools.rs`）
    pub name: String,
    /// 参数（JSON 值，见类型注释）
    pub arguments: serde_json::Value,
}

impl ToolCallRef {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// 一条消息。
///
/// `content` 只装**可见文本**：assistant 发起工具调用时把调用放进 [`ChatMessage::tool_calls`]，
/// 不再拿 `content` 当 arguments 的载体（缺口 C1）。所以"边说边调工具"以及**并行多调用**
/// 都能表达。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    /// 可见文本；assistant 只调工具不发言时为空串
    pub content: String,
    /// 工具结果消息必须带 `tool_call_id`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 工具名（可选，便于日志与调试）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// assistant 要调用的工具（0..n 个，支持并行调用）；只在 `role = Assistant` 上有意义
    /// （`ChatRequest::validate` 会拦住其他角色）。
    ///
    /// `#[serde(default)]` 是**向后兼容**的唯一理由：加这个字段之前落下的 JSON 没有它，
    /// 反序列化后必须是空 `Vec`（有测试守着：`old_json_without_tool_calls_still_deserializes`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRef>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }

    /// assistant「边说边调工具」（缺口 C1）：`content` 是可见文本，`calls` 是工具调用。
    ///
    /// `content` 传空串表示只调工具、不说话（此时 Completions 的 `content` 编码成 `null`）。
    pub fn assistant_calls(content: impl Into<String>, calls: Vec<ToolCallRef>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: calls,
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
            tool_calls: Vec::new(),
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
        // C1：工具调用的结构性校验。三条都必须在**发请求之前**失败——
        // 尤其 `role = Tool` 缺 id 时，三家上游都会直接 400。
        for message in &self.messages {
            if !message.tool_calls.is_empty() && message.role != Role::Assistant {
                return Err((
                    ErrorCode::ValidationError,
                    format!(
                        "只有 assistant 消息可以带 tool_calls（当前 role = {:?}）",
                        message.role
                    ),
                ));
            }
            for call in &message.tool_calls {
                if call.id.trim().is_empty() || call.name.trim().is_empty() {
                    return Err((
                        ErrorCode::ValidationError,
                        format!(
                            "工具调用的 id / name 不能为空（id = {:?}, name = {:?}）",
                            call.id, call.name
                        ),
                    ));
                }
            }
            if message.role == Role::Tool
                && message
                    .tool_call_id
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
            {
                return Err((
                    ErrorCode::ValidationError,
                    "工具结果消息必须带非空 tool_call_id".into(),
                ));
            }
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
    /// 模型要求执行工具（端侧执行）。与 `ToolCallRef` 同构但**刻意独立**：
    /// 这是对外 SSE 的 wire 形状，字段不能因为内部类型整洁而变。
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
        /// 并行调用时的序号（C9）：Completions 的 `tool_calls[].index`、
        /// Anthropic 的 `content_block_start.index`、Responses 的 `output_index`。
        ///
        /// 有了它，gateway/编排层才能把同一次并行调用的参数分片正确归位（否则 A 的参数
        /// 会被拼到 B 上）。老客户端不认识该字段：`None` 时**不序列化**，wire 向后兼容。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
    },
    /// 端侧回填的工具结果（第 2 轮请求里出现）
    ToolResult {
        id: String,
        ok: bool,
        content: String,
    },
    /// token 用量（抽样，不用于计费）。可能和 `Done` 在**同一个上游事件**里产出：
    /// `parse_events` 会先给 `Usage` 再给 `Done`（见 `crates/core/src/protocols`）。
    Usage {
        tokens_in: u32,
        tokens_out: u32,
        estimated: bool,
    },
    /// 必末且唯一（跨事件；同一事件内一定排在最后）
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
///
/// # 分片语义（缺口 C6）
///
/// 上游经常把 usage 拆在**多个事件**里，而且每个事件只给**一个字段**：
///
/// - Anthropic：`message_start` 给 `input_tokens`（`output_tokens` 只是占位值 1），
///   `message_delta` 只给 `output_tokens`；
/// - Completions：`stream_options.include_usage` 时是一个独立的末帧；
/// - Responses：`response.completed` 一次给全两边。
///
/// 因此 `tokens_in` / `tokens_out` **为 0 表示"本事件未携带该字段"，不是真实的 0**
/// （真实 0 只可能出现在空输入/空输出，实践中由"未携带"覆盖）。
/// 同一 `turn_id` 的多个 `Usage` 由 gateway / 记账层**按字段取 max 合并**：
/// 上游的累计值单调不减，Anthropic 的 input / output 又是互不重叠的分片，取 max 两种情形都对。
/// 任一事件 `estimated = true`，合并结果也是 `true`（宁可在不确定时高估）。
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

    /// C9：`ToolCall.index` 的 wire 兼容。
    ///
    /// - 有 `index` → 序列化里出现 `"index":1`，往返不丢；
    /// - 旧 JSON（没有 `index`）→ 反序列化成 `None`；
    /// - `None` → **不序列化**（老客户端看不懂新字段也无所谓）。
    #[test]
    fn tool_call_index_is_wire_compatible() {
        let with_index = Chunk::ToolCall {
            id: "call_1".into(),
            name: "get_courses".into(),
            arguments: serde_json::json!({ "weekday": 3 }),
            index: Some(1),
        };
        let json = serde_json::to_string(&with_index).unwrap();
        assert!(json.contains("\"index\":1"), "{json}");
        let back: Chunk = serde_json::from_str(&json).unwrap();
        assert_eq!(back, with_index);

        let legacy = r#"{"type":"tool_call","id":"call_1","name":"get_courses","arguments":{}}"#;
        match serde_json::from_str::<Chunk>(legacy).unwrap() {
            Chunk::ToolCall { index, .. } => assert_eq!(index, None, "旧 JSON 必须读成 None"),
            other => panic!("应为 ToolCall：{other:?}"),
        }

        let no_index = Chunk::ToolCall {
            id: "c".into(),
            name: "n".into(),
            arguments: serde_json::json!({}),
            index: None,
        };
        let json = serde_json::to_string(&no_index).unwrap();
        assert!(!json.contains("index"), "None 不能出现在 wire 里：{json}");
    }
    #[test]
    fn tool_result_detection() {
        let mut r = req();
        assert!(!r.has_tool_result());
        r.messages
            .push(ChatMessage::tool_result("c1", "get_courses", "{}"));
        assert!(r.has_tool_result());
    }

    fn calls() -> Vec<ToolCallRef> {
        vec![ToolCallRef::new(
            "call_1",
            "get_courses",
            serde_json::json!({ "weekday": 3 }),
        )]
    }

    /// C1：assistant 可以「边说边调工具」——可见文本与工具调用共存，且能过校验。
    #[test]
    fn assistant_can_carry_text_and_tool_calls() {
        let mut r = req();
        r.messages
            .push(ChatMessage::assistant_calls("我先查一下课表", calls()));
        assert!(r.validate().is_ok());

        let message = r.messages.last().unwrap();
        assert_eq!(message.content, "我先查一下课表");
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].id, "call_1");
        assert_eq!(message.tool_calls[0].name, "get_courses");

        // 序列化往返不丢字段。
        let back: ChatMessage =
            serde_json::from_str(&serde_json::to_string(message).unwrap()).unwrap();
        assert_eq!(&back, message);
    }

    /// C1：并行多工具调用（两个 call 同时下发）。
    #[test]
    fn parallel_tool_calls_are_representable() {
        let two = vec![
            ToolCallRef::new("call_1", "get_courses", serde_json::json!({ "weekday": 3 })),
            ToolCallRef::new("call_2", "get_calendar", serde_json::json!({})),
        ];
        let mut r = req();
        r.messages.push(ChatMessage::assistant_calls("", two));
        assert!(r.validate().is_ok());
        assert_eq!(r.messages.last().unwrap().tool_calls.len(), 2);
    }

    /// `#[serde(default)]` 的**唯一**理由：加字段之前落下的 JSON 必须还能读，
    /// 且序列化回去时不能平白多出空字段（wire 稳定）。
    #[test]
    fn old_json_without_tool_calls_still_deserializes() {
        let legacy = r#"{"role":"assistant","content":"旧数据"}"#;
        let message: ChatMessage = serde_json::from_str(legacy).unwrap();
        assert_eq!(message.role, Role::Assistant);
        assert_eq!(message.content, "旧数据");
        assert!(message.tool_calls.is_empty());
        assert_eq!(message.tool_call_id, None);
        assert_eq!(serde_json::to_string(&message).unwrap(), legacy);
    }

    /// 校验①：`tool_calls` 只能挂在 assistant 上，user / system 一律拒。
    #[test]
    fn tool_calls_are_rejected_outside_assistant() {
        for role in [Role::User, Role::System] {
            let mut r = req();
            r.messages.push(ChatMessage {
                role,
                content: String::new(),
                tool_call_id: None,
                name: None,
                tool_calls: calls(),
            });
            let (code, message) = r.validate().unwrap_err();
            assert_eq!(code, ErrorCode::ValidationError, "{role:?}");
            assert!(message.contains("assistant"), "{role:?}：{message}");
        }
    }

    /// 校验②：每个调用的 `id` / `name` 不能为空。
    #[test]
    fn tool_call_id_and_name_must_not_be_empty() {
        for call in [
            ToolCallRef::new("", "get_courses", serde_json::json!({})),
            ToolCallRef::new("call_1", "  ", serde_json::json!({})),
        ] {
            let mut r = req();
            r.messages
                .push(ChatMessage::assistant_calls("", vec![call]));
            let (code, message) = r.validate().unwrap_err();
            assert_eq!(code, ErrorCode::ValidationError);
            assert!(message.contains("id / name"), "{message}");
        }
    }

    /// 校验③：工具结果必须带**非空** `tool_call_id`（空串会让上游直接 400）。
    #[test]
    fn tool_result_requires_non_empty_call_id() {
        for id in [None, Some(""), Some("   ")] {
            let mut r = req();
            r.messages.push(ChatMessage {
                role: Role::Tool,
                content: "{}".into(),
                tool_call_id: id.map(str::to_owned),
                name: Some("get_courses".into()),
                tool_calls: Vec::new(),
            });
            let (code, message) = r.validate().unwrap_err();
            assert_eq!(code, ErrorCode::ValidationError, "{id:?}");
            assert!(message.contains("tool_call_id"), "{id:?}：{message}");
        }
    }
}
