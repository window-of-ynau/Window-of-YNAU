//! 网关集成测试：本地假 SSE 服务（`tokio::net::TcpListener` 手写，**不引 wiremock**）。
//!
//! 覆盖两个实现的四类行为：
//! 1. 成功（含鉴权头、端点、请求体、`Meta` 注入、`Usage` 合并、`Done` 唯一）；
//! 2. 超时（首事件超时、事件间空闲超时）；
//! 3. 上游错误（非 2xx 状态码 → 稳定错误码；流内 `error` 事件）；
//! 4. 取消（回调 `Flow::Stop`、以及直接 drop future）—— 两种情况都必须让**服务端观察到断连**。

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use yunong_shared::{Channel, ChatMessage, ChatRequest, Chunk, FinishReason, Protocol};

use super::fake_server::{FakeServer, Step};
use super::*;

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

fn request(protocol: Protocol) -> ChatRequest {
    ChatRequest {
        session_id: "s_1".into(),
        turn_id: "t_1".into(),
        round: 1,
        model: "deepseek-chat".into(),
        protocol,
        messages: vec![
            ChatMessage::system("你是助手"),
            ChatMessage::user("下周三有课吗"),
        ],
        tools: vec![],
        max_output_tokens: 128,
        stream: true,
        estimated_input_tokens: 32,
    }
}

/// 测试用三段超时：正常流给足时间，超时用例单独调小。
fn timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(2),
        first_event: Duration::from_secs(2),
        idle: Duration::from_secs(2),
    }
}

fn fast_timeouts(millis: u64) -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(2),
        first_event: Duration::from_millis(millis),
        idle: Duration::from_millis(millis),
    }
}

fn byok(server: &FakeServer, protocol: Protocol) -> DirectProvider {
    DirectProvider::new(protocol, server.base_url(), "deepseek-chat")
        .with_api_key("sk-test-123456")
        .with_timeouts(timeouts())
}

fn cloud(server: &FakeServer) -> CloudProxy {
    CloudProxy::new(server.base_url(), "tok-abc").with_timeouts(timeouts())
}

fn sse(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn sse_data(data: &str) -> String {
    format!("data: {data}\n\n")
}

fn completions_delta(text: &str) -> String {
    sse_data(&format!(
        r#"{{"choices":[{{"index":0,"delta":{{"content":"{text}"}}}}]}}"#
    ))
}

/// Completions 末帧：`finish_reason` 与 `usage` 同事件。
fn completions_finish() -> String {
    sse_data(
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":42,"completion_tokens":7}}"#,
    )
}

/// 收集整条流；`stop_after = None` 表示从不取消。
async fn run<G: ModelGateway + ?Sized>(
    gateway: &G,
    request: &ChatRequest,
    stop_after: Option<usize>,
) -> (Vec<Chunk>, CoreResult<StreamOutcome>) {
    let mut chunks = Vec::new();
    let outcome = {
        let mut sink = |chunk: Chunk| {
            chunks.push(chunk);
            match stop_after {
                Some(limit) if chunks.len() >= limit => Flow::Stop,
                _ => Flow::Continue,
            }
        };
        gateway.chat_stream(request, &mut sink).await
    };
    (chunks, outcome)
}

fn deltas(chunks: &[Chunk]) -> String {
    chunks
        .iter()
        .filter_map(|chunk| match chunk {
            Chunk::Delta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn done_count(chunks: &[Chunk]) -> usize {
    chunks
        .iter()
        .filter(|chunk| matches!(chunk, Chunk::Done { .. }))
        .count()
}

// ---------------------------------------------------------------------------
// BYOK：成功
// ---------------------------------------------------------------------------

#[tokio::test]
async fn byok_completions_streams_with_meta_and_single_done() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(completions_delta("下周三")),
            Step::Write(completions_delta("下午没课")),
            Step::Write(completions_finish()),
            // Completions 真实形态：finish 帧之后还有一个 [DONE]
            Step::Write(sse_data("[DONE]")),
            Step::Close,
        ],
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiCompletions);
    let req = request(Protocol::OpenaiCompletions);
    let (chunks, outcome) = run(&provider, &req, None).await;
    let outcome = outcome.unwrap();

    assert_eq!(deltas(&chunks), "下周三下午没课");
    assert!(
        matches!(chunks.first(), Some(Chunk::Meta { .. })),
        "{chunks:?}"
    );
    if let Chunk::Meta {
        channel,
        request_id,
        ..
    } = &chunks[0]
    {
        assert_eq!(*channel, Channel::Byok);
        assert!(request_id.starts_with("byok_"), "{request_id}");
    }
    assert_eq!(done_count(&chunks), 1, "[DONE] 必须被去重：{chunks:?}");
    assert!(matches!(chunks.last(), Some(Chunk::Done { .. })));
    assert_eq!(
        outcome.usage.tokens_in, 42,
        "usage 同事件也要拿到：{outcome:?}"
    );
    assert_eq!(outcome.usage.tokens_out, 7);
    assert!(!outcome.usage.estimated);
    assert_eq!(outcome.finish, FinishReason::Stop);
    assert!(!outcome.cancelled);
    assert_eq!(outcome.request_id, None);

    // 请求侧：端点、鉴权、请求体
    let captured = server.captured();
    assert_eq!(captured.path(), "/v1/chat/completions");
    assert_eq!(
        captured.header("authorization"),
        Some("Bearer sk-test-123456")
    );
    assert_eq!(captured.header("accept"), Some("text/event-stream"));
    let body = captured.json();
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["stream_options"]["include_usage"], true,
        "默认要开 usage 回报：{body}"
    );
}

#[tokio::test]
async fn byok_anthropic_uses_x_api_key_and_merges_usage() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(sse_data(
                r#"{"type":"message_start","message":{"usage":{"input_tokens":1200,"output_tokens":1}}}"#,
            )),
            Step::Write(sse(
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"下午没课"}}"#,
            )),
            Step::Write(sse(
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#,
            )),
            Step::Write(sse("message_stop", r#"{"type":"message_stop"}"#)),
            Step::Close,
        ],
    )
    .await;

    let provider = byok(&server, Protocol::AnthropicMessages);
    let req = request(Protocol::AnthropicMessages);
    let (chunks, outcome) = run(&provider, &req, None).await;
    let outcome = outcome.unwrap();

    assert_eq!(deltas(&chunks), "下午没课");
    assert_eq!(done_count(&chunks), 1);
    // input 在 message_start、output 在 message_delta：合并后两个都在
    assert_eq!(outcome.usage.tokens_in, 1200);
    assert_eq!(outcome.usage.tokens_out, 15);
    assert!(!outcome.usage.estimated);

    let captured = server.captured();
    assert_eq!(captured.path(), "/v1/messages");
    assert_eq!(captured.header("x-api-key"), Some("sk-test-123456"));
    assert_eq!(captured.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(
        captured.header("authorization"),
        None,
        "Anthropic 不能带 Bearer"
    );
    // 系统提示必须在顶层
    let body = captured.json();
    assert_eq!(body["system"], "你是助手");
}

#[tokio::test]
async fn byok_responses_completed_carries_usage_and_finishes_once() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(sse(
                "response.output_text.delta",
                r#"{"type":"response.output_text.delta","delta":"下午没课"}"#,
            )),
            Step::Write(sse(
                "response.completed",
                r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":42,"output_tokens":7}}}"#,
            )),
            Step::Close,
        ],
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiResponses);
    let req = request(Protocol::OpenaiResponses);
    let (chunks, outcome) = run(&provider, &req, None).await;
    let outcome = outcome.unwrap();

    assert_eq!(deltas(&chunks), "下午没课");
    assert_eq!(done_count(&chunks), 1);
    assert_eq!(outcome.usage.tokens_in, 42);
    assert_eq!(outcome.usage.tokens_out, 7);
}

#[tokio::test]
async fn byok_usage_fallback_when_upstream_omits_it() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(completions_delta("下午没课")),
            Step::Write(sse_data(
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            )),
            Step::Close,
        ],
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiCompletions);
    let req = request(Protocol::OpenaiCompletions);
    let (_, outcome) = run(&provider, &req, None).await;
    let outcome = outcome.unwrap();

    assert!(outcome.usage.estimated, "没 usage 必须标估算：{outcome:?}");
    assert_eq!(outcome.usage.tokens_in, 32, "用请求里的输入估算");
    assert!(outcome.usage.tokens_out > 0, "用实际输出估算");
}

#[tokio::test]
async fn byok_stream_without_done_is_closed_with_synthesized_done() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![Step::Write(completions_delta("半截")), Step::Close],
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiCompletions);
    let req = request(Protocol::OpenaiCompletions);
    let (chunks, outcome) = run(&provider, &req, None).await;
    let outcome = outcome.unwrap();

    assert_eq!(deltas(&chunks), "半截");
    assert_eq!(done_count(&chunks), 1, "必须补一个 Done：{chunks:?}");
    assert!(matches!(chunks.last(), Some(Chunk::Done { .. })));
    assert_eq!(outcome.finish, FinishReason::Stop);
}

// ---------------------------------------------------------------------------
// BYOK：错误与超时
// ---------------------------------------------------------------------------

#[tokio::test]
async fn byok_upstream_error_status_maps_to_remote() {
    let server = FakeServer::reply(
        429,
        "application/json",
        r#"{"error":{"message":"rate limited; slow down","type":"rate_limit"}}"#,
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiCompletions);
    let req = request(Protocol::OpenaiCompletions);
    let (chunks, outcome) = run(&provider, &req, None).await;

    assert!(chunks.is_empty(), "非 2xx 不该产生 chunk");
    match outcome.unwrap_err() {
        CoreError::Remote {
            code,
            message,
            retry_after,
        } => {
            assert_eq!(code, yunong_shared::ErrorCode::RateLimited);
            assert!(message.contains("rate limited"), "{message}");
            assert_eq!(retry_after, None);
        }
        other => panic!("应为 Remote：{other:?}"),
    }
}

#[tokio::test]
async fn byok_retry_after_is_parsed() {
    let server = FakeServer::reply_with(
        429,
        "application/json",
        vec![("retry-after", "7".to_owned())],
        r#"{"error":{"message":"slow"}}"#,
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiCompletions);
    let (_, outcome) = run(&provider, &request(Protocol::OpenaiCompletions), None).await;
    match outcome.unwrap_err() {
        CoreError::Remote { retry_after, .. } => {
            assert_eq!(retry_after, Some(Duration::from_secs(7)))
        }
        other => panic!("应为 Remote：{other:?}"),
    }
}

#[tokio::test]
async fn byok_401_maps_to_unauthorized() {
    let server = FakeServer::reply(
        401,
        "application/json",
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid api key"}}"#,
    )
    .await;

    let provider = byok(&server, Protocol::AnthropicMessages);
    let (_, outcome) = run(&provider, &request(Protocol::AnthropicMessages), None).await;
    match outcome.unwrap_err() {
        CoreError::Remote { code, message, .. } => {
            assert_eq!(code, yunong_shared::ErrorCode::Unauthorized);
            assert!(message.contains("invalid api key"), "{message}");
        }
        other => panic!("应为 Remote：{other:?}"),
    }
}

#[tokio::test]
async fn byok_html_response_is_rejected_before_parsing() {
    let server = FakeServer::reply(200, "text/html", "<html>登录页</html>").await;

    let provider = byok(&server, Protocol::OpenaiCompletions);
    let (_, outcome) = run(&provider, &request(Protocol::OpenaiCompletions), None).await;
    match outcome.unwrap_err() {
        CoreError::Remote { message, .. } => {
            assert!(message.contains("text/event-stream"), "{message}");
        }
        other => panic!("应为 Remote：{other:?}"),
    }
}

#[tokio::test]
async fn byok_first_event_timeout() {
    // 响应头立刻到，但一个字节都不发
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![Step::Sleep(Duration::from_secs(3))],
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiCompletions).with_timeouts(fast_timeouts(150));
    let (_, outcome) = run(&provider, &request(Protocol::OpenaiCompletions), None).await;
    match outcome.unwrap_err() {
        CoreError::Timeout(message) => assert!(message.contains("没有返回任何数据"), "{message}"),
        other => panic!("应为 Timeout：{other:?}"),
    }
}

#[tokio::test]
async fn byok_idle_timeout_between_events() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(completions_delta("开头")),
            Step::Sleep(Duration::from_secs(3)),
        ],
    )
    .await;

    // 首帧给足时间，空闲收紧到 150ms
    let provider = byok(&server, Protocol::OpenaiCompletions).with_timeouts(Timeouts {
        connect: Duration::from_secs(2),
        first_event: Duration::from_secs(2),
        idle: Duration::from_millis(150),
    });
    let (chunks, outcome) = run(&provider, &request(Protocol::OpenaiCompletions), None).await;
    assert_eq!(deltas(&chunks), "开头");
    match outcome.unwrap_err() {
        CoreError::Timeout(message) => assert!(message.contains("空闲"), "{message}"),
        other => panic!("应为 Timeout：{other:?}"),
    }
}

#[tokio::test]
async fn byok_missing_key_is_secret_missing() {
    let provider = DirectProvider::new(Protocol::OpenaiCompletions, "http://127.0.0.1:1", "m");
    let (_, outcome) = run(&provider, &request(Protocol::OpenaiCompletions), None).await;
    assert!(matches!(
        outcome.unwrap_err(),
        CoreError::SecretMissing("ApiKey")
    ));
}

#[tokio::test]
async fn api_key_debug_is_masked() {
    let source = ApiKeySource::Inline("sk-secret-abcdef".into());
    let rendered = format!("{source:?}");
    assert!(
        !rendered.contains("secret"),
        "Key 不能出现在 Debug 里：{rendered}"
    );
    assert!(rendered.contains("sk-s"), "{rendered}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// 取消：必须是"真的停止消费上游"，不是"读完再丢"
// ---------------------------------------------------------------------------

/// 大流量灌入：客户端一旦停止读取，服务端的写很快就会被 TCP 窗口卡住。
const FLOOD_BYTES: usize = 32 * 1024 * 1024;

#[tokio::test]
async fn byok_flow_stop_stops_consuming_upstream() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(completions_delta("一")),
            Step::Flood { bytes: FLOOD_BYTES },
        ],
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiCompletions);
    let started = tokio::time::Instant::now();
    // 收到 Meta + 第一个 delta 就取消（服务端后面还有 32MB 要灌）
    let (chunks, outcome) = run(&provider, &request(Protocol::OpenaiCompletions), Some(2)).await;
    let outcome = outcome.unwrap();

    assert!(outcome.cancelled, "回调 Stop 必须标记取消：{outcome:?}");
    assert!(chunks.len() >= 2, "{chunks:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "取消必须立刻返回，不能把上游读完：{:?}",
        started.elapsed()
    );
    // 取消后上游继续灌 32MB：客户端若还在消费，这里会被灌完
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !server.flood_finished(),
        "取消后仍在消费上游数据（已灌 {}/{} 字节），等于没取消",
        server.flooded_bytes(),
        server.flood_target()
    );
}

#[tokio::test]
async fn cloud_flow_stop_stops_consuming_upstream() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(sse("meta", r#"{"request_id":"req_1","model":"m"}"#)),
            Step::Write(sse("delta", r#"{"text":"一"}"#)),
            Step::Flood { bytes: FLOOD_BYTES },
        ],
    )
    .await;

    let proxy = cloud(&server);
    let started = tokio::time::Instant::now();
    let (_, outcome) = run(&proxy, &request(Protocol::OpenaiCompletions), Some(2)).await;
    assert!(outcome.unwrap().cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !server.flood_finished(),
        "取消后仍在消费上游数据（已灌 {}/{} 字节）",
        server.flooded_bytes(),
        server.flood_target()
    );
}

#[tokio::test]
async fn dropping_the_future_stops_consuming_upstream() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(completions_delta("一")),
            Step::Flood { bytes: FLOOD_BYTES },
        ],
    )
    .await;

    let provider = Arc::new(byok(&server, Protocol::OpenaiCompletions));
    let req = Arc::new(request(Protocol::OpenaiCompletions));
    let seen = Arc::new(Mutex::new(0_usize));

    let task = {
        let provider = Arc::clone(&provider);
        let req = Arc::clone(&req);
        let seen = Arc::clone(&seen);
        tokio::spawn(async move {
            let mut sink = |_chunk: Chunk| {
                *seen.lock().unwrap() += 1;
                Flow::Continue
            };
            let _ = provider.chat_stream(&req, &mut sink).await;
        })
    };

    // 等到确实收到过 chunk，再模拟"客户端断开"（drop future）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while *seen.lock().unwrap() == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(*seen.lock().unwrap() > 0, "应当已经收到 chunk");
    task.abort();
    let _ = task.await;

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !server.flood_finished(),
        "drop future 后仍在消费上游数据（已灌 {}/{} 字节）",
        server.flooded_bytes(),
        server.flood_target()
    );
}

// ---------------------------------------------------------------------------
// 套餐通道：成功 / 错误 / 超时
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cloud_stream_parses_contract_events_in_order() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(sse(
                "meta",
                r#"{"request_id":"req_9f3c","model":"deepseek-chat","channel":"plan"}"#,
            )),
            Step::Write(sse("ping", "{}")),
            Step::Write(sse("delta", r#"{"text":"下周三下午"}"#)),
            Step::Write(sse(
                "tool_call",
                r#"{"id":"c1","name":"get_courses","arguments":{"weekday":3}}"#,
            )),
            Step::Write(sse(
                "usage",
                r#"{"tokens_in":3120,"tokens_out":96,"estimated":false}"#,
            )),
            Step::Write(sse("done", r#"{"finish":"stop","cost_cents":1}"#)),
            Step::Close,
        ],
    )
    .await;

    let proxy = cloud(&server);
    let req = request(Protocol::OpenaiCompletions);
    let (chunks, outcome) = run(&proxy, &req, None).await;
    let outcome = outcome.unwrap();

    // Meta 必首，且用服务端给的 request_id
    assert!(
        matches!(&chunks[0], Chunk::Meta { request_id, .. } if request_id == "req_9f3c"),
        "{chunks:?}"
    );
    assert_eq!(deltas(&chunks), "下周三下午");
    assert!(chunks.iter().any(|chunk| matches!(chunk, Chunk::Ping)));
    assert!(
        chunks.iter().any(|chunk| matches!(
            chunk,
            Chunk::ToolCall { id, name, .. } if id == "c1" && name == "get_courses"
        )),
        "{chunks:?}"
    );
    assert_eq!(done_count(&chunks), 1);
    assert!(matches!(chunks.last(), Some(Chunk::Done { .. })));
    assert_eq!(outcome.usage.tokens_in, 3120);
    assert_eq!(outcome.usage.tokens_out, 96);
    assert_eq!(outcome.request_id.as_deref(), Some("req_9f3c"));
    assert_eq!(outcome.finish, FinishReason::Stop);

    // 请求侧
    let captured = server.captured();
    assert_eq!(captured.path(), "/api/v1/chat/stream");
    assert_eq!(captured.header("authorization"), Some("Bearer tok-abc"));
    let body = captured.json();
    assert_eq!(body["session_id"], "s_1");
    assert_eq!(body["turn_id"], "t_1");
}

#[tokio::test]
async fn cloud_error_envelope_keeps_code_and_retry_after() {
    let server = FakeServer::reply_with(
        429,
        "application/json",
        vec![("retry-after", "5".to_owned())],
        r#"{"ok":false,"error":{"code":"CONCURRENCY_LIMIT","message":"同一 turn 已有在途请求","request_id":"req_1"}}"#,
    )
    .await;

    let proxy = cloud(&server);
    let (chunks, outcome) = run(&proxy, &request(Protocol::OpenaiCompletions), None).await;
    assert!(chunks.is_empty());
    match outcome.unwrap_err() {
        CoreError::Remote {
            code,
            message,
            retry_after,
        } => {
            assert_eq!(code, yunong_shared::ErrorCode::ConcurrencyLimit);
            assert!(message.contains("在途请求"), "{message}");
            assert_eq!(retry_after, Some(Duration::from_secs(5)));
        }
        other => panic!("应为 Remote：{other:?}"),
    }
}

#[tokio::test]
async fn cloud_504_maps_to_upstream_timeout() {
    let server = FakeServer::reply(
        504,
        "application/json",
        r#"{"ok":false,"error":{"code":"UPSTREAM_TIMEOUT","message":"上游超时","request_id":"r"}}"#,
    )
    .await;

    let proxy = cloud(&server);
    let (_, outcome) = run(&proxy, &request(Protocol::OpenaiCompletions), None).await;
    match outcome.unwrap_err() {
        CoreError::Remote { code, .. } => {
            assert_eq!(code, yunong_shared::ErrorCode::UpstreamTimeout)
        }
        other => panic!("应为 Remote：{other:?}"),
    }
}

#[tokio::test]
async fn cloud_error_event_terminates_without_done() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(sse("meta", r#"{"request_id":"req_2"}"#)),
            Step::Write(sse("delta", r#"{"text":"半句"}"#)),
            Step::Write(sse(
                "error",
                r#"{"code":"REVIEW_REJECTED","message":"内容不合规"}"#,
            )),
            Step::Close,
        ],
    )
    .await;

    let proxy = cloud(&server);
    let (chunks, outcome) = run(&proxy, &request(Protocol::OpenaiCompletions), None).await;
    let outcome = outcome.unwrap();

    assert!(matches!(
        chunks.last(),
        Some(Chunk::Error {
            code: yunong_shared::ErrorCode::ReviewRejected,
            ..
        })
    ));
    assert_eq!(done_count(&chunks), 0, "错误后不应再有 Done：{chunks:?}");
    assert_eq!(outcome.finish, FinishReason::Error);
}

#[tokio::test]
async fn cloud_stream_without_done_gets_synthesized_done() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(sse("meta", r#"{"request_id":"req_3"}"#)),
            Step::Write(sse("delta", r#"{"text":"半句"}"#)),
            // 服务端老版本/中间层：不发 done 就断了
            Step::Close,
        ],
    )
    .await;

    let proxy = cloud(&server);
    let (chunks, outcome) = run(&proxy, &request(Protocol::OpenaiCompletions), None).await;
    let outcome = outcome.unwrap();

    assert_eq!(deltas(&chunks), "半句");
    assert_eq!(done_count(&chunks), 1, "{chunks:?}");
    assert!(matches!(chunks.last(), Some(Chunk::Done { .. })));
    assert_eq!(outcome.finish, FinishReason::Stop);
    assert_eq!(outcome.request_id.as_deref(), Some("req_3"));
}

#[tokio::test]
async fn cloud_first_event_timeout() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![Step::Sleep(Duration::from_secs(3))],
    )
    .await;

    let proxy = CloudProxy::new(server.base_url(), "tok").with_timeouts(fast_timeouts(150));
    let (_, outcome) = run(&proxy, &request(Protocol::OpenaiCompletions), None).await;
    match outcome.unwrap_err() {
        CoreError::Timeout(message) => assert!(message.contains("没有返回任何数据"), "{message}"),
        other => panic!("应为 Timeout：{other:?}"),
    }
}

#[tokio::test]
async fn cloud_idle_timeout_between_events() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(sse("meta", r#"{"request_id":"req_4"}"#)),
            Step::Write(sse("delta", r#"{"text":"开头"}"#)),
            Step::Sleep(Duration::from_secs(3)),
        ],
    )
    .await;

    let proxy = CloudProxy::new(server.base_url(), "tok").with_timeouts(Timeouts {
        connect: Duration::from_secs(2),
        first_event: Duration::from_secs(2),
        idle: Duration::from_millis(150),
    });
    let (_, outcome) = run(&proxy, &request(Protocol::OpenaiCompletions), None).await;
    match outcome.unwrap_err() {
        CoreError::Timeout(message) => assert!(message.contains("空闲"), "{message}"),
        other => panic!("应为 Timeout：{other:?}"),
    }
}

#[tokio::test]
async fn cloud_html_response_is_rejected() {
    let server = FakeServer::reply(200, "text/html", "<html>proxy login</html>").await;
    let proxy = cloud(&server);
    let (_, outcome) = run(&proxy, &request(Protocol::OpenaiCompletions), None).await;
    assert!(matches!(outcome.unwrap_err(), CoreError::Remote { .. }));
}

// ---------------------------------------------------------------------------
// 装配器语义（跨两条通道的公共约定）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn collect_chunks_returns_contract_legal_sequence() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![
            Step::Write(sse("meta", r#"{"request_id":"req_5"}"#)),
            Step::Write(sse("delta", r#"{"text":"好"}"#)),
            Step::Write(sse(
                "usage",
                r#"{"tokens_in":10,"tokens_out":2,"estimated":false}"#,
            )),
            Step::Write(sse("done", r#"{"finish":"stop"}"#)),
            Step::Close,
        ],
    )
    .await;

    let proxy = cloud(&server);
    let req = request(Protocol::OpenaiCompletions);
    let (chunks, outcome) = collect_chunks(&proxy, &req).await.unwrap();

    assert!(matches!(chunks.first(), Some(Chunk::Meta { .. })));
    assert!(matches!(chunks.last(), Some(Chunk::Done { .. })));
    assert_eq!(done_count(&chunks), 1);
    assert_eq!(outcome.usage.tokens_in, 10);
    assert_eq!(outcome.usage.tokens_out, 2);
}

#[test]
fn timeouts_defaults_are_sane() {
    let defaults = Timeouts::default();
    assert_eq!(defaults.connect, Duration::from_secs(10));
    assert_eq!(
        defaults.first_event,
        Duration::from_secs(90),
        "推理模型首 token 可能 30~60s"
    );
    assert_eq!(defaults.idle, Duration::from_secs(30), "服务端 15s 心跳");
}

/// `with_usage_reporting(false)`：请求体里**不能**出现 `stream_options`
/// （第三方兼容网关可能因为未知字段直接 400）。
#[tokio::test]
async fn byok_usage_reporting_switch_controls_stream_options() {
    let server = FakeServer::start(
        200,
        "text/event-stream",
        Vec::new(),
        vec![Step::Write(completions_finish()), Step::Close],
    )
    .await;

    let provider = byok(&server, Protocol::OpenaiCompletions).with_usage_reporting(false);
    let (_, outcome) = run(&provider, &request(Protocol::OpenaiCompletions), None).await;
    assert!(outcome.is_ok());

    let body = server.captured().json();
    assert!(
        body.get("stream_options").is_none(),
        "关掉之后不能带 stream_options：{body}"
    );
}
