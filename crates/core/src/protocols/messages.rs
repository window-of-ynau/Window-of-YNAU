//! Anthropic `/v1/messages`（`content_block_delta` 等流式事件）↔ 内部结构。
//!
//! 编码要点（§9）：
//! - 系统提示：顶层 `system`（字符串），messages 里只有 user/assistant
//! - 工具声明：`tools[]{name, description, input_schema}`（不是 `parameters`）
//! - 流式开关：`stream`；输出上限：`max_tokens`
//! - 工具调用：assistant 的 `tool_use` block；工具结果：user 的 `tool_result` block
//!
//! # 损失点（契约缺口的落点）
//!
//! - `tool_result.is_error` 表达不出来（`ChatMessage` 没有 `ok`）：失败只能靠文本。
//! - `tool_use.input` 必须是 **JSON 对象**，而 `ChatMessage.content` 是字符串：
//!   非法 JSON 时降级成 `{}`（原始文本丢弃）——这是 Anthropic 独有的限制，
//!   Completions/Responses 的 `arguments` 是字符串可以原样透传。
//! - assistant 的可见文本与 `tool_use` 不能共存（同上条的 content 复用）。
//! - Anthropic 要求 `tool_result` 必须紧跟对应的 `tool_use`：连续的工具结果会被
//!   合并进**同一条** user 消息（多个 block），否则上游会 400。
//! - `ChatRequest` 没有 `temperature`：不发送该字段。

use serde_json::{json, Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, Role, ToolSpec};

use super::{finish_reason, openai_usage, partial_json, system_prompt};

/// 构造请求体。
pub(super) fn build(request: &ChatRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(request.stream));
    // Anthropic 的 max_tokens 是必填字段，语义与 Completions 相同。
    body.insert("max_tokens".into(), json!(request.max_output_tokens));

    // 系统提示走顶层 system 字符串；多条 system 用空行拼接。
    if let Some(system) = system_prompt(&request.messages) {
        body.insert("system".into(), json!(system));
    }
    body.insert(
        "messages".into(),
        Value::Array(build_messages(&request.messages)),
    );

    if !request.tools.is_empty() {
        body.insert("tools".into(), encode_tools(&request.tools));
    }
    Value::Object(body)
}

/// user/assistant 消息数组。
///
/// 连续的工具结果合并成一条 user 消息的多个 `tool_result` block：Anthropic 要求
/// 所有结果紧跟 `tool_use`，逐条发成多个 user 消息虽然也能被上游合并，但语义上
/// "一条消息多个结果"才是正确形状。
fn build_messages(messages: &[ChatMessage]) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len());
    let mut results: Vec<Value> = Vec::new();
    for message in messages.iter().filter(|m| m.role != Role::System) {
        match message.role {
            Role::Tool => results.push(tool_result_block(message)),
            _ => {
                flush_results(&mut out, &mut results);
                out.push(encode_message(message));
            }
        }
    }
    flush_results(&mut out, &mut results);
    out
}

fn flush_results(out: &mut Vec<Value>, results: &mut Vec<Value>) {
    if !results.is_empty() {
        out.push(json!({
            "role": "user",
            "content": Value::Array(std::mem::take(results)),
        }));
    }
}

/// 一条 user/assistant 消息。`system` 已被 `build` 抽走。
fn encode_message(message: &ChatMessage) -> Value {
    match message.role {
        // 兜底：正常流程不会走到这里（Anthropic 的 messages 没有 system 角色）。
        Role::System => json!({ "role": "user", "content": message.content }),
        Role::User => json!({ "role": "user", "content": message.content }),
        Role::Assistant => encode_assistant(message),
        Role::Tool => {
            // 单条工具结果不会走到这里（`build_messages` 已收集），兜底成 user 消息。
            json!({ "role": "user", "content": [tool_result_block(message)] })
        }
    }
}

/// assistant：带 `tool_call_id` + `name` 视为工具调用（content 放 arguments JSON），
/// 否则当普通文本。
fn encode_assistant(message: &ChatMessage) -> Value {
    match (message.tool_call_id.as_deref(), message.name.as_deref()) {
        (Some(call_id), Some(name)) => json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": call_id,
                "name": name,
                // input 必须是对象：非法 JSON 只能降级成 {}（见模块注释）。
                "input": tool_input(&message.content),
            }],
        }),
        _ => json!({ "role": "assistant", "content": message.content }),
    }
}

/// 工具结果 block。
fn tool_result_block(message: &ChatMessage) -> Value {
    // tool_use_id 是必填；缺失时只能用空串（`validate` 应在前置阶段拦住，见交付说明）。
    let call_id = message.tool_call_id.as_deref().unwrap_or_default();
    json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": message.content,
    })
}

/// arguments 文本 → `input` 对象。
fn tool_input(raw: &str) -> Value {
    serde_json::from_str(raw.trim()).unwrap_or_else(|_| Value::Object(Map::new()))
}

/// 工具声明：`tools[]{name, description, input_schema}`。
fn encode_tools(tools: &[ToolSpec]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect(),
    )
}

/// 解析一条 Anthropic 流式事件。
///
/// # usage 的取舍（契约缺口）
///
/// Anthropic 把 usage **拆在两个事件**里：
/// - `message_start` 给 `usage.input_tokens`（还有 output_tokens，但那只是**占位值**，
///   真实输出计数在 `message_delta`）；
/// - `message_delta` 同时给 `delta.stop_reason` **和** `usage.output_tokens`。
///
/// 一次 `data` 只能返回一个 `Chunk`，`message_delta` 这里选择 **返回 `Done`**
/// （`stop_reason` 是结束原因的唯一来源，丢了它 `max_tokens` / `tool_use` 就都退化成
/// `Stop`），代价是真实的 `output_tokens` 被丢弃——调用方要么按文本长度估算
/// （`estimated = true`），要么在 `gateway` 层直接读原始 JSON 的 `usage`。
/// `message_start` 则返回 `Usage`（`input_tokens` 真实）。
///
/// # 工具调用分片
///
/// - `content_block_start`：`content_block.type = "tool_use"` 时给 `id` + `name`，
///   `input` 此时是空对象（占位）；
/// - `content_block_delta` 的 `input_json_delta.partial_json`：**只有参数片段**，
///   没有 id/name。本函数无状态，`id`/`name` 留空，`arguments` 返回片段
///   （`Value::String`）；调用方需按 `index` 关联到之前的 `content_block_start`。
pub(super) fn parse(value: &Value) -> Option<Chunk> {
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "message_start" => value
            .get("message")
            .and_then(|m| m.get("usage"))
            .and_then(openai_usage),
        "content_block_start" => tool_use_start(value),
        "content_block_delta" => content_block_delta(value),
        "message_delta" => {
            let raw = value
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)?;
            Some(Chunk::Done {
                finish: finish_reason(raw),
            })
        }
        // `error` 事件在 `parse_event` 里已由 upstream_error 拦下，这里兜底。
        "error" => super::upstream_error(value),
        // message_stop / ping / content_block_stop / 未知事件：没有新增信息
        // （§9 要求心跳 → None）。
        _ => None,
    }
}

fn tool_use_start(value: &Value) -> Option<Chunk> {
    let block = value.get("content_block")?;
    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
    let name = block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input = block
        .get("input")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    Some(Chunk::ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        // 起始帧的 input 是占位空对象；真实参数由 input_json_delta 下发。
        arguments: if input.is_object() {
            input
        } else {
            Value::Object(Map::new())
        },
    })
}

fn content_block_delta(value: &Value) -> Option<Chunk> {
    let delta = value.get("delta")?;
    match delta.get("type").and_then(Value::as_str)? {
        "text_delta" => {
            let text = delta.get("text").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            Some(Chunk::Delta {
                text: text.to_owned(),
            })
        }
        "input_json_delta" => {
            let fragment = delta
                .get("partial_json")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(Chunk::ToolCall {
                // id/name 只在 content_block_start 里，本事件拿不到（无状态限制）。
                id: String::new(),
                name: String::new(),
                arguments: partial_json(fragment),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::{fixture_request, fixture_tool_round, FIXTURE_SYSTEM};
    use yunong_shared::{FinishReason, Protocol};

    fn body() -> Value {
        build(&fixture_request(Protocol::AnthropicMessages))
    }

    // ---------- build_body ----------

    #[test]
    fn system_prompt_becomes_top_level_string() {
        let body = body();
        assert_eq!(body["system"], FIXTURE_SYSTEM);
        assert!(body["system"].is_string());
        // messages 里只能有 user/assistant。
        assert!(body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] == "user" || m["role"] == "assistant"));
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn tools_use_input_schema_not_parameters() {
        let body = body();
        let tool = &body["tools"][0];
        assert_eq!(tool["name"], "get_courses");
        assert_eq!(tool["description"], "查询课表");
        assert_eq!(tool["input_schema"]["type"], "object");
        assert!(tool.get("parameters").is_none());
        assert!(tool.get("function").is_none());
    }

    #[test]
    fn output_cap_uses_max_tokens_field() {
        let body = body();
        assert_eq!(body["max_tokens"], 512u32);
        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(body["stream"], true);
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn tool_round_encodes_tool_use_and_tool_result_blocks() {
        let body = build(&fixture_tool_round(Protocol::AnthropicMessages));
        let messages = body["messages"].as_array().unwrap();

        let assistant = &messages[1];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"][0]["type"], "tool_use");
        assert_eq!(assistant["content"][0]["id"], "call_1");
        assert_eq!(assistant["content"][0]["name"], "get_courses");
        assert_eq!(assistant["content"][0]["input"]["weekday"], 3);

        // 工具结果必须是紧跟其后的 user 消息里的 block（不能单独成 system 或 assistant）。
        let result = &messages[2];
        assert_eq!(result["role"], "user");
        assert_eq!(result["content"][0]["type"], "tool_result");
        assert_eq!(result["content"][0]["tool_use_id"], "call_1");
        assert_eq!(result["content"][0]["content"], r#"{"courses":[]}"#);
    }

    // ---------- parse_event ----------

    /// fixture 1：纯文本增量。
    #[test]
    fn text_delta_fixture() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"下午没课"}}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            Some(Chunk::Delta {
                text: "下午没课".into()
            })
        );
    }

    /// fixture 2：工具调用（start 给 id/name，delta 给参数片段）。
    #[test]
    fn tool_call_fixture() {
        let start = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_courses","input":{}}}"#;
        assert_eq!(
            parse(&serde_json::from_str(start).unwrap()),
            Some(Chunk::ToolCall {
                id: "toolu_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({}),
            })
        );

        let delta = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"weekday\":3}"}}"#;
        assert_eq!(
            parse(&serde_json::from_str(delta).unwrap()),
            Some(Chunk::ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: serde_json::json!({ "weekday": 3 }),
            })
        );
    }

    /// fixture 2b：不完整分片原样给调用方拼接（无状态限制）。
    #[test]
    fn argument_fragments_stay_raw() {
        let data = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"weekday\":"}}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            Some(Chunk::ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: Value::String(r#"{"weekday":"#.into()),
            })
        );
    }

    /// fixture 3：usage 在 `message_start`（input_tokens 真实，output_tokens 是占位值）。
    #[test]
    fn usage_fixture() {
        let data = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet","usage":{"input_tokens":42,"output_tokens":1}}}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            Some(Chunk::Usage {
                tokens_in: 42,
                tokens_out: 1,
                estimated: false,
            })
        );
    }

    /// fixture 4：结束原因（`message_delta.stop_reason`，usage 在此丢弃，见 `parse` 注释）。
    #[test]
    fn finish_reason_fixture() {
        let cases = [
            ("tool_use", FinishReason::ToolCalls),
            ("end_turn", FinishReason::Stop),
            ("max_tokens", FinishReason::Length),
            ("stop_sequence", FinishReason::Stop),
        ];
        for (raw, expected) in cases {
            let data = format!(
                r#"{{"type":"message_delta","delta":{{"stop_reason":"{raw}","stop_sequence":null}},"usage":{{"output_tokens":15}}}}"#
            );
            assert_eq!(
                parse(&serde_json::from_str(&data).unwrap()),
                Some(Chunk::Done { finish: expected }),
                "stop_reason={raw} 映射错误"
            );
        }
    }

    /// fixture 5a：心跳与收尾事件 → None（§9 要求心跳不暴露给上层）。
    #[test]
    fn irrelevant_events_are_ignored() {
        for data in [
            r#"{"type":"ping"}"#,
            r#"{"type":"message_stop"}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ] {
            assert_eq!(parse(&serde_json::from_str(data).unwrap()), None, "{data}");
        }
    }

    /// fixture 5b：`error` 事件 → `Chunk::Error(UpstreamError)`。
    #[test]
    fn error_event_fixture() {
        let data = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(
            crate::protocols::parse_event(Protocol::AnthropicMessages, data),
            Some(Chunk::Error {
                code: yunong_shared::ErrorCode::UpstreamError,
                message: "Overloaded".into(),
            })
        );
    }
}
