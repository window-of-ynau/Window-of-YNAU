//! OpenAI `/v1/chat/completions`（`chat.completion.chunk` 流式事件）↔ 内部结构。
//!
//! 编码要点（§9）：
//! - 系统提示：`messages[0] = {"role":"system","content":..}`
//! - 工具声明：`tools[].function = {name, description, parameters}`
//! - 流式开关：`stream`；输出上限：`max_tokens`
//! - 工具调用：assistant 消息的 `tool_calls[]`；工具结果：`role = "tool"` + `tool_call_id`
//!
//! # 损失点（契约缺口的落点）
//!
//! - `ChatMessage` 没有 `tool_calls`，assistant 工具调用按模块注释第 1 条的约定拼装：
//!   `content` 当 **arguments 的 JSON 文本**原样塞进 `function.arguments`。
//!   因此 **assistant 的可见文本与工具调用不能共存**；两者都有时只能二选一
//!   （当前实现优先工具调用，文本被丢弃）。
//! - OpenAI 在 2023-12 之后从 `role:"tool"` 消息里移除了 `name` 字段，
//!   所以工具名（`ChatMessage::name`）在工具结果里**丢失**；`tool_call_id` 保留。
//! - `ChatRequest` 没有 `temperature`：不发送该字段（上游默认值）。

use serde_json::{json, Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, Role, ToolSpec};

use super::{finish_reason, openai_usage, partial_json, system_prompt};

/// 构造请求体。
pub(super) fn build(request: &ChatRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(request.stream));
    // 三家字段名不同：Completions 用 max_tokens（Responses 才是 max_output_tokens）。
    body.insert("max_tokens".into(), json!(request.max_output_tokens));

    // 系统提示必须落在 messages[0]：先抽出来放最前，再按原顺序拼其余消息。
    let mut messages = Vec::with_capacity(request.messages.len());
    if let Some(system) = system_prompt(&request.messages) {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for message in request.messages.iter().filter(|m| m.role != Role::System) {
        messages.push(encode_message(message));
    }
    body.insert("messages".into(), Value::Array(messages));

    if !request.tools.is_empty() {
        body.insert("tools".into(), encode_tools(&request.tools));
    }
    Value::Object(body)
}

/// 一条消息。`system` 已在 `build` 里抽走，这里不会出现。
fn encode_message(message: &ChatMessage) -> Value {
    match message.role {
        // 兜底：正常流程不会走到这里（`build` 已过滤）。
        Role::System => json!({ "role": "system", "content": message.content }),
        Role::User => json!({ "role": "user", "content": message.content }),
        Role::Assistant => encode_assistant(message),
        Role::Tool => encode_tool_result(message),
    }
}

/// assistant：带 `tool_call_id` + `name` 视为工具调用，否则当普通文本。
fn encode_assistant(message: &ChatMessage) -> Value {
    match (message.tool_call_id.as_deref(), message.name.as_deref()) {
        (Some(call_id), Some(name)) => json!({
            "role": "assistant",
            // 有 tool_calls 时 content 必须为 null（否则上游会认为是文本 + 调用，
            // 而我们的契约里 content 已经被 arguments 占用了）。
            "content": Value::Null,
            "tool_calls": [{
                "id": call_id,
                "type": "function",
                "function": {
                    "name": name,
                    // 约定见模块注释：content 是 arguments 的 JSON 文本，原样透传。
                    "arguments": message.content,
                },
            }],
        }),
        // `tool_call_id`/`name` 缺一个就无法构成合法的 tool_calls，降级成文本消息。
        // 这是"最小可用编码"，损失的是调用语义（见模块注释）。
        _ => json!({ "role": "assistant", "content": message.content }),
    }
}

/// 工具结果 → `role:"tool"`。缺 `tool_call_id` 时降级成 user 文本，
/// 避免给上游发一条必然 400 的消息。
fn encode_tool_result(message: &ChatMessage) -> Value {
    match message.tool_call_id.as_deref() {
        Some(call_id) => json!({
            "role": "tool",
            "tool_call_id": call_id,
            // OpenAI 的 tool 消息不接受 name（已废弃/移除），工具名在此丢失。
            "content": message.content,
        }),
        None => {
            let name = message.name.as_deref().unwrap_or("tool");
            json!({
                "role": "user",
                "content": format!("[工具 {name} 的结果]\n{}", message.content),
            })
        }
    }
}

/// 工具声明：`tools[].function{name,description,parameters}`。
fn encode_tools(tools: &[ToolSpec]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                })
            })
            .collect(),
    )
}

/// 解析一条 `chat.completion.chunk`。
///
/// 顺序说明：Completions 的 `finish_reason` 和 `usage` 在**不同** chunk 里
/// （`stream_options.include_usage` 时 usage 是最后一个独立 chunk，之后还有 `[DONE]`），
/// 所以这里先看 `finish_reason` 再看 `usage`，两者都能正确落到不同的 `Chunk` 上。
///
/// 工具调用分片聚合：OpenAI 把一次调用拆成多个 chunk——
/// 首帧给 `index` + `id` + `function.name`（`arguments` 常为空串），
/// 后续帧只给 `index` + `function.arguments` 的**字符串片段**。
/// 本函数无状态，只返回当前帧：
/// - `id` / `name` 缺失时给空串（后续帧本来就不带）；
/// - `arguments` 完整 → 解析成 JSON；不完整 → `Value::String(片段)`，调用方须按
///   `tool_calls[0].index` 累加字符串，等 `finish_reason == "tool_calls"` 后整体解析。
pub(super) fn parse(value: &Value) -> Option<Chunk> {
    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        for choice in choices {
            if let Some(raw) = choice.get("finish_reason").and_then(Value::as_str) {
                return Some(Chunk::Done {
                    finish: finish_reason(raw),
                });
            }
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    return Some(Chunk::Delta {
                        text: text.to_owned(),
                    });
                }
            }
            if let Some(call) = first_tool_call(delta) {
                return Some(call);
            }
        }
    }
    // usage chunk：`choices: []` + `usage: {...}`（`usage` 为 null 时忽略）。
    value.get("usage").and_then(openai_usage)
}

fn first_tool_call(delta: &Value) -> Option<Chunk> {
    let call = delta.get("tool_calls")?.as_array()?.first()?;
    Some(tool_call(call))
}

fn tool_call(call: &Value) -> Chunk {
    let function = call.get("function");
    let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
    let name = function
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = function
        .and_then(|f| f.get("arguments"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Chunk::ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: partial_json(arguments),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::{fixture_request, fixture_tool_round, FIXTURE_SYSTEM};
    use yunong_shared::{FinishReason, Protocol};

    fn body() -> Value {
        build(&fixture_request(Protocol::OpenaiCompletions))
    }

    // ---------- build_body ----------

    #[test]
    fn system_prompt_goes_to_messages_head() {
        let body = body();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], FIXTURE_SYSTEM);
        // 其余消息里不能再出现 system（抽走而非复制）。
        assert!(body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .all(|m| m["role"] != "system"));
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn tools_are_wrapped_in_function_object() {
        let body = body();
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "get_courses");
        assert_eq!(body["tools"][0]["function"]["description"], "查询课表");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
        // 工具名不能出现在 function 层之外。
        assert!(body["tools"][0].get("name").is_none());
    }

    #[test]
    fn output_cap_uses_max_tokens_field() {
        let body = body();
        assert_eq!(body["max_tokens"], 512u32);
        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(body["stream"], true);
        // temperature 未提供（契约里根本没有该字段）→ 不能凭空造一个。
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn tool_round_encodes_tool_calls_and_tool_role() {
        let body = build(&fixture_tool_round(Protocol::OpenaiCompletions));
        let messages = body["messages"].as_array().unwrap();
        let assistant = &messages[2];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], Value::Null);
        assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
        assert_eq!(assistant["tool_calls"][0]["type"], "function");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            "get_courses"
        );
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            r#"{"weekday":3}"#
        );

        let result = &messages[3];
        assert_eq!(result["role"], "tool");
        assert_eq!(result["tool_call_id"], "call_1");
        assert_eq!(result["content"], r#"{"courses":[]}"#);
    }

    // ---------- parse_event ----------

    /// fixture 1：纯文本增量。
    #[test]
    fn text_delta_fixture() {
        let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"deepseek-chat","choices":[{"index":0,"delta":{"content":"下午没课"},"finish_reason":null}]}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            Some(Chunk::Delta {
                text: "下午没课".into()
            })
        );
    }

    /// fixture 2：工具调用（首帧带 id/name，参数完整时可解析）。
    #[test]
    fn tool_call_fixture() {
        let data = r#"{"id":"chatcmpl-2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_courses","arguments":"{\"weekday\":3}"}}]},"finish_reason":null}]}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            Some(Chunk::ToolCall {
                id: "call_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({ "weekday": 3 }),
            })
        );
    }

    /// fixture 2b：参数分片是无状态函数的已知限制——片段原样保留给调用方拼接。
    #[test]
    fn tool_call_argument_fragments_stay_raw() {
        let first = r#"{"id":"chatcmpl-2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_courses","arguments":""}}]},"finish_reason":null}]}"#;
        assert_eq!(
            parse(&serde_json::from_str(first).unwrap()),
            Some(Chunk::ToolCall {
                id: "call_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({}),
            })
        );

        let fragment = r#"{"id":"chatcmpl-2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"weekday\":"}}]},"finish_reason":null}]}"#;
        assert_eq!(
            parse(&serde_json::from_str(fragment).unwrap()),
            Some(Chunk::ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: Value::String(r#"{"weekday":"#.into()),
            })
        );
    }

    /// fixture 3：usage（`choices: []` 的独立 chunk）。
    #[test]
    fn usage_fixture() {
        let data = r#"{"id":"chatcmpl-3","choices":[],"usage":{"prompt_tokens":42,"completion_tokens":7,"total_tokens":49}}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            Some(Chunk::Usage {
                tokens_in: 42,
                tokens_out: 7,
                estimated: false,
            })
        );
    }

    /// fixture 4：结束原因。
    #[test]
    fn finish_reason_fixture() {
        let tool_calls = r#"{"id":"chatcmpl-4","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        assert_eq!(
            parse(&serde_json::from_str(tool_calls).unwrap()),
            Some(Chunk::Done {
                finish: FinishReason::ToolCalls
            })
        );

        let length =
            r#"{"id":"chatcmpl-4","choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#;
        assert_eq!(
            parse(&serde_json::from_str(length).unwrap()),
            Some(Chunk::Done {
                finish: FinishReason::Length
            })
        );

        assert_eq!(
            crate::protocols::parse_event(Protocol::OpenaiCompletions, "[DONE]"),
            Some(Chunk::Done {
                finish: FinishReason::Stop
            })
        );
    }

    /// fixture 5a：无关事件（角色首帧 / 空 choices）→ None。
    #[test]
    fn irrelevant_events_are_ignored() {
        let role_frame = r#"{"id":"chatcmpl-5","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#;
        assert_eq!(parse(&serde_json::from_str(role_frame).unwrap()), None);

        let empty = r#"{"id":"chatcmpl-5","choices":[]}"#;
        assert_eq!(parse(&serde_json::from_str(empty).unwrap()), None);

        let null_usage = r#"{"id":"chatcmpl-5","choices":[],"usage":null}"#;
        assert_eq!(parse(&serde_json::from_str(null_usage).unwrap()), None);
    }

    /// fixture 5b：上游错误对象 → `Chunk::Error(UpstreamError)`。
    #[test]
    fn error_object_fixture() {
        let data =
            r#"{"error":{"message":"upstream is overloaded","type":"server_error","code":null}}"#;
        assert_eq!(
            crate::protocols::parse_event(Protocol::OpenaiCompletions, data),
            Some(Chunk::Error {
                code: yunong_shared::ErrorCode::UpstreamError,
                message: "upstream is overloaded".into(),
            })
        );
    }
}
