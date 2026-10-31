//! OpenAI `/v1/responses`（`response.*` 流式事件）↔ 内部结构。
//!
//! 编码要点（§9）：
//! - 系统提示：顶层 `instructions`（从 messages 里抽走 system）
//! - 工具声明：`tools[]{type:"function", name, description, parameters}`（**扁平**，
//!   没有 Completions 的 `function` 包装层）
//! - 流式开关：`stream`；输出上限：`max_output_tokens`（不是 `max_tokens`）
//! - 工具调用：`input[]` 里的 `function_call` item；工具结果：`function_call_output` item
//!
//! # 损失点（契约缺口的落点）
//!
//! - `function_call_output` 只有 `call_id` + `output`：**工具名丢失**，
//!   `ok` 也无法表达（只能把失败文本写进 `output`）。
//! - `function_call` 的 `arguments` 是 JSON **字符串**，而内部 `ChatMessage.content`
//!   同时承担"arguments 文本"和"可见文本"，所以 **assistant 文本 + 工具调用不能共存**。
//! - `ChatRequest` 没有 `temperature`：不发送该字段。
//! - 本事件集没有"纯文本 + usage"的独立事件：`response.completed` 同时是**终态**和
//!   **唯一 usage 来源**，见 `parse` 的说明。

use serde_json::{json, Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, Role, ToolSpec};

use super::{finish_reason, openai_usage, partial_json, system_prompt};

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
    let input: Vec<Value> = request
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(encode_item)
        .collect();
    body.insert("input".into(), Value::Array(input));

    if !request.tools.is_empty() {
        body.insert("tools".into(), encode_tools(&request.tools));
    }
    Value::Object(body)
}

/// 一条输入 item。`system` 已被 `build` 抽走。
fn encode_item(message: &ChatMessage) -> Value {
    match message.role {
        // 兜底：正常流程不会走到这里。
        Role::System => json!({ "role": "system", "content": message.content }),
        Role::User => text_item("user", &message.content),
        Role::Assistant => encode_assistant(message),
        Role::Tool => encode_tool_output(message),
    }
}

/// assistant：带 `tool_call_id` + `name` 视为工具调用，否则当普通文本消息。
fn encode_assistant(message: &ChatMessage) -> Value {
    match (message.tool_call_id.as_deref(), message.name.as_deref()) {
        (Some(call_id), Some(name)) => json!({
            "type": "function_call",
            // Responses 用 call_id 关联调用与结果；`id` 是服务端 item id，由上游生成。
            "call_id": call_id,
            "name": name,
            // 与 Completions 同约定：content 是 arguments 的 JSON 文本。
            "arguments": message.content,
        }),
        _ => text_item("assistant", &message.content),
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

/// 解析一条 Responses 流式事件。
///
/// # 结束原因 / usage 的取舍
///
/// Completions 与 Anthropic 都有独立的终态事件（`[DONE]` / `message_stop`），
/// Responses **没有**：唯一的终态事件 `response.completed` 同时携带 `status` 与 `usage`，
/// 而一次 `data` 只能返回一个 `Chunk`。这里选择：**有 usage → 返回 `Chunk::Usage`**，
/// `Done` 由调用方在流结束时补发（`[DONE]` 事件若存在也会给出 `Done{Stop}`）。
/// 理由：usage 无法事后补，`status=completed` 的语义与"流正常结束"等价。
/// 反之 `response.incomplete`（被 max_output_tokens / content_filter 截断）**唯一**
/// 的 finish 信息必须保留，因此那个事件返回 `Done`，其 usage 丢弃。
///
/// # 工具调用分片
///
/// - `response.output_item.added/done`：`item` 里有 `call_id` + `name`，
///   `.added` 时 `arguments` 通常为空串，`.done` 时是完整 JSON。
/// - `response.function_call_arguments.delta`：**只有 `item_id` 和参数字符串片段**，
///   没有 name / call_id。本函数无状态，只能把 `item_id` 当 `id`、`name` 留空、
///   `arguments` 返回片段（`Value::String`）；调用方需按 `item_id` 自行关联到
///   之前 `.added` 事件里的 name。
pub(super) fn parse(value: &Value) -> Option<Chunk> {
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "response.output_text.delta" => {
            let text = value.get("delta").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            Some(Chunk::Delta {
                text: text.to_owned(),
            })
        }
        "response.output_item.added" | "response.output_item.done" => function_call(value),
        "response.function_call_arguments.delta" => arguments_delta(value),
        "response.completed" => completed(value),
        "response.incomplete" => Some(Chunk::Done {
            finish: finish_reason(incomplete_reason(value)),
        }),
        // `response.failed` 在 `parse_event` 里已由 upstream_error 拦下，这里兜底。
        "response.failed" => super::upstream_error(value),
        // created / in_progress / output_text.done / output_item.done(message) 等
        // 都没有新增信息（文本已经流过）→ 不暴露给上层。
        _ => None,
    }
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
    })
}

fn completed(value: &Value) -> Option<Chunk> {
    let response = value.get("response");
    if let Some(usage) = response.and_then(|r| r.get("usage")) {
        if let Some(chunk) = openai_usage(usage) {
            return Some(chunk);
        }
    }
    let status = response
        .and_then(|r| r.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("completed");
    Some(Chunk::Done {
        finish: finish_reason(status),
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
    use crate::protocols::{fixture_request, fixture_tool_round, FIXTURE_SYSTEM};
    use yunong_shared::{FinishReason, Protocol};

    fn body() -> Value {
        build(&fixture_request(Protocol::OpenaiResponses))
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

    // ---------- parse_event ----------

    /// fixture 1：纯文本增量。
    #[test]
    fn text_delta_fixture() {
        let data = r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"下午没课"}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
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
            parse(&serde_json::from_str(added).unwrap()),
            Some(Chunk::ToolCall {
                id: "call_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({}),
            })
        );

        let done = r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"get_courses","arguments":"{\"weekday\":3}"}}"#;
        assert_eq!(
            parse(&serde_json::from_str(done).unwrap()),
            Some(Chunk::ToolCall {
                id: "call_1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({ "weekday": 3 }),
            })
        );
    }

    /// fixture 2b：参数字符串分片——本事件没有 name/call_id，只能按 `item_id` 关联。
    #[test]
    fn argument_delta_has_no_name() {
        let json = r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":1,"delta":"{\"weekday\":"}"#;
        assert_eq!(
            parse(&serde_json::from_str(json).unwrap()),
            Some(Chunk::ToolCall {
                id: "fc_1".into(),
                name: String::new(),
                arguments: Value::String(r#"{"weekday":"#.into()),
            })
        );
    }

    /// fixture 3：usage（`response.completed` 是唯一 usage 来源，优先返回 Usage）。
    #[test]
    fn usage_fixture() {
        let data = r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":42,"output_tokens":7,"total_tokens":49}}}"#;
        assert_eq!(
            parse(&serde_json::from_str(data).unwrap()),
            Some(Chunk::Usage {
                tokens_in: 42,
                tokens_out: 7,
                estimated: false,
            })
        );
    }

    /// fixture 4：结束。
    #[test]
    fn finish_reason_fixture() {
        // 没有 usage 的 completed → 直接给 Done。
        let completed =
            r#"{"type":"response.completed","response":{"id":"resp_2","status":"completed"}}"#;
        assert_eq!(
            parse(&serde_json::from_str(completed).unwrap()),
            Some(Chunk::Done {
                finish: FinishReason::Stop
            })
        );

        // 被截断：finish 信息必须保留，故这个事件给 Done 而不是 Usage。
        let incomplete = r#"{"type":"response.incomplete","response":{"id":"resp_3","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":42,"output_tokens":512}}}"#;
        assert_eq!(
            parse(&serde_json::from_str(incomplete).unwrap()),
            Some(Chunk::Done {
                finish: FinishReason::Length
            })
        );

        assert_eq!(
            crate::protocols::parse_event(Protocol::OpenaiResponses, "[DONE]"),
            Some(Chunk::Done {
                finish: FinishReason::Stop
            })
        );
    }

    /// fixture 5a：无关事件（created / 文本收尾 / 消息 item）→ None。
    #[test]
    fn irrelevant_events_are_ignored() {
        let created =
            r#"{"type":"response.created","response":{"id":"resp_4","status":"in_progress"}}"#;
        assert_eq!(parse(&serde_json::from_str(created).unwrap()), None);

        let text_done =
            r#"{"type":"response.output_text.done","item_id":"msg_1","text":"下午没课"}"#;
        assert_eq!(parse(&serde_json::from_str(text_done).unwrap()), None);

        let message_item = r#"{"type":"response.output_item.added","item":{"type":"message","id":"msg_1","role":"assistant"}}"#;
        assert_eq!(parse(&serde_json::from_str(message_item).unwrap()), None);
    }

    /// fixture 5b：`response.failed` → `Chunk::Error(UpstreamError)`。
    #[test]
    fn failed_event_fixture() {
        let data = r#"{"type":"response.failed","response":{"id":"resp_5","status":"failed","error":{"code":"server_error","message":"upstream is overloaded"}}}"#;
        assert_eq!(
            crate::protocols::parse_event(Protocol::OpenaiResponses, data),
            Some(Chunk::Error {
                code: yunong_shared::ErrorCode::UpstreamError,
                message: "upstream is overloaded".into(),
            })
        );
    }
}
