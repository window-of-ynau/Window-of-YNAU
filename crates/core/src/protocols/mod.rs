//! 三协议适配（契约见 `docs/03-接口文档.md` §9 / ADR-13）。
//!
//! 这一层只做**纯数据变换**：把内部 `ChatRequest` 编成各家请求体，
//! 把各家的流式事件解析成统一的 `Chunk`。
//!
//! 刻意**不在这里发网络请求**：网络交给 `gateway`，这样适配逻辑可以用 fixture 单测，
//! 不需要真实 Key、不需要联网。
//!
//! # 已知契约缺口
//!
//! 下面这些点在现有 `yunong_shared` 类型里**无法无损表达**，实现里只能给"最小可用编码"，
//! 逐条在对应协议的模块注释里写清了损失点：
//!
//! 1. `ChatMessage` 没有 `tool_calls` 字段。约定：`role = Assistant` 且同时带
//!    `tool_call_id` + `name` 的消息表示"模型请求调用工具"，`content` 放 **arguments 的
//!    JSON 文本**；assistant 的可见文本与工具调用**不能在同一条消息里共存**。
//! 2. `ChatMessage` 没有 `ok` / `is_error`。工具失败在 Anthropic 的
//!    `tool_result.is_error` 里表达不出来，只能把失败文本写进 `content`。
//! 3. `ChatRequest` 没有 `temperature`（§9 的伪代码里有，契约类型没落地）。
//!    三协议一律**不发送**温度字段，用上游默认值。
//! 4. `parse_event` 是**无状态**的：一次 `data` 一次调用，工具调用的 `id` / `name` /
//!    `arguments` 被上游拆到多个事件里时，每次只能给出一部分，见各协议注释。
//! 5. 一次 `data` 只能产出一个 `Chunk`：上游把"结束原因"和 `usage` 塞进同一个事件时
//!    必须二选一（Anthropic `message_delta`、Responses `response.completed`）。
//! 6. `Chunk::Meta` 需要 `channel`（业务概念，来自路由）与 `request_id`，本层拿不到，
//!    所以**不产出 Meta**，由 `gateway` 注入。同理上游的心跳一律 → `None`
//!    （§9 要求"空串/心跳/无关事件 → None"）。

mod completions;
mod messages;
mod responses;

use serde_json::{Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, FinishReason, Protocol, Role};

/// 构造对应协议的请求体。
///
/// 必须处理的差异（见 `docs/03-接口文档.md` §9）：
/// - 系统提示位置：`messages[0]` / `instructions` / 顶层 `system`
/// - 工具声明：`tools[].function` / `tools[]` / `tools[].input_schema`
/// - 流式开关字段名
///
/// 以入参 `protocol` 为准（`request.protocol` 只记录端侧意图）；两者不一致时不 panic，
/// 这样调用方可以按路由结果覆盖端侧声明。
pub fn build_body(protocol: Protocol, request: &ChatRequest) -> Value {
    match protocol {
        Protocol::OpenaiCompletions => completions::build(request),
        Protocol::OpenaiResponses => responses::build(request),
        Protocol::AnthropicMessages => messages::build(request),
    }
}

/// 解析一条流式事件（`data: ` 前缀已剥离；可能是 `[DONE]`）。
///
/// 返回 `None` 表示该事件不需要暴露给上层（例如 keep-alive、空 delta、角色首帧）。
///
/// 三协议统一行为（契约测试覆盖）：
/// - `[DONE]` → `Chunk::Done { finish: Stop }`（Anthropic / Responses 不保证发送它，
///   缺失时由 `gateway` 在流结束时补发）
/// - 空串、非 JSON 文本 → `None`
/// - 顶层 `error` 对象 / `response.failed` → `Chunk::Error { code: UpstreamError, .. }`
///
/// **无状态**：工具调用参数分片请按各协议注释聚合（本函数对不完整分片返回
/// `Value::String` 原始片段）。
pub fn parse_event(protocol: Protocol, data: &str) -> Option<Chunk> {
    let data = data.trim();
    if data.is_empty() {
        return None;
    }
    // 三协议一致：`[DONE]` 视为正常收尾。
    if data == "[DONE]" {
        return Some(Chunk::Done {
            finish: FinishReason::Stop,
        });
    }
    // 非 JSON（SSE 注释、纯文本心跳）不需要暴露给上层。
    let value: Value = serde_json::from_str(data).ok()?;
    if let Some(error) = upstream_error(&value) {
        return Some(error);
    }
    match protocol {
        Protocol::OpenaiCompletions => completions::parse(&value),
        Protocol::OpenaiResponses => responses::parse(&value),
        Protocol::AnthropicMessages => messages::parse(&value),
    }
}

/// 系统提示：把上下文里的所有 `system` 消息按原顺序用空行拼成一段。
///
/// 三协议都需要"把 system 从 messages 里抽走"再各自放置，所以抽成公共函数。
/// 拼接是**唯一**的合并方式：抽走本身不改变各条 system 的相对顺序（append-only 语义保持）。
fn system_prompt(messages: &[ChatMessage]) -> Option<String> {
    let parts: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.trim())
        .filter(|content| !content.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// 结束原因统一映射（§9：`stop` / `length` / `tool_calls` / 其他 → `Stop`）。
///
/// 三家字面量不同，但语义同构：
/// - 正常收尾：`stop`(Completions) / `completed`(Responses) / `end_turn`、`stop_sequence`(Messages)
/// - 达到输出上限：`length`(Completions) / `max_output_tokens`(Responses) / `max_tokens`(Messages)
/// - 要求调用工具：`tool_calls`、`function_call` / `tool_use`(Messages)
///
/// 其他（`content_filter`、`refusal`、`pause_turn`、`cancelled`…）按 §9 归到 `Stop`。
fn finish_reason(raw: &str) -> FinishReason {
    match raw {
        "stop" | "completed" | "end_turn" | "stop_sequence" => FinishReason::Stop,
        "length" | "max_tokens" | "max_output_tokens" => FinishReason::Length,
        "tool_calls" | "function_call" | "tool_use" => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    }
}

/// 工具调用参数分片的统一降级策略（函数无状态，见模块注释第 4 条）。
///
/// - 空分片（首帧常见）→ `{}`
/// - 完整 JSON → 解析后的值
/// - **不完整分片** → `Value::String(原样片段)`，调用方必须按 `index`/`item_id` 累加字符串
///   后整体 `serde_json::from_str`；这是"一次 data 一次调用"的已知限制。
fn partial_json(raw: &str) -> Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return Value::Object(Map::new());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

/// Completions / Responses 的 usage 对象（两家键名不同，语义一致）。
///
/// `estimated = true` 表示上游给了 usage 对象但里面没有可用的计数字段，
/// 此时两个 0 只是占位；真正的端侧估算由 `context` 模块负责。
fn openai_usage(usage: &Value) -> Option<Chunk> {
    let usage = usage.as_object()?;
    let tokens_in = token_count(
        usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens")),
    );
    let tokens_out = token_count(
        usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens")),
    );
    let known = tokens_in.is_some() || tokens_out.is_some();
    Some(Chunk::Usage {
        tokens_in: tokens_in.unwrap_or(0),
        tokens_out: tokens_out.unwrap_or(0),
        estimated: !known,
    })
}

fn token_count(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// 上游错误对象 → `Chunk::Error`（§9 只要求归一成 `UpstreamError`）。
///
/// 三家形状：
/// - Completions：`{"error": {"message": .., "type": ..}}`
/// - Anthropic：`{"type": "error", "error": {"type": .., "message": ..}}`
/// - Responses：`{"type": "response.failed", "response": {"error": {..}}}`
///
/// 上游的细分原因（`overloaded_error` 等）只体现在 `message` 里：`ErrorCode` 是全局
/// 稳定枚举，不能为每家厂商造码，重试策略由 `gateway` 按 HTTP 状态决定。
fn upstream_error(value: &Value) -> Option<Chunk> {
    Some(Chunk::Error {
        code: yunong_shared::ErrorCode::UpstreamError,
        message: error_message(error_object(value)?),
    })
}

fn error_object(value: &Value) -> Option<&Value> {
    if let Some(error) = value.get("error") {
        if error.is_object() || error.is_string() {
            return Some(error);
        }
    }
    // Responses 的失败事件把错误嵌在 response 里。
    if value.get("type").and_then(Value::as_str) == Some("response.failed") {
        if let Some(error) = value.get("response").and_then(|r| r.get("error")) {
            if error.is_object() || error.is_string() {
                return Some(error);
            }
        }
    }
    None
}

fn error_message(error: &Value) -> String {
    if let Some(text) = error.as_str() {
        if !text.trim().is_empty() {
            return text.to_owned();
        }
    }
    // 优先人类可读的 message，其次用 type/code 兜底（都不为空才用）。
    for key in ["message", "type", "code"] {
        if let Some(text) = error.get(key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return text.to_owned();
            }
        }
    }
    error.to_string()
}

/// 单测共用请求：一条 system + 一条 user + 一个工具。
#[cfg(test)]
fn fixture_request(protocol: Protocol) -> ChatRequest {
    ChatRequest {
        session_id: "s_1".into(),
        turn_id: "t_1".into(),
        round: 1,
        model: "deepseek-chat".into(),
        protocol,
        messages: vec![
            ChatMessage::system(FIXTURE_SYSTEM),
            ChatMessage::user("下周三下午有课吗"),
        ],
        tools: vec![yunong_shared::ToolSpec {
            name: "get_courses".into(),
            description: "查询课表".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "weekday": { "type": "integer" } },
                "required": ["weekday"],
            }),
        }],
        max_output_tokens: 512,
        stream: true,
        estimated_input_tokens: 100,
    }
}

/// 工具循环第 2 轮的请求：末尾追加 assistant(tool_call) 与 tool(tool_result)。
#[cfg(test)]
fn fixture_tool_round(protocol: Protocol) -> ChatRequest {
    let mut request = fixture_request(protocol);
    request.round = 2;
    request.messages.push(ChatMessage {
        role: Role::Assistant,
        content: r#"{"weekday":3}"#.into(),
        tool_call_id: Some("call_1".into()),
        name: Some("get_courses".into()),
    });
    request.messages.push(ChatMessage::tool_result(
        "call_1",
        "get_courses",
        r#"{"courses":[]}"#,
    ));
    request
}

#[cfg(test)]
const FIXTURE_SYSTEM: &str = "你是云农之窗助手";

#[cfg(test)]
const FIXTURE_MODEL: &str = "deepseek-chat";

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Protocol; 3] = [
        Protocol::OpenaiCompletions,
        Protocol::OpenaiResponses,
        Protocol::AnthropicMessages,
    ];

    /// 三协议对 `[DONE]` 的处理必须一致（契约要求：二选一但三协议一致）。
    #[test]
    fn done_marker_is_uniform_across_protocols() {
        for protocol in ALL {
            assert_eq!(
                parse_event(protocol, "[DONE]"),
                Some(Chunk::Done {
                    finish: FinishReason::Stop
                }),
                "{protocol:?} 的 [DONE] 处理与契约不一致"
            );
        }
    }

    /// 空串、纯文本心跳、非 JSON 一律不暴露给上层。
    #[test]
    fn blank_and_non_json_events_are_ignored() {
        for protocol in ALL {
            assert_eq!(parse_event(protocol, ""), None, "{protocol:?}");
            assert_eq!(parse_event(protocol, "   \n"), None, "{protocol:?}");
            assert_eq!(parse_event(protocol, ": keep-alive"), None, "{protocol:?}");
            assert_eq!(parse_event(protocol, "not-json"), None, "{protocol:?}");
        }
    }

    /// 顶层 `error` 对象 → `UpstreamError`，三协议行为一致。
    #[test]
    fn top_level_error_maps_to_upstream_error() {
        let data = r#"{"error":{"message":"upstream boom","type":"api_error"}}"#;
        for protocol in ALL {
            assert_eq!(
                parse_event(protocol, data),
                Some(Chunk::Error {
                    code: yunong_shared::ErrorCode::UpstreamError,
                    message: "upstream boom".into(),
                }),
                "{protocol:?}"
            );
        }
    }

    /// 结束原因映射：三家字面量各有不同，但语义同构。
    #[test]
    fn finish_reason_mapping_is_shared() {
        assert_eq!(finish_reason("stop"), FinishReason::Stop);
        assert_eq!(finish_reason("end_turn"), FinishReason::Stop);
        assert_eq!(finish_reason("length"), FinishReason::Length);
        assert_eq!(finish_reason("max_tokens"), FinishReason::Length);
        assert_eq!(finish_reason("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(finish_reason("tool_use"), FinishReason::ToolCalls);
        // §9：其他 → Stop
        assert_eq!(finish_reason("content_filter"), FinishReason::Stop);
        assert_eq!(finish_reason("pause_turn"), FinishReason::Stop);
    }

    /// 工具参数分片的三种降级形态（无状态函数的已知限制）。
    #[test]
    fn partial_json_degrades_in_three_ways() {
        assert_eq!(partial_json(""), Value::Object(Map::new()));
        assert_eq!(
            partial_json(r#"{"weekday":3}"#),
            serde_json::json!({"weekday":3})
        );
        assert_eq!(
            partial_json(r#"{"weekday":"#),
            Value::String(r#"{"weekday":"#.into())
        );
    }

    /// `build_body` 以参数 `protocol` 为准，且三种协议产出的顶层字段名互不相同。
    #[test]
    fn build_body_dispatches_on_protocol_argument() {
        let request = fixture_request(Protocol::OpenaiCompletions);
        // 端侧声明是 Completions，但调用方按路由覆盖成 Anthropic：不应 panic，按参数走。
        let body = build_body(Protocol::AnthropicMessages, &request);
        assert!(body.get("system").is_some());
        assert!(
            body.get("messages")
                .and_then(Value::as_array)
                .is_some_and(|m| m.iter().all(|msg| msg["role"] != "system")),
            "Anthropic 的 messages 里不能出现 system 角色"
        );
    }

    /// 模型名与流式开关三协议同名，但必须落在正确位置。
    #[test]
    fn model_and_stream_are_present() {
        for protocol in ALL {
            let body = build_body(protocol, &fixture_request(protocol));
            assert_eq!(body["model"], FIXTURE_MODEL, "{protocol:?}");
            assert_eq!(body["stream"], true, "{protocol:?}");
        }
    }
}
