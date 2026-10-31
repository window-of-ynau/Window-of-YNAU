//! OpenAI `/v1/responses`（`response.*` 流式事件）↔ 内部结构。
//!
//! 编码要点（§9）：
//! - 系统提示：顶层 `instructions`（从 messages 里抽走 system）
//! - 工具声明：`tools[]{type:"function", name, description, parameters}`（**扁平**，
//!   没有 Completions 的 `function` 包装层）
//! - 流式开关：`stream`；输出上限：`max_output_tokens`（不是 `max_tokens`）
//! - 工具调用：`input[]` 里的 `function_call` item；工具结果：`function_call_output` item
//!
//! # 契约落点（`docs/design/contract-gaps.md`）
//!
//! - **C1 已解决（ADR-16）**：一条内部 assistant 消息可以展开成**多个** input item
//!   （有文本 → 一个 message item；每个工具调用 → 一个 `function_call` item），
//!   所以"边说边调工具"与并行多调用都能表达。
//! - `function_call_output` 只有 `call_id` + `output`：**工具名丢失**，
//!   `ok` 也无法表达（只能把失败文本写进 `output`，缺口 C3 / C4）。
//! - `ChatRequest` 没有 `temperature`：不发送该字段（缺口 C2）。
//! - 本事件集没有"纯文本 + usage"的独立事件：`response.completed`
//!   **同时**是终态和唯一 usage 来源（缺口 C6 的核心案例，见 `parse`）。

use serde_json::{json, Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, Role, ToolSpec};

use super::{call_index, finish_reason, openai_usage, partial_json, system_prompt};

/// 构造请求体。
pub(super) fn build(request: &ChatRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(request.stream));
    body.insert("max_output_tokens".into(), json!(request.max_output_tokens));

    // 系统提示走顶层 instructions，messages 里不能再出现 system。
    if let Some(system) = system_prompt(&request.messages) {
        body.insert("instructions".into(), json!(system));
    }
    // C1：一条内部 assistant 消息可能展开成多个 item（文本 + 每个工具调用一个）。
    let input: Vec<Value> = request
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .flat_map(encode_item)
        .collect();
    body.insert("input".into(), Value::Array(input));

    if !request.tools.is_empty() {
        body.insert("tools".into(), encode_tools(&request.tools));
    }
    Value::Object(body)
}

/// 一条消息 → **多个**输入 item（C1：assistant 的文本与工具调用各占一个 item）。
fn encode_item(message: &ChatMessage) -> Vec<Value> {
    match message.role {
        // 兜底：正常流程不会走到这里。
        Role::System => vec![text_item("system", &message.content)],
        Role::User => vec![text_item("user", &message.content)],
        Role::Assistant => encode_assistant(message),
        Role::Tool => vec![encode_tool_output(message)],
    }
}

/// assistant：可见文本与工具调用**共存**（C1）。
///
/// - 有文本 → 一个 EasyInputMessage item；
/// - 每个调用 → 一个 `function_call` item（Responses 用 `call_id` 关联结果，
///   服务端 item id 由上游生成，这里不填）。
fn encode_assistant(message: &ChatMessage) -> Vec<Value> {
    let mut items = Vec::with_capacity(message.tool_calls.len() + 1);
    if !message.content.is_empty() {
        items.push(text_item("assistant", &message.content));
    }
    for call in &message.tool_calls {
        items.push(json!({
            "type": "function_call",
            "call_id": call.id,
            "name": call.name,
            "arguments": arguments_text(&call.arguments),
        }));
    }
    if items.is_empty() {
        // 既无文本也无调用：保留一条空 assistant 消息，别让它在编码里凭空消失。
        items.push(text_item("assistant", ""));
    }
    items
}

/// `arguments` → 字符串（Responses 与 Completions 一样是 JSON **字符串**）。
fn arguments_text(arguments: &Value) -> String {
    match arguments {
        // 未聚合完的片段原样转发，让上游明确报参数非法，而不是静默变成 `{}`（C5）。
        Value::String(raw) => raw.clone(),
        value => value.to_string(),
    }
}

/// 工具结果 → `function_call_output`。缺 `call_id` 时降级为 user 文本（发出去必 400）。
fn encode_tool_output(message: &ChatMessage) -> Value {
    match message.tool_call_id.as_deref() {
        Some(call_id) => json!({
            "type": "function_call_output",
            "call_id": call_id,
            // 工具名（ChatMessage::name）在 Responses 里没有位置，丢失。
            "output": message.content,
        }),
        None => {
            let name = message.name.as_deref().unwrap_or("tool");
            text_item(
                "user",
                &format!("[工具 {name} 的结果]\n{}", message.content),
            )
        }
    }
}

/// 纯文本消息用 EasyInputMessage 简化形式（`role` + 字符串 `content`），
/// 避免为纯文本场景引入 content part 数组的噪声。
fn text_item(role: &str, content: &str) -> Value {
    json!({ "role": role, "content": content })
}

/// 工具声明：扁平结构，`type:"function"` 与 name/description/parameters 同级。
fn encode_tools(tools: &[ToolSpec]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                })
            })
            .collect(),
    )
}

/// 解析一条 Responses 流式事件 → **0..n 个** `Chunk`（缺口 C6）。
///
/// # 终态与 usage 同事件（C6 的核心案例）
///
/// Completions 与 Anthropic 都有独立的终态事件（`[DONE]` / `message_stop`），
/// Responses **没有**：唯一的终态事件 `response.completed` 同时携带 `status` 与 `usage`。
/// 旧实现只能二选一（要么丢 usage，要么丢终态）。现在返回 `[Usage, Done]`：
///
/// - 有 `usage` → `[Usage, Done]`（`Done` 必末，§10）；
/// - 无 `usage` → `[Done]`；
/// - `response.incomplete`（被 max_output_tokens / content_filter 截断）同样
///   `[Usage, Done]`，且结束原因是**真实**截断原因而不是 `Stop`。
///
/// # 工具调用分片
///
/// - `response.output_item.added/done`：`item` 里有 `call_id` + `name`，
///   `.added` 时 `arguments` 通常为空串，`.done` 时是完整 JSON。
/// - `response.function_call_arguments.delta`：**只有 `item_id` 和参数字符串片段**，
///   没有 name / call_id。本函数无状态，只能把 `item_id` 当 `id`、`name` 留空、
///   `arguments` 返回片段（`Value::String`）；调用方需按 `item_id` 自行关联到
///   之前 `.added` 事件里的 name。
pub(super) fn parse(value: &Value) -> Vec<Chunk> {
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "response.output_text.delta" => {
            let text = value
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![Chunk::Delta {
                    text: text.to_owned(),
                }]
            }
        }
        "response.output_item.added" | "response.output_item.done" => {
            option_chunk(function_call(value))
        }
        "response.function_call_arguments.delta" => option_chunk(arguments_delta(value)),
        // completed 与 incomplete 都同时带 usage + 结束原因。
        "response.completed" | "response.incomplete" => terminal(value),
        // `response.failed` 在 `parse_events` 里已由 upstream_error 拦下，这里兜底。
        "response.failed" => option_chunk(super::upstream_error(value)),
        // created / in_progress / output_text.done / output_item.done(message) 等
        // 都没有新增信息（文本已经流过）→ 不暴露给上层。
        _ => Vec::new(),
    }
}

/// 终态事件：`usage` 与结束原因**两个都给**（`[Usage, Done]`，`Done` 在最后）。
fn terminal(value: &Value) -> Vec<Chunk> {
    let response = value.get("response");
    let mut chunks = Vec::new();
    if let Some(usage) = response.and_then(|r| r.get("usage")).and_then(openai_usage) {
        chunks.push(usage);
    }
    let finish = if value.get("type").and_then(Value::as_str) == Some("response.incomplete") {
        finish_reason(incomplete_reason(value))
    } else {
        finish_reason(
            response
                .and_then(|r| r.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("completed"),
        )
    };
    chunks.push(Chunk::Done { finish });
    super::done_last(chunks)
}

/// 最多产出一个 chunk 的分支：收成 `Vec`。
fn option_chunk(chunk: Option<Chunk>) -> Vec<Chunk> {
    chunk.into_iter().collect()
}

/// `item` 必须是 `type:"function_call"`，否则（消息 item）忽略。
fn function_call(value: &Value) -> Option<Chunk> {
    let item = value.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }
    let id = item
        .get("call_id")
        .and_then(Value::as_str)
        .or_else(|| item.get("id").and_then(Value::as_str))
        .unwrap_or_default();
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(Chunk::ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: partial_json(arguments),
        index: call_index(value, "output_index"),
    })
}

fn arguments_delta(value: &Value) -> Option<Chunk> {
    let id = value
        .get("item_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let fragment = value
        .get("delta")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(Chunk::ToolCall {
        id: id.to_owned(),
        // name 只出现在 output_item 事件里，本事件拿不到（无状态限制）。
        name: String::new(),
        arguments: partial_json(fragment),
        index: call_index(value, "output_index"),
    })
}

fn incomplete_reason(value: &Value) -> &str {
    value
        .get("response")
        .and_then(|r| r.get("incomplete_details"))
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or("incomplete")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::{
        fixture_request, fixture_text_and_tool_round, fixture_tool_round, FIXTURE_SYSTEM,
    };
    use yunong_shared::{FinishReason, Protocol, ToolCallRef};

    fn body() -> Value {
        build(&fixture_request(Protocol::OpenaiResponses))
    }

    /// 单 chunk 事件收成 `Option`；顺带断言"这条事件确实只产出一个 chunk"。
    fn parse_one(data: &str) -> Option<Chunk> {
        let mut chunks = parse(&serde_json::from_str(data).unwrap());
        assert!(chunks.len() <= 1, "该事件应只有一个 chunk：{chunks:?}");
        chunks.pop()
    }

    // ---------- build_body ----------

    #[test]
    fn system_prompt_becomes_top_level_instructions() {
        let body = body();
        assert_eq!(body["instructions"], FIXTURE_SYSTEM);
        // messages 里（现在是 input）不能再出现 system。
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["role"] != "system"));
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"], "下周三下午有课吗");
    }

    #[test]
    fn tools_are_flat_without_function_wrapper() {
        let body = body();
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], "get_courses");
        assert_eq!(tool["description"], "查询课表");
        assert_eq!(tool["parameters"]["type"], "object");
        // Responses 没有 `function` 包装层。
        assert!(tool.get("function").is_none());
    }

    #[test]
    fn output_cap_uses_max_output_tokens_field() {
        let body = body();
        assert_eq!(body["max_output_tokens"], 512u32);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["stream"], true);
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn tool_round_encodes_function_call_items() {
        let body = build(&fixture_tool_round(Protocol::OpenaiResponses));
        let input = body["input"].as_array().unwrap();
        // input 里没有 system：0=user、1=assistant(function_call)、2=tool(function_call_output)。
        assert_eq!(input[0]["role"], "user");
        let call = &input[1];
        assert_eq!(call["type"], "function_call");
        assert_eq!(call["call_id"], "call_1");
        assert_eq!(call["name"], "get_courses");
        assert_eq!(call["arguments"], r#"{"weekday":3}"#);

        let output = &input[2];
        assert_eq!(output["type"], "function_call_output");
        assert_eq!(output["call_id"], "call_1");
        assert_eq!(output["output"], r#"{"courses":[]}"#);
    }

    /// C1：「文本 + 工具同轮」→ 一条内部 assistant 消息展开成**两个** item。
    #[test]
    fn text_and_tool_call_become_two_items() {
        let body = build(&fixture_text_and_tool_round(Protocol::OpenaiResponses));
        let input = body["input"].as_array().unwrap();
        // 0=user、1=assistant(text item)、2=function_call、3=function_call_output
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"], "我先查一下课表");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["arguments"], r#"{"weekday":3}"#);
        assert_eq!(input[3]["type"], "function_call_output");
    }

    /// C1：并行多调用 → 文本 item + 每个调用一个 `function_call` item。
    #[test]
    fn parallel_tool_calls_become_multiple_items() {
        let mut request = fixture_request(Protocol::OpenaiResponses);
        request.messages.push(ChatMessage::assistant_calls(
            "稍等",
            vec![
                ToolCallRef::new("call_1", "get_courses", serde_json::json!({ "weekday": 3 })),
                ToolCallRef::new("call_2", "get_calendar", serde_json::json!({})),
            ],
        ));
        let body = build(&request);
        let input = body["input"].as_array().unwrap();
        let calls: Vec<&Value> = input
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect();
        assert_eq!(calls.len(), 2, "{input:?}");
        assert_eq!(calls[1]["call_id"], "call_2");
        let text: Vec<&Value> = input
            .iter()
            .filter(|item| item["role"] == "assistant")
            .collect();
        assert_eq!(text.len(), 1, "{input:?}");
        assert_eq!(text[0]["content"], "稍等");
    }

    // ---------- parse_events ----------

    /// fixture 1：纯文本增量。
    #[test]
    fn text_delta_fixture() {
        let data = r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"下午没课"}"#;
        assert_eq!(
            parse_one(data),
            Some(Chunk::Delta {
                text: "下午没课".into()
            })
        );
    }

    /// fixture 2：工具调用（`.added` 给 name，`.done` 给完整参数）。
    #[test]
    fn tool_call_fixture() {
        let added = r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"get_courses","arguments":""}}"#;
        assert_eq!(
            parse_one(added),
            Some(Chunk::ToolCall {
                id: "call_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({}),
                index: Some(1),
            })
        );

        let done = r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"get_courses","arguments":"{\"weekday\":3}"}}"#;
        assert_eq!(
            parse_one(done),
            Some(Chunk::ToolCall {
                id: "call_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({ "weekday": 3 }),
                index: Some(1),
            })
        );
    }

    /// fixture 2b：参数字符串分片——本事件没有 name/call_id，只能按 `item_id` 关联。
    #[test]
    fn argument_delta_has_no_name() {
        let json = r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":1,"delta":"{\"weekday\":"}"#;
        assert_eq!(
            parse_one(json),
            Some(Chunk::ToolCall {
                id: "fc_1".into(),
                name: String::new(),
                arguments: Value::String(r#"{"weekday":"#.into()),
                index: Some(1),
            })
        );
    }

    /// C6 fixture：`response.completed` **同时**是终态和唯一 usage 来源 → `[Usage, Done]`。
    ///
    /// 旧实现二选一（要么丢 usage，要么丢终态）；现在两个都给，`Done` 在最后。
    #[test]
    fn usage_and_finish_in_one_event() {
        let data = r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":42,"output_tokens":7,"total_tokens":49}}}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            vec![
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

    /// fixture 4：终态（无 usage / 被截断）。
    #[test]
    fn finish_reason_fixture() {
        // 没有 usage 的 completed → 只有 Done。
        let completed =
            r#"{"type":"response.completed","response":{"id":"resp_2","status":"completed"}}"#;
        assert_eq!(
            parse_one(completed),
            Some(Chunk::Done {
                finish: FinishReason::Stop
            })
        );

        // 被截断：真实原因与 usage 都要保留 → `[Usage, Done{Length}]`。
        let incomplete = r#"{"type":"response.incomplete","response":{"id":"resp_3","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":42,"output_tokens":512}}}"#;
        assert_eq!(
            parse(&serde_json::from_str(incomplete).unwrap()),
            vec![
                Chunk::Usage {
                    tokens_in: 42,
                    tokens_out: 512,
                    estimated: false,
                },
                Chunk::Done {
                    finish: FinishReason::Length
                },
            ]
        );

        assert_eq!(
            crate::protocols::parse_events(Protocol::OpenaiResponses, "[DONE]"),
            vec![Chunk::Done {
                finish: FinishReason::Stop
            }]
        );
    }

    /// fixture 5a：无关事件（created / 文本收尾 / 消息 item）→ None。
    #[test]
    fn irrelevant_events_are_ignored() {
        let created =
            r#"{"type":"response.created","response":{"id":"resp_4","status":"in_progress"}}"#;
        assert_eq!(parse_one(created), None);

        let text_done =
            r#"{"type":"response.output_text.done","item_id":"msg_1","text":"下午没课"}"#;
        assert_eq!(parse_one(text_done), None);

        let message_item = r#"{"type":"response.output_item.added","item":{"type":"message","id":"msg_1","role":"assistant"}}"#;
        assert_eq!(parse_one(message_item), None);
    }

    /// fixture 5b：`response.failed` → `Chunk::Error(UpstreamError)`。
    #[test]
    fn failed_event_fixture() {
        let data = r#"{"type":"response.failed","response":{"id":"resp_5","status":"failed","error":{"code":"server_error","message":"upstream is overloaded"}}}"#;
        assert_eq!(
            crate::protocols::parse_events(Protocol::OpenaiResponses, data),
            vec![Chunk::Error {
                code: yunong_shared::ErrorCode::UpstreamError,
                message: "upstream is overloaded".into(),
            }]
        );
    }
}
