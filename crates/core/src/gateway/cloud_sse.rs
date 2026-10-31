//! 套餐通道（我们自己的 SSE）事件 → [`Chunk`]。
//!
//! 协议见 `docs/03-接口文档.md` §3.2 / §10，事件集固定为：
//! `meta` `delta` `tool_call` `tool_result` `usage` `done` `error` `ping`。
//!
//! 这里**只做解析**：`Meta` 注入 / `Done` 去重 / `usage` 合并三件事由
//! `yunong_shared::stream::ChunkAssembler` 统一负责（ADR-16），客户端不再写第二份。

use serde_json::Value;
use yunong_shared::{Channel, Chunk, ErrorCode, FinishReason};

use super::sse::SseFrame;
use super::truncate;

/// 解析一帧套餐通道 SSE。
///
/// - `None`：这一帧不需要变成 `Chunk`（未知事件、空 payload）；
/// - `Some(Chunk::Error { .. })`：服务端发来的 `error` 事件
///   （终止事件，装配器会据此收尾）。
pub fn parse_cloud_event(frame: &SseFrame) -> Option<Chunk> {
    let event = frame.event.as_str();
    if event == "ping" {
        // 心跳：让上层知道"链路还活着"，没有 payload 也要透传
        return Some(Chunk::Ping);
    }
    if frame.data.trim().is_empty() {
        return None;
    }
    let data: Value = match serde_json::from_str(&frame.data) {
        Ok(value) => value,
        Err(err) => {
            return Some(Chunk::Error {
                code: ErrorCode::UpstreamError,
                message: truncate(
                    &format!("套餐通道事件 `{event}` 不是合法 JSON：{err}"),
                    super::MAX_ERROR_CHARS,
                ),
            })
        }
    };
    match event {
        "meta" => Some(Chunk::Meta {
            request_id: text(&data, "request_id").unwrap_or_default(),
            model: text(&data, "model").unwrap_or_default(),
            channel: channel(&data),
        }),
        "delta" => match text(&data, "text") {
            // 空增量没有信息量（服务端不该发，但兼容）
            Some(text) if !text.is_empty() => Some(Chunk::Delta { text }),
            _ => None,
        },
        "tool_call" => Some(Chunk::ToolCall {
            id: text(&data, "id").unwrap_or_default(),
            name: text(&data, "name").unwrap_or_default(),
            index: None,
            arguments: data
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default())),
        }),
        "tool_result" => Some(Chunk::ToolResult {
            id: text(&data, "id").unwrap_or_default(),
            // 没有显式 `ok` 时按成功处理（协议里失败必须显式写 false）
            ok: data.get("ok").and_then(Value::as_bool).unwrap_or(true),
            content: match data.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            },
        }),
        "usage" => Some(Chunk::Usage {
            tokens_in: number(&data, "tokens_in"),
            tokens_out: number(&data, "tokens_out"),
            estimated: data
                .get("estimated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        "done" => Some(Chunk::Done {
            finish: finish(&data),
        }),
        "error" => Some(Chunk::Error {
            code: error_code(&data),
            message: truncate(
                &text(&data, "message").unwrap_or_else(|| "服务端返回了错误事件".to_owned()),
                super::MAX_ERROR_CHARS,
            ),
        }),
        // 未知事件（服务端升级时可能出现新事件）：客户端不能因此炸掉
        _ => None,
    }
}

fn text(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number(data: &Value, key: &str) -> u32 {
    data.get(key)
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

fn channel(data: &Value) -> Channel {
    text(data, "channel")
        .and_then(|raw| serde_json::from_value::<Channel>(Value::String(raw)).ok())
        .unwrap_or(Channel::Plan)
}

fn finish(data: &Value) -> FinishReason {
    text(data, "finish")
        .and_then(|raw| serde_json::from_value::<FinishReason>(Value::String(raw)).ok())
        .unwrap_or(FinishReason::Stop)
}

fn error_code(data: &Value) -> ErrorCode {
    text(data, "code")
        .and_then(|raw| serde_json::from_value::<ErrorCode>(Value::String(raw)).ok())
        // 认不出的错误码按上游错误处理（不要冒成 INTERNAL，那会误导重试策略）
        .unwrap_or(ErrorCode::UpstreamError)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(event: &str, data: &str) -> SseFrame {
        SseFrame {
            event: event.into(),
            data: data.into(),
        }
    }

    fn parse(event: &str, data: &str) -> Option<Chunk> {
        parse_cloud_event(&frame(event, data))
    }

    #[test]
    fn meta_carries_request_id() {
        let chunk = parse(
            "meta",
            r#"{"request_id":"req_9f3c","model":"deepseek-chat","channel":"plan"}"#,
        )
        .unwrap();
        assert_eq!(
            chunk,
            Chunk::Meta {
                request_id: "req_9f3c".into(),
                model: "deepseek-chat".into(),
                channel: Channel::Plan,
            }
        );
    }

    #[test]
    fn delta_and_empty_delta() {
        assert_eq!(
            parse("delta", r#"{"text":"下周三下午"}"#),
            Some(Chunk::Delta {
                text: "下周三下午".into()
            })
        );
        // 空增量不产生事件
        assert_eq!(parse("delta", r#"{"text":""}"#), None);
        // 缺字段也不产生事件
        assert_eq!(parse("delta", "{}"), None);
    }

    #[test]
    fn tool_call_and_tool_result() {
        assert_eq!(
            parse(
                "tool_call",
                r#"{"id":"c1","name":"get_courses","arguments":{"weekday":3}}"#
            ),
            Some(Chunk::ToolCall {
                id: "c1".into(),
                name: "get_courses".into(),
                arguments: serde_json::json!({ "weekday": 3 }),
                index: None,
            })
        );
        // arguments 缺省是空对象（不要给 null）
        assert_eq!(
            parse("tool_call", r#"{"id":"c1","name":"x"}"#),
            Some(Chunk::ToolCall {
                id: "c1".into(),
                name: "x".into(),
                arguments: serde_json::json!({}),
                index: None,
            })
        );
        assert_eq!(
            parse("tool_result", r#"{"id":"c1","content":"{}"}"#),
            Some(Chunk::ToolResult {
                id: "c1".into(),
                ok: true,
                content: "{}".into(),
            })
        );
        assert_eq!(
            parse(
                "tool_result",
                r#"{"id":"c1","ok":false,"content":"没登录"}"#
            ),
            Some(Chunk::ToolResult {
                id: "c1".into(),
                ok: false,
                content: "没登录".into(),
            })
        );
    }

    #[test]
    fn usage_done_ping() {
        assert_eq!(
            parse(
                "usage",
                r#"{"tokens_in":3120,"tokens_out":96,"estimated":false}"#
            ),
            Some(Chunk::Usage {
                tokens_in: 3120,
                tokens_out: 96,
                estimated: false,
            })
        );
        assert_eq!(
            parse("done", r#"{"finish":"stop","cost_cents":1}"#),
            Some(Chunk::Done {
                finish: FinishReason::Stop
            })
        );
        // 结束原因缺失 → 按 stop（服务端已经正常收尾）
        assert_eq!(
            parse("done", "{}"),
            Some(Chunk::Done {
                finish: FinishReason::Stop
            })
        );
        assert_eq!(parse("ping", ""), Some(Chunk::Ping));
    }

    #[test]
    fn error_event_maps_code() {
        let chunk = parse(
            "error",
            r#"{"code":"REVIEW_REJECTED","message":"内容不合规"}"#,
        )
        .unwrap();
        assert_eq!(
            chunk,
            Chunk::Error {
                code: ErrorCode::ReviewRejected,
                message: "内容不合规".into(),
            }
        );
        // 认不出的码 → UpstreamError，而不是 INTERNAL
        let chunk = parse("error", r#"{"code":"WHATEVER","message":"x"}"#).unwrap();
        assert!(matches!(
            chunk,
            Chunk::Error {
                code: ErrorCode::UpstreamError,
                ..
            }
        ));
    }

    #[test]
    fn malformed_json_becomes_upstream_error() {
        let chunk = parse("delta", "{not json").unwrap();
        match chunk {
            Chunk::Error { code, message } => {
                assert_eq!(code, ErrorCode::UpstreamError);
                assert!(message.contains("delta"), "{message}");
            }
            other => panic!("应变成 Error：{other:?}"),
        }
    }

    #[test]
    fn unknown_events_and_empty_payloads_are_ignored() {
        assert_eq!(parse("future_event", r#"{"x":1}"#), None);
        assert_eq!(parse("delta", ""), None);
        assert_eq!(parse("meta", "   "), None);
    }

    #[test]
    fn long_error_message_is_truncated() {
        let long = "x".repeat(1000);
        let data = format!(r#"{{"code":"UPSTREAM_ERROR","message":"{long}"}}"#);
        let chunk = parse("error", &data).unwrap();
        match chunk {
            Chunk::Error { message, .. } => {
                assert!(message.chars().count() <= super::super::MAX_ERROR_CHARS);
            }
            other => panic!("应为 Error：{other:?}"),
        }
    }
}
