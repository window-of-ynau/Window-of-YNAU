// 内部 API（非对外 HTTP 契约）：parse_event → parse_events，见 ADR-16。
//! 三协议适配（契约见 `docs/03-接口文档.md` §9 / ADR-13）。
//!
//! 这一层只做**纯数据变换**：把内部 `ChatRequest` 编成各家请求体，
//! 把各家的流式事件解析成统一的 `Chunk`。
//!
//! 刻意**不在这里发网络请求**：网络交给 `gateway`，这样适配逻辑可以用 fixture 单测，
//! 不需要真实 Key、不需要联网。
//!
//! # 契约缺口（登记在 `docs/design/contract-gaps.md`）
//!
//! - **C1 已解决（ADR-16）**：`ChatMessage::tool_calls` 取代"content 装 arguments"的旧约定，
//!   assistant 的可见文本与工具调用可以共存，并行多调用也能表达。
//! - **C6 已解决（ADR-16）**：`parse_events` 一次返回 `Vec<Chunk>`，一个上游事件可以同时
//!   产出 `Usage` 与 `Done`（顺序固定为 `[.., Usage, Done]`）。
//! - 仍需实现层变通的点，逐条在对应协议的模块注释里写清：
//!   1. `ChatMessage` 没有 `ok` / `is_error`（C3）。工具失败在 Anthropic 的
//!      `tool_result.is_error` 里表达不出来，只能把失败文本写进 `content`。
//!   2. `ChatRequest` 没有 `temperature`（C2）。三协议一律**不发送**温度字段。
//!   3. `parse_events` 是**无状态**的（C7）：工具调用的 `id` / `name` / `arguments`
//!      被上游拆到多个事件里时，每次只能给出一部分，跨事件聚合由 `gateway` 负责。
//!   4. `Chunk::Meta` 需要 `channel`（业务概念，来自路由）与 `request_id`，本层拿不到，
//!      所以**不产出 Meta**，由 `gateway` 注入（C8）。上游心跳一律 → 空 `Vec`。

mod completions;
mod messages;
mod responses;

use serde_json::{Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, FinishReason, Protocol, Role};

// 只有 fixture 用到（生产路径里 `ChatRequest` 已携带 tool_calls）。
#[cfg(test)]
use yunong_shared::ToolCallRef;

/// 构造请求体时的开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildOptions {
    /// 是否让上游在流里回报 `usage`（Completions 的 `stream_options.include_usage`）。
    ///
    /// 默认 `true`：不开的话 OpenAI 流里根本没有 usage，记账只能靠估算。
    /// 上游是**第三方兼容网关**、因为未知字段返回 400 时，由调用方关掉
    /// （`DirectProvider::with_usage_reporting(false)`）。
    pub report_usage: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self { report_usage: true }
    }
}

/// 构造对应协议的请求体（默认开关）。
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

/// [`build_body`] 的可配置版本（目前只有 Completions 的 usage 回报开关）。
pub fn build_body_with(protocol: Protocol, request: &ChatRequest, options: &BuildOptions) -> Value {
    match protocol {
        Protocol::OpenaiCompletions => completions::build_with(request, options),
        Protocol::OpenaiResponses => responses::build(request),
        Protocol::AnthropicMessages => messages::build(request),
    }
}

/// 解析一条流式事件（`data: ` 前缀已剥离；可能是 `[DONE]`）→ **0..n 个** `Chunk`。
///
/// 返回空 `Vec` 表示该事件不需要暴露给上层（keep-alive、空 delta、角色首帧……）。
///
/// # 为什么是 `Vec`（缺口 C6，ADR-16）
///
/// 上游会把"结束原因"和"usage"塞进**同一个事件**：Anthropic 的 `message_delta`
/// （`stop_reason` + `usage.output_tokens`）、Responses 的 `response.completed`
/// （`status` + `usage`）、Completions 的 `finish_reason` + `usage` 合并帧。
/// 一次只能返回一个 `Chunk` 就必然二选一，丢 token 数 = 成本监控失真。
///
/// # 顺序保证（全流不变量：`Done` 必末且唯一）
///
/// - 同一事件同时含 usage 与结束原因 → `[.., Usage, Done]`（`Done` 在最后，见 [`done_last`]）；
/// - 同一事件既含文本增量又含工具调用 → `[Delta, ToolCall, ..]`（按上游字段顺序）。
///
/// **`[DONE]` 是幂等的收尾标记**：Completions 的 `finish_reason` 帧已经给出 `Done`
/// （带真实原因），随后还会再来一个 `[DONE]` → 又给一个 `Done{Stop}`。
/// 因此"`Done` 唯一"由 **`gateway` 去重**保证（首个 `Done` 为准，忽略后续），
/// 而不是靠丢掉带原因的 `finish_reason`。
///
/// 三协议统一行为（契约测试覆盖）：
/// - `[DONE]` → `[Chunk::Done { finish: Stop }]`（Anthropic / Responses 不保证发送它，
///   缺失时由 `gateway` 在流结束时补发）
/// - 空串、非 JSON 文本 → 空 `Vec`
/// - 顶层 `error` 对象 / `response.failed` → `[Chunk::Error { code: UpstreamError, .. }]`
///
/// **无状态**：工具调用参数分片请按各协议注释聚合（本函数对不完整分片返回
/// `Value::String` 原始片段）。
pub fn parse_events(protocol: Protocol, data: &str) -> Vec<Chunk> {
    let data = data.trim();
    if data.is_empty() {
        return Vec::new();
    }
    // 三协议一致：`[DONE]` 视为正常收尾。
    if data == "[DONE]" {
        return vec![Chunk::Done {
            finish: FinishReason::Stop,
        }];
    }
    // 非 JSON（SSE 注释、纯文本心跳）不需要暴露给上层。
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    if let Some(error) = upstream_error(&value) {
        return vec![error];
    }
    match protocol {
        Protocol::OpenaiCompletions => completions::parse(&value),
        Protocol::OpenaiResponses => responses::parse(&value),
        Protocol::AnthropicMessages => messages::parse(&value),
    }
}

/// 把 `Done` 挪到末尾，其余保持相对顺序（`sort_by_key` 是稳定排序）。
///
/// 全流不变量 `Done` 必末且唯一：适配器只负责"这个事件里有什么"，
/// 排序统一在这里做，免得三家各写一遍（也免得某家忘写）。
fn done_last(mut chunks: Vec<Chunk>) -> Vec<Chunk> {
    if chunks.len() > 1 && chunks.iter().any(|c| matches!(c, Chunk::Done { .. })) {
        chunks.sort_by_key(|c| matches!(c, Chunk::Done { .. }));
    }
    chunks
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

/// 工具调用的序号（C9）：Completions 用 `index`、Responses 用 `output_index`。
///
/// 并行调用时，参数分片必须靠它归位——没有序号就只能瞎猜（A 的参数会拼到 B 上）。
pub(crate) fn call_index(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
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

/// 单次工具调用的 fixture（C1：`id` / `name` / `arguments` 三件套）。
#[cfg(test)]
fn fixture_tool_calls() -> Vec<ToolCallRef> {
    vec![ToolCallRef::new(
        "call_1",
        "get_courses",
        serde_json::json!({ "weekday": 3 }),
    )]
}

/// 工具循环第 2 轮的请求：末尾追加 assistant(tool_calls) 与 tool(tool_result)。
#[cfg(test)]
fn fixture_tool_round(protocol: Protocol) -> ChatRequest {
    let mut request = fixture_request(protocol);
    request.round = 2;
    request
        .messages
        .push(ChatMessage::assistant_calls("", fixture_tool_calls()));
    request.messages.push(ChatMessage::tool_result(
        "call_1",
        "get_courses",
        r#"{"courses":[]}"#,
    ));
    request
}

/// C1 的"文本 + 工具同轮"：assistant 边说边调工具（三协议共用的契约 fixture）。
#[cfg(test)]
fn fixture_text_and_tool_round(protocol: Protocol) -> ChatRequest {
    let mut request = fixture_tool_round(protocol);
    // assistant 紧挨在 tool_result 之前。
    let assistant = request.messages.len() - 2;
    request.messages[assistant] =
        ChatMessage::assistant_calls("我先查一下课表", fixture_tool_calls());
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

    /// 三协议对 `[DONE]` 的处理必须一致（契约要求：三协议一致）。
    #[test]
    fn done_marker_is_uniform_across_protocols() {
        for protocol in ALL {
            assert_eq!(
                parse_events(protocol, "[DONE]"),
                vec![Chunk::Done {
                    finish: FinishReason::Stop
                }],
                "{protocol:?} 的 [DONE] 处理与契约不一致"
            );
        }
    }

    /// 空串、纯文本心跳、非 JSON 一律不暴露给上层（空 `Vec`，不是 `None`）。
    #[test]
    fn blank_and_non_json_events_are_ignored() {
        for protocol in ALL {
            for data in ["", "   \n", ": keep-alive", "not-json"] {
                assert!(parse_events(protocol, data).is_empty(), "{protocol:?}");
            }
        }
    }

    /// 顶层 `error` 对象 → `UpstreamError`，三协议行为一致。
    #[test]
    fn top_level_error_maps_to_upstream_error() {
        let data = r#"{"error":{"message":"upstream boom","type":"api_error"}}"#;
        for protocol in ALL {
            assert_eq!(
                parse_events(protocol, data),
                vec![Chunk::Error {
                    code: yunong_shared::ErrorCode::UpstreamError,
                    message: "upstream boom".into(),
                }],
                "{protocol:?}"
            );
        }
    }

    /// 全流不变量 + `[DONE]` 去重口径（**t2 gateway 的接口约定**）。
    ///
    /// - 每次 `parse_events` 的产出里 `Done` 必在最后（同事件不变量）；
    /// - Completions 的 `finish_reason` 帧之后还会来一个 `[DONE]`，所以一条完整流会给出
    ///   **两个** `Done`：`[DONE]` 是幂等收尾标记，**gateway 必须去重**（首个 Done 为准）；
    /// - 三协议的 usage 分片形态不同，记账层按字段取 max 才能合出完整用量。
    #[test]
    fn stream_done_invariant_and_dedup_contract() {
        /// (名字, 协议, 事件序列, 期望 Done 个数, 期望 usage 分片)
        type StreamCase = (
            &'static str,
            Protocol,
            &'static [&'static str],
            usize,
            Vec<(u32, u32)>,
        );
        let streams: [StreamCase; 3] = [
            (
                "completions",
                Protocol::OpenaiCompletions,
                &[
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"下"}}]}"#,
                    r#"{"choices":[{"index":0,"delta":{"content":"午没课"},"finish_reason":"stop"}],"usage":{"prompt_tokens":42,"completion_tokens":7}}"#,
                    "[DONE]",
                ],
                // finish_reason 帧 + [DONE] → 两个 Done，由 gateway 去重。
                2,
                vec![(42, 7)],
            ),
            (
                "responses",
                Protocol::OpenaiResponses,
                &[
                    r#"{"type":"response.output_text.delta","delta":"下午没课"}"#,
                    r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":42,"output_tokens":7}}}"#,
                ],
                1,
                vec![(42, 7)],
            ),
            (
                "messages",
                Protocol::AnthropicMessages,
                &[
                    r#"{"type":"message_start","message":{"usage":{"input_tokens":42,"output_tokens":1}}}"#,
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"下午没课"}}"#,
                    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
                    r#"{"type":"message_stop"}"#,
                ],
                1,
                // usage 是两个事件里各半：input 在 start、output 在 delta。
                vec![(42, 1), (0, 7)],
            ),
        ];

        for (name, protocol, events, expected_dones, expected_usage) in streams {
            let chunks: Vec<Chunk> = events
                .iter()
                .flat_map(|event| parse_events(protocol, event))
                .collect();
            assert!(
                matches!(chunks.last(), Some(Chunk::Done { .. })),
                "{name}：Done 不在最后一个：{chunks:?}"
            );
            assert_eq!(
                chunks
                    .iter()
                    .filter(|c| matches!(c, Chunk::Done { .. }))
                    .count(),
                expected_dones,
                "{name}：{chunks:?}"
            );
            let text: String = chunks
                .iter()
                .filter_map(|c| match c {
                    Chunk::Delta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(text, "下午没课", "{name}");
            let usage: Vec<(u32, u32)> = chunks
                .iter()
                .filter_map(|c| match c {
                    Chunk::Usage {
                        tokens_in,
                        tokens_out,
                        ..
                    } => Some((*tokens_in, *tokens_out)),
                    _ => None,
                })
                .collect();
            assert_eq!(usage, expected_usage, "{name}：usage 分片形态变了");
        }
    }

    /// C6 的排序保证：`Done` 永远在末尾，其余保持相对顺序（稳定排序）。
    #[test]
    fn done_last_moves_done_to_tail() {
        let usage = Chunk::Usage {
            tokens_in: 0,
            tokens_out: 15,
            estimated: false,
        };
        assert_eq!(
            done_last(vec![
                Chunk::Done {
                    finish: FinishReason::Stop
                },
                Chunk::Delta { text: "a".into() },
                usage.clone(),
            ]),
            vec![
                Chunk::Delta { text: "a".into() },
                usage,
                Chunk::Done {
                    finish: FinishReason::Stop
                },
            ]
        );
        // 单元素 / 无 Done 的序列原样返回。
        assert_eq!(done_last(vec![Chunk::Ping]), vec![Chunk::Ping]);
        assert_eq!(done_last(Vec::new()), Vec::new());
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
