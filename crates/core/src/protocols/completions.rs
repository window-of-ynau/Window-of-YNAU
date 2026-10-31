//! OpenAI `/v1/chat/completions`（`chat.completion.chunk` 流式事件）↔ 内部结构。
//!
//! 编码要点（§9）：
//! - 系统提示：`messages[0] = {"role":"system","content":..}`
//! - 工具声明：`tools[].function = {name, description, parameters}`
//! - 流式开关：`stream`；输出上限：`max_tokens`
//! - 工具调用：assistant 消息的 `tool_calls[]`；工具结果：`role = "tool"` + `tool_call_id`
//!
//! # 契约落点（`docs/design/contract-gaps.md`）
//!
//! - **C1 已解决（ADR-16）**：assistant 的可见文本与 `tool_calls[]` 可以**同时**出现；
//!   有调用且文本为空时 `content` 编成 `null`（OpenAI 的推荐形状）。
//!   并行多调用就是 `tool_calls[]` 里的多个元素，不再需要"二选一"降级。
//! - OpenAI 在 2023-12 之后从 `role:"tool"` 消息里移除了 `name` 字段，
//!   所以工具名（`ChatMessage::name`）在工具结果里**丢失**；`tool_call_id` 保留（缺口 C4）。
//! - `ChatRequest` 没有 `temperature`：不发送该字段（上游默认值，缺口 C2）。
//! - 参数分片无状态（缺口 C7）：完整 JSON 才解析；不完整片段原样给调用方拼接。

use serde_json::{json, Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, Role, ToolCallRef, ToolSpec};

use super::{call_index, finish_reason, openai_usage, partial_json, system_prompt, BuildOptions};

/// 构造请求体（默认开关，测试与普通调用方用）。
pub(super) fn build(request: &ChatRequest) -> Value {
    build_with(request, &BuildOptions::default())
}

/// 构造请求体。
///
/// `options.report_usage` 决定是否带 `stream_options: {"include_usage": true}`：
/// OpenAI 只在打开它时才在流里回报 `usage`，不开就只能靠端侧估算。
pub(super) fn build_with(request: &ChatRequest, options: &BuildOptions) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(request.stream));
    // 三家字段名不同：Completions 用 max_tokens（Responses 才是 max_output_tokens）。
    body.insert("max_tokens".into(), json!(request.max_output_tokens));
    if options.report_usage {
        // 只有 OpenAI 需要在流外声明"请在流里回报 usage"；Anthropic/Responses 默认就给。
        body.insert("stream_options".into(), json!({ "include_usage": true }));
    }

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

/// assistant：可见文本与工具调用**可以共存**（C1）；`content` 不再兼作 arguments 载体。
///
/// - 无 `tool_calls` → 普通文本消息（`content` 永远是字符串）；
/// - 有 `tool_calls` → `content` 为文本，或文本为空时是 `null`。
fn encode_assistant(message: &ChatMessage) -> Value {
    if message.tool_calls.is_empty() {
        return json!({ "role": "assistant", "content": message.content });
    }
    let content = if message.content.is_empty() {
        Value::Null
    } else {
        json!(message.content)
    };
    json!({
        "role": "assistant",
        "content": content,
        "tool_calls": encode_tool_calls(&message.tool_calls),
    })
}

/// `tool_calls[]`：`function.arguments` 是 **JSON 字符串**（不是对象）。
fn encode_tool_calls(calls: &[ToolCallRef]) -> Value {
    Value::Array(calls.iter().map(tool_call_json).collect())
}

fn tool_call_json(call: &ToolCallRef) -> Value {
    json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": call.name,
            // C5：`arguments` 永远是合法 JSON 值；只有"未聚合完的片段"是 `Value::String`。
            // 那种情况**原样转发**（不加引号包一层），让上游明确报参数非法，
            // 而不是静默替换成 `{}` 让模型以为参数是空的。
            "arguments": match &call.arguments {
                Value::String(raw) => raw.clone(),
                value => value.to_string(),
            },
        },
    })
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

/// 解析一条 `chat.completion.chunk` → **0..n 个** `Chunk`（缺口 C6）。
///
/// 一个事件里可能同时有：文本增量、**多个**工具调用（并行调用）、`finish_reason`、`usage`。
/// 产出顺序按上游字段顺序：先 `Delta`，再各 `ToolCall`；`Usage` 排在 `Done` 之前
/// （`Done` 必末且唯一，排序见 `super::done_last`）。
///
/// 工具调用分片聚合：OpenAI 把一次调用拆成多个 chunk——
/// 首帧给 `index` + `id` + `function.name`（`arguments` 常为空串），
/// 后续帧只给 `index` + `function.arguments` 的**字符串片段**。
/// 本函数无状态，只返回当前帧：
/// - `id` / `name` 缺失时给空串（后续帧本来就不带）；
/// - `arguments` 完整 → 解析成 JSON；不完整 → `Value::String(片段)`，调用方须按
///   `tool_calls[i].index` 累加字符串，等 `finish_reason == "tool_calls"` 后整体解析。
pub(super) fn parse(value: &Value) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        for choice in choices {
            if let Some(delta) = choice.get("delta") {
                if let Some(text) = delta.get("content").and_then(Value::as_str) {
                    if !text.is_empty() {
                        chunks.push(Chunk::Delta {
                            text: text.to_owned(),
                        });
                    }
                }
                // 并行调用：一个 delta 里可以有多个 `tool_calls` 元素，全部给出去。
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    chunks.extend(calls.iter().map(tool_call));
                }
            }
            if let Some(raw) = choice.get("finish_reason").and_then(Value::as_str) {
                chunks.push(Chunk::Done {
                    finish: finish_reason(raw),
                });
            }
        }
    }
    // usage 可能是独立的末帧（`choices: []`），也可能和 `finish_reason` 挤在同一个 chunk。
    if let Some(usage) = value.get("usage").and_then(openai_usage) {
        chunks.push(usage);
    }
    super::done_last(chunks)
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
        index: call_index(call, "index"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::{
        fixture_request, fixture_text_and_tool_round, fixture_tool_round, FIXTURE_SYSTEM,
    };
    use yunong_shared::{FinishReason, Protocol};

    fn body() -> Value {
        build(&fixture_request(Protocol::OpenaiCompletions))
    }

    /// 单 chunk 事件收成 `Option`；顺带断言"这条事件确实只产出一个 chunk"。
    fn parse_one(data: &str) -> Option<Chunk> {
        let mut chunks = parse(&serde_json::from_str(data).unwrap());
        assert!(chunks.len() <= 1, "该事件应只有一个 chunk：{chunks:?}");
        chunks.pop()
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

    // ---------- parse_events ----------

    /// fixture 1：纯文本增量。
    #[test]
    fn text_delta_fixture() {
        let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"deepseek-chat","choices":[{"index":0,"delta":{"content":"下午没课"},"finish_reason":null}]}"#;
        assert_eq!(
            parse_one(data),
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
            parse_one(data),
            Some(Chunk::ToolCall {
                id: "call_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({ "weekday": 3 }),
                index: Some(0),
            })
        );
    }

    /// fixture 2b：参数分片是无状态函数的已知限制——片段原样保留给调用方拼接。
    #[test]
    fn tool_call_argument_fragments_stay_raw() {
        let first = r#"{"id":"chatcmpl-2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_courses","arguments":""}}]},"finish_reason":null}]}"#;
        assert_eq!(
            parse_one(first),
            Some(Chunk::ToolCall {
                id: "call_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({}),
                index: Some(0),
            })
        );

        let fragment = r#"{"id":"chatcmpl-2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"weekday\":"}}]},"finish_reason":null}]}"#;
        assert_eq!(
            parse_one(fragment),
            Some(Chunk::ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: Value::String(r#"{"weekday":"#.into()),
                index: Some(0),
            })
        );
    }

    /// fixture 3：usage（`choices: []` 的独立 chunk）。
    #[test]
    fn usage_fixture() {
        let data = r#"{"id":"chatcmpl-3","choices":[],"usage":{"prompt_tokens":42,"completion_tokens":7,"total_tokens":49}}"#;
        assert_eq!(
            parse_one(data),
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
            parse_one(tool_calls),
            Some(Chunk::Done {
                finish: FinishReason::ToolCalls
            })
        );

        let length =
            r#"{"id":"chatcmpl-4","choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#;
        assert_eq!(
            parse_one(length),
            Some(Chunk::Done {
                finish: FinishReason::Length
            })
        );

        assert_eq!(
            crate::protocols::parse_events(Protocol::OpenaiCompletions, "[DONE]"),
            vec![Chunk::Done {
                finish: FinishReason::Stop
            }]
        );
    }

    /// C1：assistant「边说边调工具」——文本与工具调用同轮编码（旧实现二选一，文本会被丢）。
    #[test]
    fn text_and_tool_calls_encode_together() {
        let body = build(&fixture_text_and_tool_round(Protocol::OpenaiCompletions));
        let assistant = &body["messages"].as_array().unwrap()[2];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "我先查一下课表");
        assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            r#"{"weekday":3}"#
        );
    }

    /// C1：并行多工具调用 → `tool_calls[]` 多个元素（旧契约无法表达）。
    #[test]
    fn parallel_tool_calls_encode_as_array() {
        let mut request = fixture_request(Protocol::OpenaiCompletions);
        request.messages.push(ChatMessage::assistant_calls(
            "",
            vec![
                ToolCallRef::new("call_1", "get_courses", serde_json::json!({ "weekday": 3 })),
                ToolCallRef::new("call_2", "get_calendar", serde_json::json!({})),
            ],
        ));
        let body = build(&request);
        let assistant = body["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(assistant["content"], Value::Null);
        assert_eq!(assistant["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(
            assistant["tool_calls"][1]["function"]["name"],
            "get_calendar"
        );
    }

    /// fixture 5a：无关事件（角色首帧 / 空 choices）→ None。
    #[test]
    fn irrelevant_events_are_ignored() {
        let role_frame = r#"{"id":"chatcmpl-5","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#;
        assert_eq!(parse_one(role_frame), None);

        let empty = r#"{"id":"chatcmpl-5","choices":[]}"#;
        assert_eq!(parse_one(empty), None);

        let null_usage = r#"{"id":"chatcmpl-5","choices":[],"usage":null}"#;
        assert_eq!(parse_one(null_usage), None);
    }

    /// C6 fixture：**一个** chunk 同时带文本增量与工具调用 → `[Delta, ToolCall]`。
    #[test]
    fn text_and_tool_call_in_one_event() {
        let data = r#"{"id":"chatcmpl-6","choices":[{"index":0,"delta":{"content":"我先查课表","tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_courses","arguments":"{\"weekday\":3}"}}]},"finish_reason":null}]}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            vec![
                Chunk::Delta {
                    text: "我先查课表".into()
                },
                Chunk::ToolCall {
                    id: "call_1".into(),
                    name: "get_courses".into(),
                    arguments: serde_json::json!({ "weekday": 3 }),
                    index: Some(0),
                },
            ]
        );
    }

    /// C6 fixture：一个 chunk 里的**并行**工具调用全部给出（旧实现只给第一个）。
    #[test]
    fn parallel_tool_calls_in_one_event() {
        let data = r#"{"id":"chatcmpl-7","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_courses","arguments":"{}"}},{"index":1,"id":"call_2","function":{"name":"get_calendar","arguments":"{}"}}]},"finish_reason":null}]}"#;
        let chunks = parse(&serde_json::from_str(data).unwrap());
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert!(
            matches!(&chunks[0], Chunk::ToolCall { id, .. } if id == "call_1"),
            "{chunks:?}"
        );
        assert!(
            matches!(&chunks[1], Chunk::ToolCall { id, .. } if id == "call_2"),
            "{chunks:?}"
        );
    }

    /// C6 fixture：`finish_reason` 与 `usage` 在**同一个** chunk → `[Usage, Done]`。
    ///
    /// 顺序不是随便定的：`Done` 必末（§10），所以 `Usage` 必须在它前面。
    #[test]
    fn usage_and_finish_reason_in_one_event() {
        let data = r#"{"id":"chatcmpl-8","choices":[{"index":0,"delta":{"content":"好"},"finish_reason":"stop"}],"usage":{"prompt_tokens":42,"completion_tokens":7,"total_tokens":49}}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            vec![
                Chunk::Delta { text: "好".into() },
                Chunk::Usage {
                    tokens_in: 42,
                    tokens_out: 7,
                    estimated: false,
                },
                Chunk::Done {
                    finish: FinishReason::Stop
                },
            ]
        );
    }

    /// fixture 5b：上游错误对象 → `Chunk::Error(UpstreamError)`。
    #[test]
    fn error_object_fixture() {
        let data =
            r#"{"error":{"message":"upstream is overloaded","type":"server_error","code":null}}"#;
        assert_eq!(
            crate::protocols::parse_events(Protocol::OpenaiCompletions, data),
            vec![Chunk::Error {
                code: yunong_shared::ErrorCode::UpstreamError,
                message: "upstream is overloaded".into(),
            }]
        );
    }
}
