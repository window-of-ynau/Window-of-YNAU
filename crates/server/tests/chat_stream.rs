//! 套餐通道 SSE 代理的集成测试（ADR-03 / ADR-05）。
//!
//! 这里**不 mock 网络层**：真起一个 axum 上游（[`Mock`]），服务端用真实的
//! TCP + HTTP/1.1 + SSE 解码转发，再把上游事件装配成我们自己的 SSE。
//!
//! 覆盖验收要求的四条：
//!
//! 1. 无 token → 401（且不会打上游）；
//! 2. 合法 token → 正常流（Meta 必首、Done 必末且唯一、usage 合并）；
//! 3. 并发超限 → 429 `CONCURRENCY_LIMIT` + `Retry-After`；
//! 4. 客户端断开 → 取消上游（mock 立刻停止产出）且闸门名额归还。
//!
//! 外加错误映射（上游 5xx → 502、上游超时 → 504）与「未配置上游」的行为。
//!
//! 文末还有三条**需要 PostgreSQL** 的用例（默认 `#[ignore]`，见 [`common`]）：
//! 抽样落库只写一行终值、记账写失败不影响流、闸门拒绝留审计。

mod common;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, State as AxumState};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use futures_core::Stream;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use sqlx::Row;
use tokio::sync::mpsc;
use tower::ServiceExt;

use common::{count, TestDb};
use yunong_server::config::Config;
use yunong_server::state::AppState;
use yunong_server::upstream::UpstreamConfig;

const PASSWORD: &str = "correct-horse-battery";

// ============ mock 上游 ============

/// mock 上游的剧本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    /// 正常 SSE：文本增量 + 「usage 与结束原因同帧」+ `[DONE]`
    CompletionsOk,
    /// Anthropic Messages 的正常 SSE（验证第二套协议与鉴权头）
    MessagesOk,
    /// 直接 500
    ServerError,
    /// 迟迟不给响应头（触发 504）
    SlowHeaders,
    /// 一直产出，等客户端断开（验证取消；真实厂商也是如此）
    Endless,
}

#[derive(Debug, Default)]
struct Counters {
    requests: AtomicUsize,
    delivered: AtomicUsize,
    aborted: AtomicUsize,
    finished: AtomicUsize,
}

/// 上游收到的请求（验证我们的转发是否规范）。
#[derive(Debug, Clone)]
struct Received {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

#[derive(Clone)]
struct MockCtx {
    script: Script,
    counters: Arc<Counters>,
    received: Arc<Mutex<Vec<Received>>>,
}

/// 一个跑在 127.0.0.1 随机端口上的 mock 上游。
struct Mock {
    addr: SocketAddr,
    counters: Arc<Counters>,
    received: Arc<Mutex<Vec<Received>>>,
}

impl Mock {
    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> usize {
        self.counters.requests.load(Ordering::SeqCst)
    }

    fn delivered(&self) -> usize {
        self.counters.delivered.load(Ordering::SeqCst)
    }

    fn aborted(&self) -> usize {
        self.counters.aborted.load(Ordering::SeqCst)
    }

    fn finished(&self) -> usize {
        self.counters.finished.load(Ordering::SeqCst)
    }

    fn last_request(&self) -> Received {
        self.received
            .lock()
            .expect("mock 记录锁")
            .last()
            .cloned()
            .expect("上游应当收到过请求")
    }
}

async fn start_mock(script: Script) -> Mock {
    let counters = Arc::new(Counters::default());
    let received = Arc::new(Mutex::new(Vec::new()));
    let ctx = MockCtx {
        script,
        counters: Arc::clone(&counters),
        received: Arc::clone(&received),
    };
    // 三家端点都挂上，这样也能验证"协议 → 端点"的映射
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_handler))
        .route("/v1/responses", post(mock_handler))
        .route("/v1/messages", post(mock_handler))
        .with_state(ctx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定 mock 上游端口");
    let addr = listener.local_addr().expect("取 mock 上游地址");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock 上游应能运行");
    });
    Mock {
        addr,
        counters,
        received,
    }
}

async fn mock_handler(
    AxumState(ctx): AxumState<MockCtx>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    ctx.counters.requests.fetch_add(1, Ordering::SeqCst);
    let recorded = Received {
        path: uri.path().to_owned(),
        headers: headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect(),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    ctx.received.lock().expect("mock 记录锁").push(recorded);

    match ctx.script {
        Script::ServerError => (StatusCode::INTERNAL_SERVER_ERROR, "upstream boom").into_response(),
        Script::SlowHeaders => {
            // 比客户端的读超时更久：服务端应自己判 504，而不是一直挂着
            tokio::time::sleep(Duration::from_secs(10)).await;
            sse_response(Arc::clone(&ctx.counters), completions_frames())
        }
        Script::CompletionsOk => sse_response(Arc::clone(&ctx.counters), completions_frames()),
        Script::MessagesOk => sse_response(Arc::clone(&ctx.counters), messages_frames()),
        Script::Endless => sse_response(Arc::clone(&ctx.counters), endless_frames()),
    }
}

/// 每 10ms 发一帧，一直发到客户端断开为止（`endless_frames` 提供上限）。
fn sse_response(counters: Arc<Counters>, frames: Vec<String>) -> Response {
    let (sender, receiver) = mpsc::channel::<Bytes>(4);
    tokio::spawn(async move {
        for frame in frames {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if sender.send(Bytes::from(frame)).await.is_err() {
                // 对端关闭：真实厂商也是这么停下生成的
                counters.aborted.fetch_add(1, Ordering::SeqCst);
                return;
            }
            counters.delivered.fetch_add(1, Ordering::SeqCst);
        }
        counters.finished.fetch_add(1, Ordering::SeqCst);
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(TestStream { receiver }))
        .expect("mock 响应合法")
}

/// 极小的 `Stream` 包装（`Body::from_stream` 要的就是 `futures_core::Stream`）。
struct TestStream {
    receiver: mpsc::Receiver<Bytes>,
}

impl Stream for TestStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx).map(|next| next.map(Ok))
    }
}

fn sse(data: &str) -> String {
    format!("data: {data}\n\n")
}

fn sse_event(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// Completions 的正常流：注意「`usage` 与 `finish_reason` 同帧」以及后面的 `[DONE]`——
/// 这正是契约里要求去重的地方（一个 Done，不是两个）。
fn completions_frames() -> Vec<String> {
    vec![
        sse(
            r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
        ),
        sse(
            r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"你好"}}]}"#,
        ),
        sse(
            r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"，同学"}}]}"#,
        ),
        sse(
            r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":312,"completion_tokens":96}}"#,
        ),
        sse("[DONE]"),
    ]
}

/// Anthropic Messages 的正常流：`message_start` 给 input_tokens（output 只是占位值），
/// `message_delta` 同时给 `stop_reason` 与真实的 output_tokens。
fn messages_frames() -> Vec<String> {
    vec![
        sse_event(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet","usage":{"input_tokens":42,"output_tokens":1}}}"#,
        ),
        sse_event(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        sse_event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}"#,
        ),
        sse_event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        sse_event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#,
        ),
        sse_event("message_stop", r#"{"type":"message_stop"}"#),
    ]
}

fn endless_frames() -> Vec<String> {
    (0..200)
        .map(|index| {
            sse(&format!(
                r#"{{"id":"c1","object":"chat.completion.chunk","choices":[{{"index":0,"delta":{{"content":"片{index}"}}}}]}}"#
            ))
        })
        .collect()
}

// ============ 被测服务 ============

struct TestApp {
    state: AppState,
}

impl TestApp {
    fn new(mock: &Mock) -> Self {
        Self::with_config(Self::config(mock))
    }

    fn config(mock: &Mock) -> Config {
        let mut config = Config::for_tests();
        config.upstream = UpstreamConfig::single(mock.base_url(), "test-key");
        config
    }

    fn with_config(config: Config) -> Self {
        Self::with_db(config, None)
    }

    /// 接真库启动（记账相关用例用；此时鉴权后端也会切到 PostgreSQL）。
    fn with_db(config: Config, db: Option<PgPool>) -> Self {
        Self {
            state: AppState::new(config, db),
        }
    }

    async fn call(&self, request: Request<Body>) -> Response {
        yunong_server::app(self.state.clone())
            .oneshot(request)
            .await
            .expect("服务应能处理请求")
    }

    /// 注册一个账号并拿到访问令牌（无数据库模式 → 内存后端）。
    async fn register(&self, email: &str) -> String {
        let response = self
            .call(json_request(
                "POST",
                "/api/v1/auth/register",
                &json!({ "email": email, "password": PASSWORD }).to_string(),
                None,
            ))
            .await;
        assert_eq!(response.status(), StatusCode::CREATED, "注册应成功");
        let body = read_json(response).await;
        body["data"]["access_token"]
            .as_str()
            .expect("注册应返回 access_token")
            .to_owned()
    }
}

fn json_request(method: &str, uri: &str, body: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::from(body.to_owned())).expect("构造请求")
}

fn chat_body(turn_id: &str) -> String {
    json!({
        "session_id": "s_1",
        "turn_id": turn_id,
        "round": 1,
        "model": "mock-model",
        "protocol": "openai_completions",
        "messages": [{"role": "user", "content": "你好"}],
        "tools": [],
        "max_output_tokens": 128,
        "stream": true,
        "estimated_input_tokens": 12
    })
    .to_string()
}

fn chat_request(token: Option<&str>, turn_id: &str) -> Request<Body> {
    json_request("POST", "/api/v1/chat/stream", &chat_body(turn_id), token)
}

fn with_protocol(mut body: String, protocol: &str) -> String {
    body = body.replace("\"openai_completions\"", &format!("\"{protocol}\""));
    body
}

async fn read_json(response: Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("读响应体")
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// 把整条 SSE 流读成 `(event, data)` 列表。
async fn read_events(response: Response) -> Vec<(String, String)> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("读 SSE 流")
        .to_bytes();
    parse_sse(&bytes)
}

fn parse_sse(bytes: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8(bytes.to_vec()).expect("SSE 必须是 UTF-8");
    let mut events = Vec::new();
    let mut event = String::new();
    let mut data: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !data.is_empty() {
                events.push((std::mem::take(&mut event), data.join("\n")));
                data.clear();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.trim().to_owned());
        }
    }
    events
}

fn json_of(events: &[(String, String)], name: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|(event, _)| event == name)
        .map(|(_, data)| serde_json::from_str(data).expect("事件 data 必须是 JSON"))
        .collect()
}

fn header_str<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

async fn wait_until(mut condition: impl FnMut() -> bool, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    condition()
}

/// 轮询等待"某条 count SQL 至少到 `expected`"（记账是 fire-and-forget，落库有延迟）。
async fn wait_for_rows(db: &TestDb, sql: &str, expected: i64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = count(&db.pool, sql).await;
        if rows >= expected || Instant::now() >= deadline {
            return rows >= expected;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ============ 用例 ============

#[tokio::test]
async fn rejects_without_bearer_token() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::new(&mock);

    let response = app.call(chat_request(None, "turn-1")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let body = read_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    assert!(
        body["error"]["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "错误信封必须带 request_id：{body}"
    );
    assert_eq!(mock.requests(), 0, "没有令牌时不该打到上游");
}

#[tokio::test]
async fn streams_meta_first_done_last_and_merges_usage() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_str(&response, "content-type"),
        Some("text/event-stream; charset=utf-8")
    );
    assert_eq!(header_str(&response, "cache-control"), Some("no-cache"));
    assert_eq!(
        header_str(&response, "x-accel-buffering"),
        Some("no"),
        "要显式禁用代理缓冲"
    );
    let request_id = header_str(&response, "x-request-id")
        .expect("必须回写 x-request-id")
        .to_owned();

    let events = read_events(response).await;
    let names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(names.first().copied(), Some("meta"), "Meta 必首：{names:?}");
    assert_eq!(names.last().copied(), Some("done"), "Done 必末：{names:?}");
    assert_eq!(
        names.iter().filter(|name| **name == "meta").count(),
        1,
        "Meta 唯一：{names:?}"
    );
    assert_eq!(
        names.iter().filter(|name| **name == "done").count(),
        1,
        "Completions 的 [DONE] 会造成两个 Done，装配器必须去重：{names:?}"
    );
    assert_eq!(
        names.iter().filter(|name| **name == "usage").count(),
        1,
        "usage 与结束原因同帧时也只应出现一条 usage：{names:?}"
    );

    let meta = json_of(&events, "meta");
    assert_eq!(meta[0]["type"], "meta");
    assert_eq!(meta[0]["channel"], "plan");
    assert_eq!(meta[0]["model"], "mock-model");
    assert_eq!(
        meta[0]["request_id"], request_id,
        "meta.request_id 要等于响应头"
    );

    let text: String = json_of(&events, "delta")
        .iter()
        .filter_map(|chunk| chunk["text"].as_str())
        .collect();
    assert_eq!(text, "你好，同学", "文本增量顺序不能乱");

    let usage = json_of(&events, "usage");
    assert_eq!(usage[0]["tokens_in"], 312);
    assert_eq!(usage[0]["tokens_out"], 96);
    assert_eq!(usage[0]["estimated"], false);

    let done = json_of(&events, "done");
    assert_eq!(done[0]["finish"], "stop", "结束原因来自 finish_reason 帧");

    // 上游确实按契约收到了请求
    let received = mock.last_request();
    assert_eq!(received.path, "/v1/chat/completions");
    assert_eq!(
        received.headers.get("authorization").map(String::as_str),
        Some("Bearer test-key"),
        "Completions 用 Bearer"
    );
    assert_eq!(
        received.headers.get("accept").map(String::as_str),
        Some("text/event-stream")
    );
    assert_eq!(received.body["model"], "mock-model");
    assert_eq!(received.body["stream"], true);
    assert_eq!(mock.requests(), 1);
}

#[tokio::test]
async fn anthropic_protocol_uses_messages_endpoint_and_x_api_key() {
    let mock = start_mock(Script::MessagesOk).await;
    let app = TestApp::new(&mock);
    let token = app.register("claude@ynau.edu.cn").await;

    let body = with_protocol(chat_body("turn-messages"), "anthropic_messages");
    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &body,
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let events = read_events(response).await;
    let names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(names.first().copied(), Some("meta"), "{names:?}");
    assert_eq!(names.last().copied(), Some("done"), "{names:?}");
    assert_eq!(names.iter().filter(|name| **name == "done").count(), 1);

    let text: String = json_of(&events, "delta")
        .iter()
        .filter_map(|chunk| chunk["text"].as_str())
        .collect();
    assert_eq!(text, "你好");

    // usage 是"按字段取 max"合并出来的：input 来自 message_start，output 来自 message_delta
    let usage = json_of(&events, "usage");
    assert_eq!(usage[0]["tokens_in"], 42);
    assert_eq!(
        usage[0]["tokens_out"], 15,
        "真实 output 在 message_delta 里"
    );

    let done = json_of(&events, "done");
    assert_eq!(done[0]["finish"], "stop", "end_turn → Stop");

    let received = mock.last_request();
    assert_eq!(received.path, "/v1/messages");
    assert_eq!(
        received.headers.get("x-api-key").map(String::as_str),
        Some("test-key"),
        "Anthropic 用 x-api-key"
    );
    assert_eq!(
        received
            .headers
            .get("anthropic-version")
            .map(String::as_str),
        Some("2023-06-01")
    );
    assert!(
        !received.headers.contains_key("authorization"),
        "Anthropic 不该带 Bearer"
    );
}

#[tokio::test]
async fn global_concurrency_limit_returns_429_with_retry_after() {
    let mock = start_mock(Script::Endless).await;
    let mut config = TestApp::config(&mock);
    config.concurrency_limit = 1; // 全局只有 1 个在途 turn
    let app = TestApp::with_config(config);

    let alice = app.register("alice@ynau.edu.cn").await;
    let bob = app.register("bob@ynau.edu.cn").await;

    // 第一条流拿住档位（响应体不读也不丢 → 生产任务仍在跑）
    let held = app.call(chat_request(Some(&alice), "turn-a")).await;
    assert_eq!(held.status(), StatusCode::OK);

    let rejected = app.call(chat_request(Some(&bob), "turn-b")).await;
    assert_eq!(
        rejected.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "全局档位满了必须 429"
    );
    assert_eq!(
        header_str(&rejected, "retry-after"),
        Some("1"),
        "429 必须带 Retry-After"
    );
    let body = read_json(rejected).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "CONCURRENCY_LIMIT");
    assert!(
        body["error"]["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{body}"
    );

    // 放掉第一条流之后，档位应该立刻可复用
    drop(held);
    assert!(
        wait_until(
            {
                let state = app.state.clone();
                move || state.gate.in_flight() == 0
            },
            Duration::from_secs(2)
        )
        .await,
        "断开后闸门名额必须归还"
    );
    let after = app.call(chat_request(Some(&bob), "turn-b")).await;
    assert_eq!(after.status(), StatusCode::OK, "名额归还后应能重新开流");
}

#[tokio::test]
async fn per_user_limit_allows_only_one_in_flight_turn() {
    let mock = start_mock(Script::Endless).await;
    let app = TestApp::new(&mock); // 全局 50，只可能被"单用户 1 个"挡住
    let alice = app.register("alice@ynau.edu.cn").await;

    let held = app.call(chat_request(Some(&alice), "turn-a")).await;
    assert_eq!(held.status(), StatusCode::OK);

    let second = app.call(chat_request(Some(&alice), "turn-b")).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = read_json(second).await;
    assert_eq!(body["error"]["code"], "CONCURRENCY_LIMIT");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("1 个在途")),
        "措辞要能指导用户：{body}"
    );

    // 同一个 turn 重复提交，也要被挡住（不是"新开一条流"）
    let duplicate = app.call(chat_request(Some(&alice), "turn-a")).await;
    assert_eq!(duplicate.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = read_json(duplicate).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("同一 turn")),
        "{body}"
    );

    drop(held);
}

#[tokio::test]
async fn upstream_5xx_maps_to_502_upstream_error() {
    let mock = start_mock(Script::ServerError).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = read_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "UPSTREAM_ERROR");
    assert!(
        body["error"]["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{body}"
    );
}

#[tokio::test]
async fn upstream_timeout_maps_to_504_upstream_timeout() {
    let mock = start_mock(Script::SlowHeaders).await;
    let mut config = TestApp::config(&mock);
    config.upstream_read_timeout_secs = 1; // mock 要睡 10s
    let app = TestApp::with_config(config);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "UPSTREAM_TIMEOUT");
}

#[tokio::test]
async fn unconfigured_upstream_is_a_clear_502() {
    let app = TestApp::with_config(Config::for_tests()); // 没配上游
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "UPSTREAM_ERROR");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("UPSTREAM_")),
        "要告诉运维配哪个变量：{body}"
    );
}

#[tokio::test]
async fn malformed_body_is_validation_error() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            r#"{"session_id":"s_1"}"#,
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(mock.requests(), 0, "参数不对不该打到上游");
}

#[tokio::test]
async fn client_disconnect_cancels_upstream_and_releases_the_gate() {
    let mock = start_mock(Script::Endless).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();

    // 读到第一帧就走人（真实客户端也会这么做：用户点了"停止"或切走）
    let frame = body.frame().await.expect("应有第一帧").expect("帧不应出错");
    assert!(frame.data_ref().is_some(), "第一帧应该是 SSE 数据");
    let delivered_at_drop = mock.delivered();
    drop(body);

    // 上游必须被取消：mock 的下一帧写不出去 → aborted 计数 +1
    assert!(
        wait_until(|| mock.aborted() > 0, Duration::from_secs(3)).await,
        "客户端断开后上游没有被取消（还在烧 token）"
    );
    assert!(
        mock.delivered() < 200,
        "上游仍在继续产出：断开前 {}，断开后 {}",
        delivered_at_drop,
        mock.delivered()
    );
    assert_eq!(mock.finished(), 0, "被取消的流不应该跑到自然结束");

    // 闸门名额必须随任务结束归还：同一个 turn 立刻能再来一次
    assert!(
        wait_until(
            {
                let state = app.state.clone();
                move || state.gate.in_flight() == 0
            },
            Duration::from_secs(2),
        )
        .await,
        "取消之后闸门名额必须归还"
    );
    let again = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(again.status(), StatusCode::OK, "同一 turn 应可重新开始");
}

// ============ 抽样记账与审计（需要 PostgreSQL，默认 skip） ============

/// 抽样命中：`usage_samples` **恰好一行**，写的是装配器合并后的终值。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_sample_writes_one_row_with_merged_usage() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::CompletionsOk).await;
    let mut config = TestApp::config(&mock);
    config.usage_sample_percent = 100; // 这个用例必须命中抽样
    let app = TestApp::with_db(config, Some(db.pool.clone()));

    let token = app.register("sampled@ynau.edu.cn").await;
    let response = app.call(chat_request(Some(&token), "turn-sample-1")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = header_str(&response, "x-request-id")
        .expect("必须回写 x-request-id")
        .to_owned();
    let events = read_events(response).await;
    assert_eq!(events.last().map(|(name, _)| name.as_str()), Some("done"));

    assert!(
        wait_for_rows(&db, "SELECT count(*) FROM usage_samples", 1).await,
        "抽样命中却没有写 usage_samples"
    );
    // 一个 turn 一行：逐事件写会把成本显著高估
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM usage_samples").await,
        1,
        "一个 turn 只能有一行抽样"
    );

    let row = sqlx::query(
        "SELECT req_id, model, protocol, channel, round, tokens_in, tokens_out \
         FROM usage_samples",
    )
    .fetch_one(&db.pool)
    .await
    .expect("应能读到抽样行");
    assert_eq!(row.get::<String, _>("req_id"), request_id);
    assert_eq!(row.get::<String, _>("model"), "mock-model");
    assert_eq!(row.get::<String, _>("protocol"), "openai_completions");
    assert_eq!(row.get::<String, _>("channel"), "plan");
    assert_eq!(row.get::<i16, _>("round"), 1);
    // 合并后的终值：与 SSE 里那条 usage 一致
    assert_eq!(row.get::<i32, _>("tokens_in"), 312);
    assert_eq!(row.get::<i32, _>("tokens_out"), 96);

    // 放行也要留审计（不含正文，只有动作与目标）
    assert!(
        wait_for_rows(
            &db,
            "SELECT count(*) FROM audit_logs WHERE decision = 'allow' AND action = 'chat.stream'",
            1
        )
        .await,
        "放行应留一条 allow 审计"
    );

    db.finish().await;
}

/// 记账写库失败**不能影响流**：这是验收项之一（fire-and-forget + warn）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_usage_write_failure_does_not_break_the_stream() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    // 把 usage_samples 换成一张"写入必然违反约束"的表：列结构与迁移一致，
    // INSERT 会走到 CHECK，从而稳定地失败（不需要断开数据库）。
    db.execute("DROP TABLE usage_samples").await;
    db.execute(
        "CREATE TABLE usage_samples (\
             id bigserial PRIMARY KEY,\
             user_id uuid REFERENCES users (id) ON DELETE SET NULL,\
             req_id text NOT NULL,\
             model text NOT NULL,\
             protocol text NOT NULL,\
             channel text NOT NULL CHECK (channel IN ('byok', 'plan')),\
             round smallint NOT NULL DEFAULT 1,\
             tokens_in integer NOT NULL DEFAULT 0,\
             tokens_out integer NOT NULL DEFAULT 0,\
             created_at timestamptz NOT NULL DEFAULT now(),\
             CONSTRAINT usage_samples_never_writable CHECK (false)\
         )",
    )
    .await;

    let mock = start_mock(Script::CompletionsOk).await;
    let mut config = TestApp::config(&mock);
    config.usage_sample_percent = 100;
    let app = TestApp::with_db(config, Some(db.pool.clone()));

    let token = app.register("broken@ynau.edu.cn").await;
    let response = app
        .call(chat_request(Some(&token), "turn-broken-write"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let events = read_events(response).await;
    let names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(names.first().copied(), Some("meta"), "{names:?}");
    assert_eq!(
        names.last().copied(),
        Some("done"),
        "记账失败不能截断对话流：{names:?}"
    );
    assert_eq!(names.iter().filter(|name| **name == "done").count(), 1);
    let text: String = json_of(&events, "delta")
        .iter()
        .filter_map(|chunk| chunk["text"].as_str())
        .collect();
    assert_eq!(text, "你好，同学", "记账失败不能丢内容");

    // 写入确实失败了：表在，但一行都没有（CHECK(false) 让每次 INSERT 都失败）
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM usage_samples").await,
        0,
        "这张表本来就写不进去，正好验证失败被吞掉"
    );

    db.finish().await;
}

/// 闸门拒绝要留审计（`decision = 'deny'` + 原因），方便事后追查是谁在刷并发。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_gate_rejection_is_audited_as_deny() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::Endless).await;
    let app = TestApp::with_db(TestApp::config(&mock), Some(db.pool.clone()));
    let alice = app.register("alice@ynau.edu.cn").await;

    let held = app.call(chat_request(Some(&alice), "turn-deny-1")).await;
    assert_eq!(held.status(), StatusCode::OK);

    let rejected = app.call(chat_request(Some(&alice), "turn-deny-2")).await;
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);

    assert!(
        wait_for_rows(
            &db,
            "SELECT count(*) FROM audit_logs \
             WHERE decision = 'deny' AND reason = 'user_in_flight'",
            1
        )
        .await,
        "闸门拒绝必须留 deny 审计"
    );
    assert!(
        wait_for_rows(
            &db,
            "SELECT count(*) FROM audit_logs WHERE decision = 'allow'",
            1
        )
        .await,
        "被放行的那条流也要留 allow 审计"
    );

    drop(held);
    db.finish().await;
}
