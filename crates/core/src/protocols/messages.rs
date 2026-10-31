//! Anthropic `/v1/messages`（`content_block_delta` 等流式事件）↔ 内部结构。
//!
//! 编码要点（§9）：
//! - 系统提示：顶层 `system`（字符串），messages 里只有 user/assistant
//! - 工具声明：`tools[]{name, description, input_schema}`（不是 `parameters`）
//! - 流式开关：`stream`；输出上限：`max_tokens`
//! - 工具调用：assistant 的 `tool_use` block；工具结果：user 的 `tool_result` block
//!
//! # 契约落点（`docs/design/contract-gaps.md`）
//!
//! - **C1 已解决（ADR-16）**：assistant 的可见文本编成 `text` block、工具调用编成
//!   `tool_use` block，两者可以共存于同一条消息的 `content` 数组（数组化只在有调用时发生，
//!   纯文本仍是字符串）。并行调用 = 多个 `tool_use` block。
//! - `tool_result.is_error` 表达不出来（`ChatMessage` 没有 `ok`）：失败只能靠文本（缺口 C3）。
//! - `tool_use.input` 必须是 **JSON 对象**（缺口 C5）：`Value::String`（未聚合完的片段）
//!   没有位置可放，只能降级成 `{}`——这是 Anthropic 独有的限制，
//!   Completions / Responses 的 `arguments` 是字符串可以原样透传。
//! - Anthropic 要求 `tool_result` 必须紧跟对应的 `tool_use`：连续的工具结果会被
//!   合并进**同一条** user 消息（多个 block），否则上游会 400。
//! - `ChatRequest` 没有 `temperature`：不发送该字段（缺口 C2）。

use serde_json::{json, Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, Role, ToolSpec};

use super::{call_index, finish_reason, openai_usage, partial_json, system_prompt};

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

/// assistant：可见文本与工具调用**可以共存**（C1）。
///
/// 无工具调用时保持最简形状（`content` 是字符串）；有调用时 `content` 变成 block 数组：
/// 有文本才发 `text` block，随后每个调用一个 `tool_use` block。
fn encode_assistant(message: &ChatMessage) -> Value {
    if message.tool_calls.is_empty() {
        return json!({ "role": "assistant", "content": message.content });
    }
    let mut blocks = Vec::with_capacity(message.tool_calls.len() + 1);
    if !message.content.is_empty() {
        blocks.push(json!({ "type": "text", "text": message.content }));
    }
    for call in &message.tool_calls {
        blocks.push(json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            // input 必须是对象：未聚合完的片段只能降级成 {}（见模块注释）。
            "input": tool_input(&call.arguments),
        }));
    }
    json!({ "role": "assistant", "content": blocks })
}

/// 工具结果 block。
fn tool_result_block(message: &ChatMessage) -> Value {
    // tool_use_id 是必填；缺失时只能用空串（`ChatRequest::validate` 会在发请求前拦住）。
    let call_id = message.tool_call_id.as_deref().unwrap_or_default();
    json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": message.content,
    })
}

/// `arguments` → `input` 对象。Anthropic 只接受 JSON 对象（C5）：
/// 未聚合完的 `Value::String` 片段在这里没有位置，只能降级成 `{}`。
fn tool_input(arguments: &Value) -> Value {
    match arguments {
        Value::Object(_) => arguments.clone(),
        _ => Value::Object(Map::new()),
    }
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

/// 解析一条 Anthropic 流式事件 → **0..n 个** `Chunk`（缺口 C6）。
///
/// # usage 与结束原因同事件（C6 的核心案例）
///
/// Anthropic 把 usage 拆在两个事件里：
/// - `message_start` 给 `usage.input_tokens`（那里的 `output_tokens` 只是**占位值**，
///   真实输出计数在 `message_delta`）；
/// - `message_delta` **同时**给 `delta.stop_reason` 和 `usage.output_tokens`。
///
/// 旧实现只能二选一：要么丢结束原因（`max_tokens` / `tool_use` 退化成 `Stop`），
/// 要么丢真实输出 token 数（成本监控失真）。现在返回 `[Usage, Done]`——
/// `Done` 必末（§10），所以 `Usage` 排在它前面。合并口径见 `yunong_shared::Usage`。
///
/// # 工具调用分片
///
/// - `content_block_start`：`content_block.type = "tool_use"` 时给 `id` + `name`，
///   `input` 此时是空对象（占位）；
/// - `content_block_delta` 的 `input_json_delta.partial_json`：**只有参数片段**，
///   没有 id/name。本函数无状态，`id`/`name` 留空，`arguments` 返回片段
///   （`Value::String`）；调用方需按 `index` 关联到之前的 `content_block_start`。
pub(super) fn parse(value: &Value) -> Vec<Chunk> {
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "message_start" => option_chunk(
            value
                .get("message")
                .and_then(|m| m.get("usage"))
                .and_then(openai_usage),
        ),
        "content_block_start" => option_chunk(tool_use_start(value)),
        "content_block_delta" => option_chunk(content_block_delta(value)),
        "message_delta" => message_delta(value),
        // `error` 事件在 `parse_events` 里已由 upstream_error 拦下，这里兜底。
        "error" => option_chunk(super::upstream_error(value)),
        // message_stop / ping / content_block_stop / 未知事件：没有新增信息
        // （§9 要求心跳 → 空 Vec）。
        _ => Vec::new(),
    }
}

/// `message_delta`：结束原因与 usage 在**同一个事件**里，两个都要给（`[Usage, Done]`）。
fn message_delta(value: &Value) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    if let Some(usage) = value.get("usage").and_then(openai_usage) {
        chunks.push(usage);
    }
    if let Some(raw) = value
        .get("delta")
        .and_then(|d| d.get("stop_reason"))
        .and_then(Value::as_str)
    {
        chunks.push(Chunk::Done {
            finish: finish_reason(raw),
        });
    }
    super::done_last(chunks)
}

/// 最多产出一个 chunk 的分支：收成 `Vec`，免得每处都写 `Option` 包装。
fn option_chunk(chunk: Option<Chunk>) -> Vec<Chunk> {
    chunk.into_iter().collect()
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
        index: call_index(value, "index"),
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
                // 分片自身带 `index`：并行调用靠它归位（C9）。
                index: call_index(value, "index"),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::{
        fixture_request, fixture_text_and_tool_round, fixture_tool_round, FIXTURE_SYSTEM,
    };
    use yunong_shared::{FinishReason, Protocol, ToolCallRef};

    fn body() -> Value {
        build(&fixture_request(Protocol::AnthropicMessages))
    }

    /// 单 chunk 事件收成 `Option`；顺带断言"这条事件确实只产出一个 chunk"。
    fn parse_one(data: &str) -> Option<Chunk> {
        let mut chunks = parse(&serde_json::from_str(data).unwrap());
        assert!(chunks.len() <= 1, "该事件应只有一个 chunk：{chunks:?}");
        chunks.pop()
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

    /// C1：assistant「边说边调工具」→ `content` 数组里 `text` block + `tool_use` block 共存。
    #[test]
    fn text_and_tool_use_blocks_coexist() {
        let body = build(&fixture_text_and_tool_round(Protocol::AnthropicMessages));
        let assistant = &body["messages"].as_array().unwrap()[1];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"][0]["type"], "text");
        assert_eq!(assistant["content"][0]["text"], "我先查一下课表");
        assert_eq!(assistant["content"][1]["type"], "tool_use");
        assert_eq!(assistant["content"][1]["id"], "call_1");
        assert_eq!(assistant["content"][1]["input"]["weekday"], 3);
    }

    /// C1：并行多调用 = 多个 `tool_use` block（文本 + 两个调用）。
    #[test]
    fn parallel_tool_use_blocks() {
        let mut request = fixture_request(Protocol::AnthropicMessages);
        request.messages.push(ChatMessage::assistant_calls(
            "稍等",
            vec![
                ToolCallRef::new("call_1", "get_courses", serde_json::json!({ "weekday": 3 })),
                ToolCallRef::new("call_2", "get_calendar", serde_json::json!({})),
            ],
        ));
        let body = build(&request);
        let messages = body["messages"].as_array().unwrap();
        // 没有 tool_result 收尾，assistant 就是最后一条。
        let assistant = messages.last().unwrap();
        let blocks = assistant["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert_eq!(blocks[0]["type"], "text");
        assert!(blocks[1..].iter().all(|b| b["type"] == "tool_use"));
        assert_eq!(blocks[2]["name"], "get_calendar");
    }

    // ---------- parse_events ----------

    /// fixture 1：纯文本增量。
    #[test]
    fn text_delta_fixture() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"下午没课"}}"#;
        assert_eq!(
            parse_one(data),
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
            parse_one(start),
            Some(Chunk::ToolCall {
                id: "toolu_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({}),
                index: Some(1),
            })
        );

        let delta = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"weekday\":3}"}}"#;
        assert_eq!(
            parse_one(delta),
            Some(Chunk::ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: serde_json::json!({ "weekday": 3 }),
                index: Some(1),
            })
        );
    }

    /// fixture 2b：不完整分片原样给调用方拼接（无状态限制）。
    #[test]
    fn argument_fragments_stay_raw() {
        let data = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"weekday\":"}}"#;
        assert_eq!(
            parse_one(data),
            Some(Chunk::ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: Value::String(r#"{"weekday":"#.into()),
                index: Some(1),
            })
        );
    }

    /// fixture 3：usage 在 `message_start`（input_tokens 真实，output_tokens 是占位值）。
    #[test]
    fn usage_fixture() {
        let data = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet","usage":{"input_tokens":42,"output_tokens":1}}}"#;
        assert_eq!(
            parse_one(data),
            Some(Chunk::Usage {
                tokens_in: 42,
                tokens_out: 1,
                estimated: false,
            })
        );
    }

    /// C6 fixture：`message_delta` 同时带 `stop_reason` 与 `usage.output_tokens`。
    ///
    /// 旧实现只能二选一（丢 token 数或丢结束原因）；现在两个都给，且 `Done` 在最后。
    #[test]
    fn finish_reason_and_usage_in_one_event() {
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
                vec![
                    Chunk::Usage {
                        tokens_in: 0,
                        tokens_out: 15,
                        estimated: false,
                    },
                    Chunk::Done { finish: expected },
                ],
                "stop_reason={raw} 映射错误"
            );
        }
    }

    /// fixture 5a：心跳与收尾事件 → 空 `Vec`（§9 要求心跳不暴露给上层）。
    #[test]
    fn irrelevant_events_are_ignored() {
        for data in [
            r#"{"type":"ping"}"#,
            r#"{"type":"message_stop"}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ] {
            assert_eq!(parse_one(data), None, "{data}");
        }
    }

    /// fixture 5b：`error` 事件 → `Chunk::Error(UpstreamError)`。
    #[test]
    fn error_event_fixture() {
        let data = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(
            crate::protocols::parse_events(Protocol::AnthropicMessages, data),
            vec![Chunk::Error {
                code: yunong_shared::ErrorCode::UpstreamError,
                message: "Overloaded".into(),
            }]
        );
    }
}
