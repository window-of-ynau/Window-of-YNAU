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
    /// 正常 SSE，但**完全不带 usage**（验证服务端的估算兜底与 estimated 落库）
    CompletionsNoUsage,
    /// Anthropic Messages 的正常 SSE（验证第二套协议与鉴权头）
    MessagesOk,
    /// 直接 500，响应体里塞满内部细节（验证 F3：这些绝不能出现在客户端响应里）
    ServerError,
    /// 200 但不是 SSE：模拟网关默认页/登录跳转页（验证 F4 的 content-type 校验）
    HtmlLoginPage,
    /// 正常开流，但**流中**发一个厂商错误对象（验证 F9：厂商原文不能漏给客户端）
    StreamError,
    /// 迟迟不给响应头（触发 504）
    SlowHeaders,
    /// 302 重定向到别处（验证 M2：服务端不跟随重定向）
    Redirect,
    /// 一直产出，等客户端断开（验证取消；真实厂商也是如此）
    Endless,
    /// 大口大口地灌（每帧 32KB）：客户端不读了以后，Socket 缓冲很快就会被灌满
    Flood,
}

#[derive(Debug, Default)]
struct Counters {
    requests: AtomicUsize,
    delivered: AtomicUsize,
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
        // 响应体里故意塞满内部细节：内网地址、我方环境变量名、上游原文。
        // 评审 F3 要求这些**一个都不能**出现在客户端响应里（只能进日志）。
        Script::ServerError => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "http://10.0.3.7:8089/internal/upstream?k=UPSTREAM_API_KEY secret-body-marker",
        )
            .into_response(),
        // 200 + HTML：模拟网关默认页/登录跳转页（评审 F4）
        Script::HtmlLoginPage => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<html><body>please sign in</body></html>",
        )
            .into_response(),
        Script::SlowHeaders => {
            // 比客户端的读超时更久：服务端应自己判 504，而不是一直挂着
            tokio::time::sleep(Duration::from_secs(10)).await;
            sse_response(
                Arc::clone(&ctx.counters),
                completions_frames(),
                Duration::from_millis(10),
            )
        }
        Script::CompletionsOk => sse_response(
            Arc::clone(&ctx.counters),
            completions_frames(),
            Duration::from_millis(10),
        ),
        Script::CompletionsNoUsage => sse_response(
            Arc::clone(&ctx.counters),
            completions_frames_without_usage(),
            Duration::from_millis(10),
        ),
        Script::StreamError => sse_response(
            Arc::clone(&ctx.counters),
            stream_error_frames(),
            Duration::from_millis(10),
        ),
        Script::MessagesOk => sse_response(
            Arc::clone(&ctx.counters),
            messages_frames(),
            Duration::from_millis(10),
        ),
        Script::Endless => sse_response(
            Arc::clone(&ctx.counters),
            endless_frames(),
            Duration::from_millis(10),
        ),
        // 不 sleep：靠 channel 的背压，客户端一停读就会立刻卡住
        Script::Flood => sse_response(Arc::clone(&ctx.counters), flood_frames(), Duration::ZERO),
        // 302：`Location` 指向一个**不该被访问**的本地地址（验证"不跟随"）
        Script::Redirect => (
            StatusCode::FOUND,
            [(header::LOCATION, "http://127.0.0.1:1/leak")],
            "moved",
        )
            .into_response(),
    }
}

/// 每 `delay` 发一帧；`delay = 0` 时靠背压限速。
fn sse_response(counters: Arc<Counters>, frames: Vec<String>, delay: Duration) -> Response {
    let (sender, receiver) = mpsc::channel::<Bytes>(4);
    tokio::spawn(async move {
        for frame in frames {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if sender.send(Bytes::from(frame)).await.is_err() {
                // 收端没了（本服务已断开）：真实厂商在这里也会停止生成
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

/// 正常 SSE，但**完全不带 usage**（评审 F2：验证服务端估算兜底并置 estimated=true）。
fn completions_frames_without_usage() -> Vec<String> {
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
            r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ),
        sse("[DONE]"),
    ]
}

/// 正常开流，但流中直接发一个厂商错误对象（评审 F9 的路径）：
/// 适配器会把它变成 `Chunk::Error{ message = 厂商原文 }`，服务端转发前必须洗掉。
fn stream_error_frames() -> Vec<String> {
    vec![
        sse(
            r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"先吐一点"}}]}"#,
        ),
        sse(
            r#"{"error":{"message":"vendor-secret-marker daily-quota-exceeded org-abc123 http://10.9.9.9/internal","type":"insufficient_quota","code":"insufficient_quota"}}"#,
        ),
        // 错误之后上游还会发结束标记：装配器不能再补 Done（Error 是终止事件）
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

/// 大口径的流：每帧 32KB，足够在客户端停止读取后迅速把 Socket 缓冲灌满。
fn flood_frames() -> Vec<String> {
    let padding = "x".repeat(32 * 1024);
    (0..400)
        .map(|index| {
            sse(&format!(
                r#"{{"id":"c1","object":"chat.completion.chunk","choices":[{{"index":0,"delta":{{"content":"{index}-{padding}"}}}}]}}"#
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

/// 只用于"body 有没有被读"的计数流：每 yield 一块就 +1。
struct CountingStream {
    remaining: usize,
    polls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Stream for CountingStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use std::sync::atomic::Ordering;
        if self.remaining == 0 {
            return Poll::Ready(None);
        }
        self.remaining -= 1;
        self.polls.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Some(Ok(Bytes::from(vec![b'x'; 64 * 1024]))))
    }
}

/// 往已构造好的请求上加一个头（`json_request` 只支持 token，其它头用这个补）。
fn with_header(mut request: Request<Body>, name: &str, value: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::HeaderName::from_bytes(name.as_bytes()).expect("头名"),
        header::HeaderValue::from_str(value).expect("头值"),
    );
    request
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

/// 把响应体读成文本（用在"断言没有泄露"这类检查上——那时它可能不是 JSON）。
async fn read_text(response: Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("读响应体")
        .to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
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

/// 评审 F3：上游错误的细节（内网地址 / 我方环境变量名 / 上游响应原文）只能进日志，
/// **绝不能**出现在响应体里。mock 的 500 响应体里刻意塞了三种标记。
#[tokio::test]
async fn upstream_error_does_not_leak_internal_details() {
    let mock = start_mock(Script::ServerError).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = read_text(response).await;

    assert!(!body.contains("http://"), "泄露了内网上游地址：{body}");
    // 注意：`UPSTREAM_ERROR` 这个错误码本身是合法的，要查的是**环境变量名**
    for leaked in [
        "UPSTREAM_API_KEY",
        "UPSTREAM_BASE_URL",
        "UPSTREAM_COMPLETIONS",
    ] {
        assert!(!body.contains(leaked), "泄露了环境变量名 {leaked}：{body}");
    }
    assert!(
        !body.contains("secret-body-marker"),
        "泄露了上游响应原文：{body}"
    );
    assert!(!body.contains("10.0.3.7"), "泄露了内网 IP：{body}");
    // 但错误码与固定文案要在
    assert!(body.contains("UPSTREAM_ERROR"), "{body}");
    assert!(body.contains("上游暂时不可用"), "{body}");
}

/// 评审 F9：**流中**的厂商错误原文也不能漏给客户端（这是 F3 没覆盖到的路径）。
///
/// 上游在 SSE 里直接发 `{"error":{...}}`，厂商原文（org id / 配额提示 / 内网地址）
/// 会随 `Chunk::Error.message` 一路走到客户端；服务端转发前必须换成固定文案。
#[tokio::test]
async fn upstream_stream_error_does_not_leak_vendor_message() {
    let mock = start_mock(Script::StreamError).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::OK, "流已经开了，错误在流里");
    let events = read_events(response).await;
    let body: String = events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n"))
        .collect();

    // 终止事件是 error，且只有它一个（装配器不会在错误后再补 Done）
    let names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(names.last().copied(), Some("error"), "{names:?}");
    assert_eq!(
        names.iter().filter(|name| **name == "error").count(),
        1,
        "{names:?}"
    );
    assert!(!names.contains(&"done"), "错误之后不能再补 done：{names:?}");

    // 厂商原文一个字都不能出现
    for marker in [
        "vendor-secret-marker",
        "daily-quota-exceeded",
        "org-abc123",
        "http://",
        "10.9.9.9",
    ] {
        assert!(!body.contains(marker), "泄露了 {marker}：{body}");
    }
    // 但错误码与固定文案要在，且码要是 UPSTREAM_ERROR
    let last = events
        .last()
        .map(|(_, data)| data.as_str())
        .unwrap_or_default();
    assert!(last.contains("UPSTREAM_ERROR"), "{last}");
    assert!(last.contains("上游暂时不可用"), "{last}");
}

/// 评审 F4：2xx 但不是 SSE（登录页/网关默认页）→ 502，而不是 Meta + Done 的"空回答"。
#[tokio::test]
async fn non_sse_2xx_maps_to_502_and_never_produces_done() {
    let mock = start_mock(Script::HtmlLoginPage).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "200 的 HTML 页不能被当成 SSE"
    );
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        !content_type.starts_with("text/event-stream"),
        "出错时不该还声明 SSE：{content_type}"
    );

    let body = read_text(response).await;
    assert!(!body.contains("event: done"), "不能产出 done：{body}");
    assert!(!body.contains("event: meta"), "不能产出 meta：{body}");
    assert!(body.contains("UPSTREAM_ERROR"), "{body}");
    // 上游返回的 HTML 也不能漏进响应体
    assert!(!body.contains("please sign in"), "{body}");
}

/// 评审 F4：请求体里的 `stream` 由服务端强制成 true（端侧声明的 false 不作数）。
#[tokio::test]
async fn upstream_request_forces_stream_true() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    // 客户端偏偏声明 stream=false
    let mut body: Value = serde_json::from_str(&chat_body("turn-1")).expect("请求体");
    body["stream"] = json!(false);
    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &body.to_string(),
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = read_events(response).await;

    let received = mock.last_request();
    assert_eq!(
        received.body["stream"], true,
        "转发给上游的请求体必须强制 stream=true：{}",
        received.body
    );
}

#[tokio::test]
async fn unconfigured_upstream_is_a_clear_502() {
    let app = TestApp::with_config(Config::for_tests()); // 没配上游
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "UPSTREAM_ERROR");
    // 评审 F3：配置细节（该配哪个环境变量）只进日志，不进响应体
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("UPSTREAM_"),
        "不该把环境变量名发给客户端：{message}"
    );
    assert!(message.contains("上游暂时不可用"), "{message}");
}

#[tokio::test]
async fn upstream_timeout_maps_to_504_upstream_timeout() {
    let mock = start_mock(Script::SlowHeaders).await;
    let mut config = TestApp::config(&mock);
    config.upstream_first_event_timeout_secs = 1; // mock 要睡 10s
    let app = TestApp::with_config(config);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "UPSTREAM_TIMEOUT");
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
async fn client_disconnect_stops_draining_upstream_and_releases_the_gate() {
    let mock = start_mock(Script::Flood).await;
    let app = TestApp::new(&mock);
    let token = app.register("student@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-1")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();

    // 读到第一帧就走人（真实客户端也会这么做：用户点了"停止"或切走）
    let frame = body.frame().await.expect("应有第一帧").expect("帧不应出错");
    assert!(frame.data_ref().is_some(), "第一帧应该是 SSE 数据");
    drop(body);

    // 断开后我们不再读上游：mock 往 socket 里灌，缓冲灌满后就写不动了。
    //
    // 判据说明：`reqwest`/`hyper` 在响应被 drop 之后**停止读取**，但不会主动发 FIN，
    // 所以这里量的是"上游的产出是否停住"，而不是"连接是否立刻关闭"。
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let settled = mock.delivered();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        mock.delivered(),
        settled,
        "客户端断开后上游仍在被持续消费（还在烧 token）"
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

/// 抽样命中：`usage_samples` **一行**，写的是装配器合并后的终值，且带上 `turn_id`。
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
    // 一轮 = 一行：逐事件写会把成本显著高估；4 轮循环才有 4 行（见下一条用例）
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM usage_samples").await,
        1,
        "一轮上游调用只能有一行抽样"
    );

    let row = sqlx::query(
        "SELECT turn_id, req_id, model, protocol, channel, round, tokens_in, tokens_out, estimated \
         FROM usage_samples",
    )
    .fetch_one(&db.pool)
    .await
    .expect("应能读到抽样行");
    assert_eq!(row.get::<String, _>("turn_id"), "turn-sample-1");
    assert_eq!(row.get::<String, _>("req_id"), request_id);
    assert_eq!(row.get::<String, _>("model"), "mock-model");
    assert_eq!(row.get::<String, _>("protocol"), "openai_completions");
    assert_eq!(row.get::<String, _>("channel"), "plan");
    assert_eq!(row.get::<i16, _>("round"), 1);
    // 合并后的终值：与 SSE 里那条 usage 一致
    assert_eq!(row.get::<i32, _>("tokens_in"), 312);
    assert_eq!(row.get::<i32, _>("tokens_out"), 96);
    // 上游给了 usage → 不是估算
    assert!(!row.get::<bool, _>("estimated"));

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

/// 评审 F1：`(turn_id, round)` 是幂等键——同一轮重放只**更新**那一行，不同轮各一行。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_sample_is_idempotent_per_round() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::CompletionsOk).await;
    let mut config = TestApp::config(&mock);
    config.usage_sample_percent = 100;
    let app = TestApp::with_db(config, Some(db.pool.clone()));
    let token = app.register("replay@ynau.edu.cn").await;

    // 同一 (turn_id, round=1) 连打两次：第二次算"重放"。
    //
    // **不能只等行数**：两轮用同一个幂等键，第一次落库时 count 就已经是 1；
    // 而记账是"流结束后另起的写任务"，第二次 upsert 可能还没提交 —— 只等行数会读到
    // 第一次的 req_id（CI run 41302813992 上实际 flake 过一次）。
    // 这里改成"先确认第一轮已落库，再确认第二轮把它**更新**成新 req_id"：
    // 既排除了竞态，又真的验证了 `DO UPDATE` 生效（用 `DO NOTHING` 会红）。
    let mut request_ids = Vec::new();
    for round in 0..2 {
        let response = app.call(chat_request(Some(&token), "turn-replay")).await;
        assert_eq!(response.status(), StatusCode::OK);
        request_ids.push(
            header_str(&response, "x-request-id")
                .expect("必须回写 x-request-id")
                .to_owned(),
        );
        let _ = read_events(response).await;

        if round == 0 {
            // 第一轮的写入必须真的落库，再发第二轮——这样"先后顺序"由测试控制，不靠调度
            assert!(
                wait_for_rows(&db, "SELECT count(*) FROM usage_samples", 1).await,
                "第一轮应写一行"
            );
            let stored: String = sqlx::query_scalar("SELECT req_id FROM usage_samples")
                .fetch_one(&db.pool)
                .await
                .expect("应能读到抽样行");
            assert_eq!(stored, request_ids[0], "第一轮的 req_id 应落库");
        }
    }

    // 第二轮是重放：同一行被更新为第二轮的值，行数仍然只有 1
    let wanted = &request_ids[1];
    let mut stored_req_id = String::new();
    for _ in 0..50 {
        stored_req_id = sqlx::query_scalar("SELECT req_id FROM usage_samples")
            .fetch_one(&db.pool)
            .await
            .expect("应能读到抽样行");
        if &stored_req_id == wanted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        &stored_req_id, wanted,
        "重放应把那一行更新成新 req_id（DO UPDATE 生效，DO NOTHING 会红）"
    );
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM usage_samples").await,
        1,
        "重放不能各写一行"
    );
    // 同一 turn 的第 2 轮 → 第二行（4 轮工具循环就是 4 行，成本统计才不漏）
    let mut body: Value = serde_json::from_str(&chat_body("turn-replay")).expect("请求体");
    body["round"] = json!(2);
    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &body.to_string(),
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = read_events(response).await;
    assert!(
        wait_for_rows(&db, "SELECT count(*) FROM usage_samples WHERE round = 2", 1).await,
        "第 2 轮应有自己的一行"
    );
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM usage_samples").await,
        2,
        "两轮 = 两行"
    );

    db.finish().await;
}

/// 评审 F11：唯一键必须带 `user_id`——两个用户撞同一个 `turn_id` 时不能互相改对方的行
/// （`turn_id` 是端侧生成的，同一设备模板 / 重装复用 id 都会撞）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_sample_is_isolated_per_user() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::CompletionsOk).await;
    let mut config = TestApp::config(&mock);
    config.usage_sample_percent = 100;
    let app = TestApp::with_db(config, Some(db.pool.clone()));

    let alice = app.register("alice-shared@ynau.edu.cn").await;
    let bob = app.register("bob-shared@ynau.edu.cn").await;

    for token in [&alice, &bob] {
        let response = app.call(chat_request(Some(token), "turn-shared")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = read_events(response).await;
    }

    assert!(
        wait_for_rows(
            &db,
            "SELECT count(*) FROM usage_samples WHERE turn_id = 'turn-shared'",
            2
        )
        .await,
        "两个用户各一行（唯一键含 user_id）"
    );
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(DISTINCT user_id) FROM usage_samples WHERE turn_id = 'turn-shared'"
        )
        .await,
        2,
        "user_id 不能被后一个用户的 upsert 改掉"
    );
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM usage_samples WHERE turn_id = 'turn-shared' AND user_id IS NULL"
        )
        .await,
        0,
        "两行都必须绑定到各自的用户"
    );

    db.finish().await;
}

/// 评审 F12：漏跑 `0003` 时启动自检必须**报错并说清该执行哪个文件**，
/// 而不是让抽样记账静默失败、账目悄悄变成 0。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_schema_guard_rejects_missing_migration() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    // 迁移跑全了：自检通过。
    // 用**不带 public** 的池：开发机/CI 的 public 往往也跑过迁移，带着 public 会让
    // 下面"删掉列"的断言观察不到（search_path 会先命中 public 的表）。
    let isolated = db.pool_without_public().await;
    yunong_server::accounting::verify_schema(&isolated)
        .await
        .expect("0003 已应用，自检应通过");

    // 模拟"只跑到 0002"：把 0003 加的列删掉
    db.execute("ALTER TABLE usage_samples DROP COLUMN turn_id")
        .await;
    let error = yunong_server::accounting::verify_schema(&isolated)
        .await
        .expect_err("少了 turn_id 必须报错");
    assert!(
        error.contains("0003"),
        "指引里要写清该执行哪个迁移文件：{error}"
    );
    assert!(error.contains("turn_id"), "{error}");

    db.finish().await;
}

/// 评审 F11/F12：`0003` 可重入——已经建过旧索引的库、重复执行都必须安全。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_migration_0003_is_reentrant() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migrations/0003_usage_turn_estimated.sql"
    ))
    .expect("读 0003 迁移");

    // 重入两次：第二次必须也是 ok（IF NOT EXISTS / DROP IF EXISTS / 幂等 UPDATE）
    for round in 1..=2 {
        sqlx::raw_sql(&sql)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("第 {round} 次执行 0003 失败（不可重入）：{error}"));
    }

    // 唯一索引存在且**带 user_id**
    let index_def: String = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes \
         WHERE schemaname = ANY (current_schemas(false)) \
           AND indexname = 'ux_usage_user_turn_round'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("ux_usage_user_turn_round 应存在");
    for column in ["user_id", "turn_id", "round"] {
        assert!(index_def.contains(column), "唯一键缺 {column}：{index_def}");
    }
    // 旧索引不能还留在库里（否则 ON CONFLICT 推断会有歧义）
    let old: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes \
         WHERE schemaname = ANY (current_schemas(false)) \
           AND indexname = 'ux_usage_turn_round'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("查旧索引");
    assert_eq!(old, 0, "旧索引 ux_usage_turn_round 应被 DROP");

    // 列也都还在，自检继续通过
    yunong_server::accounting::verify_schema(&db.pool)
        .await
        .expect("重入后自检仍应通过");

    db.finish().await;
}

/// 评审 F2：上游一次 usage 都没给 → 落库用估算值并置 `estimated = true`
/// （否则库里只有 0/0，分不清"真的是 0"与"没有数据"）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_sample_marks_estimated_when_upstream_omits_usage() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::CompletionsNoUsage).await;
    let mut config = TestApp::config(&mock);
    config.usage_sample_percent = 100;
    let app = TestApp::with_db(config, Some(db.pool.clone()));
    let token = app.register("estimated@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-estimated")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let events = read_events(response).await;
    assert_eq!(
        events.last().map(|(name, _)| name.as_str()),
        Some("done"),
        "没有 usage 也要正常收尾"
    );

    assert!(
        wait_for_rows(&db, "SELECT count(*) FROM usage_samples", 1).await,
        "抽样命中却没有写 usage_samples"
    );
    let row = sqlx::query("SELECT tokens_in, tokens_out, estimated FROM usage_samples")
        .fetch_one(&db.pool)
        .await
        .expect("应能读到抽样行");
    assert!(
        row.get::<bool, _>("estimated"),
        "上游没给 usage，必须标 estimated=true"
    );
    assert!(
        row.get::<i32, _>("tokens_in") > 0,
        "输入 token 应有估算值，而不是 0"
    );
    assert!(
        row.get::<i32, _>("tokens_out") > 0,
        "输出 token 应有估算值（按已下发的增量文本算），而不是 0"
    );

    db.finish().await;
}

/// W7：**鉴权失败**要留 deny 审计（reason 区分"正常过期"与"令牌无效"）。
///
/// 只记"确实带了令牌但被拒"的情况——完全没带令牌的请求不落库（见下一条用例），
/// 否则等于给攻击者一个免费的写放大入口。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_auth_failure_is_audited() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_db(TestApp::config(&mock), Some(db.pool.clone()));

    let response = app
        .call(chat_request(Some("not-a-real-token"), "turn-auth"))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let _ = read_json(response).await;

    assert!(
        wait_for_rows(
            &db,
            "SELECT count(*) FROM audit_logs \
             WHERE action = 'auth.authenticate' AND decision = 'deny' \
               AND reason = 'token_invalid'",
            1
        )
        .await,
        "令牌无效必须留 auth.authenticate/deny 审计"
    );
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM audit_logs WHERE user_id IS NULL AND decision = 'deny'"
        )
        .await,
        1,
        "鉴权失败时不知道该是谁，user_id 应为 NULL（不编造）"
    );

    db.finish().await;
}

/// W7：完全没带令牌 → 401，但**不写审计**（反写放大）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_missing_token_is_not_audited() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_db(TestApp::config(&mock), Some(db.pool.clone()));

    let response = app.call(chat_request(None, "turn-noauth")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 审计是异步 fire-and-forget：给它一点时间，再断言"仍然是 0"
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM audit_logs").await,
        0,
        "没带令牌的请求不该落审计（否则是免费的写放大入口）"
    );

    db.finish().await;
}

/// W7：**上游失败**记 decision=error（不是 deny，否则"被拒绝率"会被上游抖动污染）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_upstream_failure_is_audited_as_error() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::ServerError).await;
    let app = TestApp::with_db(TestApp::config(&mock), Some(db.pool.clone()));
    let token = app.register("upstream-audit@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-up")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let _ = read_json(response).await;

    assert!(
        wait_for_rows(
            &db,
            "SELECT count(*) FROM audit_logs \
             WHERE decision = 'error' AND reason = 'upstream_status_500'",
            1
        )
        .await,
        "上游 500 必须留 decision=error 审计，reason 只放短码"
    );
    // reason 里绝不能出现上游原文 / 内网地址
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM audit_logs \
             WHERE reason LIKE '%http%' OR reason LIKE '%secret%' OR reason LIKE '%UPSTREAM_%'"
        )
        .await,
        0,
        "审计的 reason 不能带上游原文（F3/F9 的边界）"
    );

    db.finish().await;
}

/// W7：上游在**流中**报错（厂商错误帧）同样记 decision=error。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_upstream_stream_error_is_audited() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mock = start_mock(Script::StreamError).await;
    let app = TestApp::with_db(TestApp::config(&mock), Some(db.pool.clone()));
    let token = app.register("stream-error@ynau.edu.cn").await;

    let response = app
        .call(chat_request(Some(&token), "turn-stream-err"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = read_events(response).await;

    assert!(
        wait_for_rows(
            &db,
            "SELECT count(*) FROM audit_logs \
             WHERE decision = 'error' AND reason LIKE 'upstream_error_frame_%'",
            1
        )
        .await,
        "流中的厂商错误帧也要留 error 审计"
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
             turn_id text NOT NULL,\
             user_id uuid REFERENCES users (id) ON DELETE SET NULL,\
             req_id text NOT NULL,\
             model text NOT NULL,\
             protocol text NOT NULL,\
             channel text NOT NULL CHECK (channel IN ('byok', 'plan')),\
             round smallint NOT NULL DEFAULT 1,\
             tokens_in integer NOT NULL DEFAULT 0,\
             tokens_out integer NOT NULL DEFAULT 0,\
             estimated boolean NOT NULL DEFAULT false,\
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

// ===================== 红队预演回归（H1 / H2 / M1 / M2） =====================

/// H1：登录按**邮箱**限速，超额返回 429 + `Retry-After`。
#[tokio::test]
async fn login_is_rate_limited_after_quota() {
    let mut config = Config::for_tests();
    config.auth_login_per_min = 2;
    let app = TestApp::with_config(config);
    // 注册不消耗登录额度（注册走自己的桶）
    let _ = app.register("rate@ynau.edu.cn").await;

    let body = json!({
        "email": "rate@ynau.edu.cn",
        "password": "definitely-wrong-password"
    })
    .to_string();

    let mut statuses = Vec::new();
    let mut retry_after = None;
    for _ in 0..3 {
        let response = app
            .call(json_request("POST", "/api/v1/auth/login", &body, None))
            .await;
        statuses.push(response.status());
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
        }
        let _ = read_json(response).await;
    }

    assert_eq!(statuses[0], StatusCode::UNAUTHORIZED, "第 1 次是正常失败");
    assert_eq!(statuses[1], StatusCode::UNAUTHORIZED, "第 2 次仍在额度内");
    assert_eq!(
        statuses[2],
        StatusCode::TOO_MANY_REQUESTS,
        "第 3 次必须被限流：{statuses:?}"
    );
    // R2-F7：`Retry-After` 按"回满 1 个 token 需要多久"算——窗口 60s、额度 2/分 → 30s。
    assert_eq!(
        retry_after.as_deref(),
        Some("30"),
        "429 的 Retry-After 必须按额度算（恒 1s 会让客户端立刻又撞 429）"
    );
}

/// H1：并发闸钉死哈希数。这里直接压限流器（集成层无法稳定制造"同时进哈希"）。
#[tokio::test]
async fn hash_concurrency_bounds_memory_amplifier() {
    use std::time::Duration as StdDuration;
    use yunong_server::limiter::AuthRoute;

    let limiter = yunong_server::limiter::AuthLimiter::new(1, 1000, 1000, 1000);
    let first = limiter
        .acquire_hash_slot(AuthRoute::Login, StdDuration::from_millis(1))
        .await
        .expect("第一个名额");
    assert!(
        limiter
            .acquire_hash_slot(AuthRoute::Login, StdDuration::from_millis(1))
            .await
            .is_err(),
        "名额满时必须立刻拒绝，而不是把 N 份 19MiB 排队堆起来"
    );
    drop(first);
    assert!(
        limiter
            .acquire_hash_slot(AuthRoute::Login, StdDuration::from_millis(50))
            .await
            .is_ok(),
        "归还后应重新可用"
    );
}

/// H2-a：请求体超过 body 上限 → 413（在 JSON 解析之前就被挡下）。
#[tokio::test]
async fn oversized_body_is_rejected_with_413() {
    let mock = start_mock(Script::CompletionsOk).await;
    let mut config = TestApp::config(&mock);
    config.body_limit_bytes = 256 * 1024;
    config.max_input_tokens = yunong_shared::MAX_INPUT_TOKENS;
    let app = TestApp::with_config(config);
    let token = app.register("big-body@ynau.edu.cn").await;

    let payload = json!({
        "session_id": "s",
        "turn_id": "t-big",
        "round": 1,
        "model": "m",
        "protocol": "openai_completions",
        "messages": [{ "role": "user", "content": "A".repeat(400_000) }],
        "tools": [],
        "max_output_tokens": 64,
        "stream": true,
        "estimated_input_tokens": 1
    })
    .to_string();
    assert!(payload.len() > 256 * 1024, "测试数据必须真的超限");

    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &payload,
            Some(&token),
        ))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "超过 body 上限必须是 413"
    );
    // security 复核 F3：413 必须是**我们的信封**（不是 axum 的 text/plain），
    // 否则客户端按契约解析时走不到统一错误分支。
    let body = read_json(response).await;
    assert_eq!(body["ok"], false, "信封必须是 ok:false：{body}");
    assert_eq!(body["error"]["code"], "MESSAGE_TOO_LARGE", "{body}");
    assert_eq!(mock.requests(), 0, "被 body 闸拦下的请求不该打上游");
}

/// H2-b：正文没超 body 上限，但客户端**虚报** token（填 1）→ 服务端估算后 413。
#[tokio::test]
async fn lying_token_estimate_is_rejected_by_server_side_estimate() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));
    let token = app.register("liar@ynau.edu.cn").await;

    // 非 CJK 字符 4 个 ≈ 1 token：150k 字符 ≈ 37.5k tokens > MAX_INPUT_TOKENS(32k)
    let payload = json!({
        "session_id": "s",
        "turn_id": "t-liar",
        "round": 1,
        "model": "m",
        "protocol": "openai_completions",
        "messages": [{ "role": "user", "content": "A".repeat(150_000) }],
        "tools": [],
        "max_output_tokens": 64,
        "stream": true,
        "estimated_input_tokens": 1
    })
    .to_string();

    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &payload,
            Some(&token),
        ))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "服务端必须自己估算，不能信客户端自报"
    );
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "MESSAGE_TOO_LARGE");
    assert_eq!(mock.requests(), 0, "准入拒绝不该打上游");
}

/// M1：不同用户用**同一个 turn_id** 互不影响（以前会被判 turn_in_flight）。
#[tokio::test]
async fn same_turn_id_is_independent_across_users() {
    let mock = start_mock(Script::Endless).await;
    let app = TestApp::with_config(TestApp::config(&mock));
    let alice = app.register("alice-turn@ynau.edu.cn").await;
    let bob = app.register("bob-turn@ynau.edu.cn").await;

    let alice_stream = app.call(chat_request(Some(&alice), "turn-shared")).await;
    assert_eq!(alice_stream.status(), StatusCode::OK);

    let bob_stream = app.call(chat_request(Some(&bob), "turn-shared")).await;
    assert_eq!(
        bob_stream.status(),
        StatusCode::OK,
        "别人占着同名 turn 不该挡住我（红队 M1）"
    );

    drop(alice_stream);
    drop(bob_stream);
}

/// R2-F5（回归）：受保护路由必须**先鉴权、再读 body**。
///
/// 之前把 body 上限挂在最外层，未鉴权连接也能缓冲整份 body（security 实测
/// 50 条未鉴权 chunked 连接 → RSS +55 MiB）。现在超限 body + 无令牌应当得到 **401**
/// 而不是 413 —— 401 说明鉴权先跑、body 一次都没读。
#[tokio::test]
async fn unauthenticated_oversized_body_is_rejected_before_reading() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));

    // 3 MiB：远超 1 MiB 的受保护路由上限
    let payload = "x".repeat(3 * 1024 * 1024);
    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &payload,
            None, // 没有令牌
        ))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "必须先 401：body 不该在鉴权之前被读"
    );
    assert_eq!(mock.requests(), 0);
}

/// R2-F5 的**直接**证据：body 一次都没被读。
///
/// 用计数流做 body（每被 poll 一次就 +1），未鉴权请求必须 401 且计数保持 0
/// ——比"看状态码"更硬：它证明我们确实没有缓冲任何字节。
#[tokio::test]
async fn unauthenticated_body_is_never_polled() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));

    let polls = Arc::new(AtomicUsize::new(0));
    // 8 MiB 的体，分成 64 KiB 的块；每块被取走都会 +1
    let stream = CountingStream {
        remaining: 128,
        polls: Arc::clone(&polls),
    };

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/chat/stream")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(stream))
        .expect("构造流式请求");

    let response = app.call(request).await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "没有令牌必须 401（在读完 body 之前）"
    );
    let read = polls.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        read, 0,
        "body 被读了 {read} 次：鉴权必须发生在 body 缓冲之前（R2-F5）"
    );
}

/// R2-F5 的反面：**通过鉴权之后**，body 上限仍然生效（否则就成了"只挡匿名、放过超限"）。
#[tokio::test]
async fn authenticated_oversized_body_is_still_413() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));
    let token = app.register("authed-big@ynau.edu.cn").await;

    let payload = "x".repeat(2 * 1024 * 1024); // > 1 MiB
    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &payload,
            Some(&token),
        ))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "带着有效令牌时，body 闸必须仍然生效"
    );
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "MESSAGE_TOO_LARGE");
    assert_eq!(mock.requests(), 0);
}

/// R2-F5：鉴权端点自己的 body 上限是 8 KiB（登录/注册/刷新只有几个字段）。
#[tokio::test]
async fn auth_endpoints_have_small_body_limit() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));

    let payload = json!({
        "email": "a@x.cn",
        "password": "x".repeat(32 * 1024) // 32 KiB > 8 KiB
    })
    .to_string();
    let response = app
        .call(json_request("POST", "/api/v1/auth/login", &payload, None))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "鉴权端点超过 8 KiB 应 413（而不是先做一次 19 MiB 的哈希）"
    );
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "MESSAGE_TOO_LARGE");
}

/// R2-F6：注册按**客户端 IP** 分桶（原来是全局桶：一个班同时注册会大面积 429）。
#[tokio::test]
async fn register_is_rate_limited_per_client_ip() {
    let mock = start_mock(Script::CompletionsOk).await;
    let mut config = TestApp::config(&mock);
    config.auth_register_per_min = 1; // 每个 IP 1 次/分钟
    config.trust_proxy = true; // 测试里用 XFF 模拟 IP
    let app = TestApp::with_config(config);

    let call = |ip: &'static str, email: String| {
        let app = &app;
        async move {
            let request = with_header(
                json_request(
                    "POST",
                    "/api/v1/auth/register",
                    &json!({ "email": email, "password": "password-123" }).to_string(),
                    None,
                ),
                "x-forwarded-for",
                ip,
            );
            app.call(request).await
        }
    };

    assert_eq!(
        call("203.0.113.1", "a1@ynau.edu.cn".into()).await.status(),
        StatusCode::CREATED,
        "IP1 第一次应成功"
    );
    assert_eq!(
        call("203.0.113.2", "a2@ynau.edu.cn".into()).await.status(),
        StatusCode::CREATED,
        "另一个 IP 不该被 IP1 的额度影响"
    );
    assert_eq!(
        call("203.0.113.1", "a3@ynau.edu.cn".into()).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "同一 IP 超过额度应 429"
    );
}

/// R2-F9：归一化口径统一（ASCII），非 ASCII 邮箱直接 422，避免"两个桶一个账号"。
#[tokio::test]
async fn non_ascii_email_is_rejected() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));

    let payload = json!({ "email": "Ä@x.cn", "password": "password-123" }).to_string();
    let response = app
        .call(json_request("POST", "/api/v1/auth/login", &payload, None))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "非 ASCII 邮箱应 422（限流键与账号键必须同一套归一化）"
    );
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

/// security 复核 F1（high）：超长邮箱必须在**进限流之前**被拒，且不进桶表。
///
/// 原来限流桶的键是这段攻击者可控的 email：600 个 × 256 KiB 就能让进程常驻
/// 154.8 MiB 且永不释放（与并发无关，并发闸挡不住）。修法是"先卡长度再进限流"。
#[tokio::test]
async fn oversized_email_is_rejected_without_growing_rate_limiter() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));
    assert_eq!(app.state.limiter.bucket_count(), 0, "起始应为空桶表");

    // 300 字节邮箱（>254）+ 1 MiB 量级的假邮箱各打一次登录
    for email in ["a".repeat(300), format!("{}@x.cn", "b".repeat(1024))] {
        let payload = json!({ "email": email, "password": "whatever-password" }).to_string();
        let response = app
            .call(json_request("POST", "/api/v1/auth/login", &payload, None))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "超长邮箱应 422（登录）"
        );
        let body = read_json(response).await;
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");

        let payload = json!({ "email": email, "password": "whatever-password" }).to_string();
        let response = app
            .call(json_request(
                "POST",
                "/api/v1/auth/register",
                &payload,
                None,
            ))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "超长邮箱应 422（注册）"
        );
    }

    // 关键断言：**桶表一个都没长**（长 email 连限流都没进）
    assert_eq!(
        app.state.limiter.bucket_count(),
        0,
        "超长邮箱不得进限流桶表（F1 的内存放大器正是它）"
    );
    assert_eq!(mock.requests(), 0);
}

/// security 复核 F2（medium）：工具描述/参数同样会转发给上游，必须计入准入估算。
#[tokio::test]
async fn tool_definition_is_counted_in_admission_estimate() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));
    let token = app.register("tools-estimate@ynau.edu.cn").await;

    // 700 KiB 工具描述：body 仍在上限内（1 MiB），但远超 32k token 预算
    let payload = json!({
        "session_id": "s",
        "turn_id": "turn-tools",
        "round": 1,
        "model": "m",
        "protocol": "openai_completions",
        "messages": [{ "role": "user", "content": "hi" }],
        "tools": [{
            "name": "demo",
            "description": "A".repeat(700 * 1024),
            "parameters": { "type": "object", "properties": {} }
        }],
        "max_output_tokens": 64,
        "stream": true,
        "estimated_input_tokens": 1
    })
    .to_string();
    assert!(
        payload.len() < 1024 * 1024,
        "要测的是估算漏算，不是 body 闸"
    );

    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &payload,
            Some(&token),
        ))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "工具描述必须计入准入估算（之前漏算 → 上游实收 700 KiB）"
    );
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "MESSAGE_TOO_LARGE");
    assert_eq!(mock.requests(), 0, "准入拒绝不该打上游");
}

/// M4：标识字段超长 → 422（**不截断**：截断会让审计/抽样的 turn_id 与后续轮次对不上）。
#[tokio::test]
async fn oversized_identifier_is_rejected_with_422() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock));
    let token = app.register("long-id@ynau.edu.cn").await;

    // 129 个字符，超过 128 上限
    let payload = json!({
        "session_id": "s",
        "turn_id": "t".repeat(129),
        "round": 1,
        "model": "m",
        "protocol": "openai_completions",
        "messages": [{ "role": "user", "content": "hi" }],
        "tools": [],
        "max_output_tokens": 64,
        "stream": true,
        "estimated_input_tokens": 1
    })
    .to_string();

    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &payload,
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(mock.requests(), 0, "超长标识不该打上游");
}

/// M5-a：`max_output_tokens` 超过服务端上限 → 422（客户端不得自行放大）。
#[tokio::test]
async fn max_output_tokens_over_cap_is_rejected() {
    let mock = start_mock(Script::CompletionsOk).await;
    let app = TestApp::with_config(TestApp::config(&mock)); // 默认 cap = 8192
    let token = app.register("big-output@ynau.edu.cn").await;

    let mut payload: Value = serde_json::from_str(&chat_body("turn-big-output")).expect("请求体");
    payload["max_output_tokens"] = json!(8193);
    let response = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            &payload.to_string(),
            Some(&token),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(mock.requests(), 0);
}

/// M5-b：**总时长**到点 → 发终止 `error` 事件（不是无限占名额），且不再补 `done`。
#[tokio::test]
async fn stream_total_timeout_ends_with_error_event() {
    let mock = start_mock(Script::Endless).await;
    let mut config = TestApp::config(&mock);
    config.stream_total_timeout_secs = 1; // 1 秒后主动收尾
    let app = TestApp::with_config(config);
    let token = app.register("total-timeout@ynau.edu.cn").await;

    let started = Instant::now();
    let response = app.call(chat_request(Some(&token), "turn-total")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let events = read_events(response).await;
    let elapsed = started.elapsed();

    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names.last().copied(),
        Some("error"),
        "总时长到点必须以 error 收尾：{names:?}"
    );
    assert!(!names.contains(&"done"), "错误之后不该再有 done：{names:?}");
    let last = events
        .last()
        .map(|(_, data)| data.as_str())
        .unwrap_or_default();
    assert!(last.contains("UPSTREAM_TIMEOUT"), "{last}");
    assert!(
        elapsed < Duration::from_secs(8),
        "应在约 1 秒后收尾，实际 {elapsed:?}"
    );
}

/// M2：上游 302 时服务端**不跟随**（不把请求带去它指定的地址）→ 502。
#[tokio::test]
async fn redirect_is_not_followed() {
    let mock = start_mock(Script::Redirect).await;
    let app = TestApp::with_config(TestApp::config(&mock));
    let token = app.register("redirect@ynau.edu.cn").await;

    let response = app.call(chat_request(Some(&token), "turn-redirect")).await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "3xx 不是可用的上游响应，必须 502"
    );
    let _ = read_json(response).await;
    assert_eq!(mock.requests(), 1, "只应打一次上游，不该跟随重定向再打一次");
}
