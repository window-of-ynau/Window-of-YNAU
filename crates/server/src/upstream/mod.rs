//! 上游转发（套餐通道，ADR-03）：把内部 `ChatRequest` 发到厂商，再把事件流交回来。
//!
//! ```text
//! api::chat ──► UpstreamClient::open() ──► UpstreamStream::next_chunks() ──► ChunkAssembler ──► SSE
//!                │ 选端点 / 鉴权头           │ reqwest（rustls）+ SSE 帧切分
//!                │ core::protocols::build_body 编请求体
//! ```
//!
//! 传输层是 `reqwest`（rustls + webpki 根证书），https 上游开箱可用；
//! 本模块只负责「协议 → 请求」的映射、SSE 帧切分与**分段超时**。
//!
//! # 超时分段（刻意不设全局 `Client::timeout()`）
//!
//! 全局超时会把**长回答整体截断**（一次推理就能跑几分钟），所以只分段：
//!
//! | 阶段 | 默认 | 为什么是这个值 |
//! |---|---|---|
//! | 建连 | 10s | DNS + TCP + TLS 握手 |
//! | 首个事件 | 90s | 推理模型常要 30~60s 才吐第一个 token |
//! | 事件之间空转 | 30s | 上游卡死时尽快失败，而不是挂一小时 |
//! | 总时长 | **不设** | 长回答本来就慢，由客户端决定何时取消 |
//!
//! **不在这里落任何正文**（ADR-02）：请求体发出即忘，响应事件转成 `Chunk` 就走。
//! 上游 Key 只来自服务端环境变量；[`UpstreamProvider`] 的 `Debug` 会把它抹掉。

pub mod sse;

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use tokio::time::timeout;
use yunong_shared::{ChatRequest, Chunk, ErrorCode, Protocol};

/// 一个上游供应商（套餐通道用；客户端永远看不到这里的 Key）。
#[derive(Clone)]
pub struct UpstreamProvider {
    base_url: String,
    api_key: String,
}

impl UpstreamProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn api_key(&self) -> &str {
        &self.api_key
    }
}

impl fmt::Debug for UpstreamProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Key 一律不打印：日志、panic、tracing 都可能带上 Debug
        f.debug_struct("UpstreamProvider")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// 三家协议各自的上游（可指向不同厂商，也可用 [`UpstreamConfig::single`] 共用一个）。
#[derive(Debug, Clone, Default)]
pub struct UpstreamConfig {
    completions: Option<UpstreamProvider>,
    responses: Option<UpstreamProvider>,
    messages: Option<UpstreamProvider>,
}

impl UpstreamConfig {
    /// 三个协议都指同一个上游（本地联调 / 单供应商部署 / 集成测试）。
    pub fn single(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let provider = UpstreamProvider::new(base_url, api_key);
        Self {
            completions: Some(provider.clone()),
            responses: Some(provider.clone()),
            messages: Some(provider),
        }
    }

    /// 从环境变量读：`UPSTREAM_<协议>_URL` / `UPSTREAM_<协议>_KEY`，
    /// 缺省时回退到 `UPSTREAM_BASE_URL` / `UPSTREAM_API_KEY`。
    pub fn from_env() -> Self {
        let fallback_url = var("UPSTREAM_BASE_URL");
        let fallback_key = var("UPSTREAM_API_KEY");
        let build = |protocol: &str| -> Option<UpstreamProvider> {
            let url = var(&format!("UPSTREAM_{protocol}_URL")).or_else(|| fallback_url.clone())?;
            let key = var(&format!("UPSTREAM_{protocol}_KEY")).or_else(|| fallback_key.clone());
            Some(UpstreamProvider::new(url, key.unwrap_or_default()))
        };
        Self {
            completions: build("COMPLETIONS"),
            responses: build("RESPONSES"),
            messages: build("MESSAGES"),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.completions.is_none() && self.responses.is_none() && self.messages.is_none()
    }

    pub fn provider(&self, protocol: Protocol) -> Option<&UpstreamProvider> {
        match protocol {
            Protocol::OpenaiCompletions => self.completions.as_ref(),
            Protocol::OpenaiResponses => self.responses.as_ref(),
            Protocol::AnthropicMessages => self.messages.as_ref(),
        }
    }
}

fn var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 分段超时（由 `Config` 传下来）。
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// 建连（DNS + TCP + TLS）
    pub connect: Duration,
    /// 从发请求到**第一个事件**的预算（含响应头等待）
    pub first_event: Duration,
    /// 第一个事件之后，相邻事件之间允许的最长空转
    pub idle: Duration,
}

/// 上游故障分类。对外只有两种错误码：超时 504、其余 502（`docs/03` §1.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamErrorKind {
    /// 建连 / 首事件 / 空闲超时
    Timeout,
    /// 连不上、被重置、TLS 失败、没有配置……
    Unavailable,
    /// 上游返回了非 2xx
    BadStatus(u16),
    /// 响应不是可用的流（协议层）
    Protocol,
}

/// 上游错误：给客户端一个笼统的错误码，给日志一份具体原因。
#[derive(Debug, Clone)]
pub struct UpstreamError {
    kind: UpstreamErrorKind,
    message: String,
}

impl UpstreamError {
    pub fn timeout(message: impl Into<String>) -> Self {
        Self {
            kind: UpstreamErrorKind::Timeout,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: UpstreamErrorKind::Unavailable,
            message: message.into(),
        }
    }

    pub fn status(status: u16, detail: impl Into<String>) -> Self {
        Self {
            kind: UpstreamErrorKind::BadStatus(status),
            message: detail.into(),
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: UpstreamErrorKind::Protocol,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> &UpstreamErrorKind {
        &self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// 对外错误码：超时单独可识别（客户端可重试一次），其余统一 `UPSTREAM_ERROR`。
    pub fn code(&self) -> ErrorCode {
        match self.kind {
            UpstreamErrorKind::Timeout => ErrorCode::UpstreamTimeout,
            _ => ErrorCode::UpstreamError,
        }
    }

    /// `reqwest` 的错误分类：超时归超时，其余（连接 / 重置 / TLS / 协议）都算不可用。
    fn from_reqwest(context: &str, error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::timeout(format!("{context}：超时（{error}）"))
        } else {
            Self::unavailable(format!("{context}：{error}"))
        }
    }
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for UpstreamError {}

/// 2xx 的响应是不是 SSE（评审 F4）。
///
/// 只认 `text/event-stream`（允许带 `; charset=utf-8`）；`Content-Type` 缺失也算不合法——
/// 宁可 502，也不要把登录页/JSON 当成"空回答"发给客户端。
fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .trim()
                .to_ascii_lowercase()
                .starts_with("text/event-stream")
        })
        .unwrap_or(false)
}

/// 协议对应的上游端点（`docs/02` §7.1）。
pub fn endpoint(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiCompletions => "/v1/chat/completions",
        Protocol::OpenaiResponses => "/v1/responses",
        Protocol::AnthropicMessages => "/v1/messages",
    }
}

/// 上游客户端：持有一个复用的 `reqwest::Client`（连接池 + TLS 会话复用）。
#[derive(Debug, Clone)]
pub struct UpstreamClient {
    providers: UpstreamConfig,
    client: reqwest::Client,
    timeouts: Timeouts,
}

impl UpstreamClient {
    pub fn new(providers: UpstreamConfig, timeouts: Timeouts) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            // 刻意**不设** `.timeout()`：那是"整条请求"的上限，会把长回答截断。
            // 首事件 / 空闲超时由 `UpstreamStream::next_chunks` 自己管（分段见模块文档）。
            .build()
            .unwrap_or_else(|error| panic!("构建上游 HTTP 客户端失败：{error}"));
        Self {
            providers,
            client,
            timeouts,
        }
    }

    pub fn provider(&self, protocol: Protocol) -> Option<&UpstreamProvider> {
        self.providers.provider(protocol)
    }

    pub fn timeouts(&self) -> Timeouts {
        self.timeouts
    }

    /// 发请求并等到响应头。之后用 [`UpstreamStream::next_chunks`] 逐条取事件。
    pub async fn open(
        &self,
        protocol: Protocol,
        request: &ChatRequest,
    ) -> Result<UpstreamStream, UpstreamError> {
        let Some(provider) = self.providers.provider(protocol) else {
            return Err(UpstreamError::unavailable(format!(
                "未配置 {protocol:?} 的上游（UPSTREAM_BASE_URL / UPSTREAM_API_KEY，\
                 或 UPSTREAM_*_URL / UPSTREAM_*_KEY）"
            )));
        };

        let url = format!(
            "{}{}",
            provider.base_url().trim_end_matches('/'),
            endpoint(protocol)
        );
        // 强制 `stream=true`（评审 F4）：这个端点是纯流式代理，端侧若声明 stream=false
        // 上游会回一整个 JSON，我们收不到 SSE 事件，只会变成"空回答"。
        // `build_body` 以入参为准，所以这里传一份改过的副本，而不是依赖端侧声明。
        let mut streaming = request.clone();
        streaming.stream = true;
        let body = yunong_core::protocols::build_body(protocol, &streaming);
        // 自己序列化而不是 reqwest 的 `.json()`：那条路径要开 `json` feature，
        // 这里只需要把 `serde_json::Value` 写进 body，没必要多开一个 feature。
        let body = serde_json::to_vec(&body)
            .map_err(|error| UpstreamError::protocol(format!("请求体序列化失败：{error}")))?;
        let mut builder = self
            .client
            .post(&url)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::USER_AGENT,
                format!("yunong-server/{}", crate::version()),
            )
            .body(body);
        builder = match protocol {
            Protocol::OpenaiCompletions | Protocol::OpenaiResponses => {
                builder.bearer_auth(provider.api_key())
            }
            Protocol::AnthropicMessages => builder
                .header("x-api-key", provider.api_key())
                .header("anthropic-version", "2023-06-01"),
        };

        // 首事件预算从这里开始算：响应头 + 第一段数据都算在 90s 里
        let first_deadline = Instant::now() + self.timeouts.first_event;
        let response = match timeout(self.timeouts.first_event, builder.send()).await {
            Err(_) => {
                return Err(UpstreamError::timeout(format!(
                    "上游 {:?} 内没有返回响应（建连或首字节超时）",
                    self.timeouts.first_event
                )))
            }
            Ok(Err(error)) => return Err(UpstreamError::from_reqwest("请求上游失败", error)),
            Ok(Ok(response)) => response,
        };

        let status = response.status();
        if !status.is_success() {
            let code = status.as_u16();
            // 错误体只截 200 字符：够定位，又不会把正文 / 大页面写进日志。
            // **注意**：这段 message 只进日志，对外由 `api::chat` 换成固定文案（评审 F3）。
            let bytes = response.bytes().await.unwrap_or_default();
            let snippet: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
            return Err(UpstreamError::status(
                code,
                format!("上游返回 {code}：{snippet}"),
            ));
        }

        // 2xx 不等于 SSE（评审 F4）：登录跳转页、网关默认页、非流式 JSON 都是 200。
        // 不校验的话这些会被当成"上游正常关闭"，产出 Meta + Done{Stop} 的空回答，
        // 与 docs/03「响应不合法 → 502」冲突。端侧网关早就验了，服务端也要验。
        if !is_event_stream(&response) {
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<缺失>")
                .to_owned();
            return Err(UpstreamError::protocol(format!(
                "上游返回的不是 SSE（content-type: {content_type}）"
            )));
        }

        Ok(UpstreamStream {
            response,
            sse: sse::SseDecoder::new(),
            protocol,
            timeouts: self.timeouts,
            first_deadline: Some(first_deadline),
            pending: VecDeque::new(),
            ended: false,
        })
    }
}

/// 一条已在传输中的上游 SSE 响应。
///
/// **取消就是 drop**：`reqwest::Response` 随结构体一起释放，我们立刻停止读取上游；
/// socket 缓冲灌满后，厂商的写入会被 TCP 背压卡住（不再有 token 被消费）。
///
/// 注意：`reqwest` / `hyper` 在响应被 drop 后**不会主动发 FIN**，连接要等上游自己超时
/// 或写完才回收——所以"取消"的观测量是"没人继续读"，而不是"连接立刻关闭"。
#[derive(Debug)]
pub struct UpstreamStream {
    response: reqwest::Response,
    sse: sse::SseDecoder,
    protocol: Protocol,
    timeouts: Timeouts,
    /// 首事件的绝对截止时间；第一个事件出现后置空，改用 `timeouts.idle`
    first_deadline: Option<Instant>,
    /// 解出来还没交出的事件
    pending: VecDeque<Vec<Chunk>>,
    ended: bool,
}

impl UpstreamStream {
    /// 取下一条上游事件对应的 `Chunk` 序列。
    ///
    /// - `None`：上游正常结束（含 `[DONE]` 之后的 EOF）；
    /// - `Some(Err(_))`：传输层出错，调用方应换成 `Chunk::Error` 下发给客户端。
    ///
    /// 这个 future 是**取消安全**的：心跳 `select!` 打断时，已解出的事件还在
    /// `pending` 里，`reqwest` 的 `chunk()` 也不会吞掉半帧数据。
    pub async fn next_chunks(&mut self) -> Option<Result<Vec<Chunk>, UpstreamError>> {
        loop {
            if let Some(chunks) = self.pending.pop_front() {
                return Some(Ok(chunks));
            }
            if self.ended {
                return None;
            }

            // 首事件用 90s 预算（绝对截止时间），之后用"事件间空转"预算
            let budget = match self.first_deadline {
                Some(deadline) => deadline.saturating_duration_since(Instant::now()),
                None => self.timeouts.idle,
            };
            if budget.is_zero() {
                self.ended = true;
                return Some(Err(UpstreamError::timeout(format!(
                    "上游 {:?} 内没有吐出第一个事件",
                    self.timeouts.first_event
                ))));
            }

            match timeout(budget, self.response.chunk()).await {
                Err(_) => {
                    self.ended = true;
                    let message = if self.first_deadline.is_some() {
                        format!("上游 {:?} 内没有吐出第一个事件", self.timeouts.first_event)
                    } else {
                        format!("上游 {:?} 没有新事件", self.timeouts.idle)
                    };
                    return Some(Err(UpstreamError::timeout(message)));
                }
                Ok(Err(error)) => {
                    self.ended = true;
                    return Some(Err(UpstreamError::from_reqwest("读上游失败", error)));
                }
                Ok(Ok(None)) => {
                    self.ended = true;
                    return None;
                }
                Ok(Ok(Some(bytes))) => {
                    self.decode(&bytes);
                    if !self.pending.is_empty() {
                        // 首个事件到了：后面按"事件间空转"判超时
                        self.first_deadline = None;
                    }
                }
            }
        }
    }

    /// body 字节 → SSE 事件 → 统一 `Chunk`。
    fn decode(&mut self, bytes: &[u8]) {
        let mut payloads = Vec::new();
        self.sse.feed(bytes, &mut payloads);
        for payload in payloads {
            // 空心跳 / 非 JSON → 空 Vec，直接丢掉（`docs/03` §9 的约定）
            let chunks = yunong_core::protocols::parse_events(self.protocol, &payload);
            if !chunks.is_empty() {
                self.pending.push_back(chunks);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use yunong_shared::ChatMessage;

    use super::*;

    const SSE_HEAD: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";

    fn test_timeouts(first_event: Duration) -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(2),
            first_event,
            idle: Duration::from_secs(2),
        }
    }

    fn client_for(addr: SocketAddr, timeouts: Timeouts) -> UpstreamClient {
        UpstreamClient::new(
            UpstreamConfig::single(format!("http://{addr}"), "test-key"),
            timeouts,
        )
    }

    fn request() -> ChatRequest {
        ChatRequest {
            session_id: "s".into(),
            turn_id: "t".into(),
            round: 1,
            model: "mock-model".into(),
            protocol: Protocol::OpenaiCompletions,
            messages: vec![ChatMessage::user("你好")],
            tools: vec![],
            max_output_tokens: 128,
            stream: true,
            estimated_input_tokens: 12,
        }
    }

    /// 一个 HTTP chunked 帧。
    fn chunk(data: &str) -> String {
        format!("{:x}\r\n{data}\r\n", data.len())
    }

    /// 起一个"只会说 HTTP"的原始上游：写回 `response` 后按 `hold` 决定挂多久。
    ///
    /// `hold = ZERO` → 写完就关（正常收尾）；`hold > 0` → 保持连接但不再说话（模拟卡住）。
    async fn raw_upstream(response: String, hold: Duration) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 mock 上游");
        let addr = listener.local_addr().expect("取 mock 地址");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            read_request(&mut socket).await;
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
            if !hold.is_zero() {
                tokio::time::sleep(hold).await;
            }
            // 连接随 task 结束关闭：客户端读到 EOF 即收尾
        });
        addr
    }

    /// 把请求读完（头 + Content-Length 声明的 body），避免上游提前关闭连接。
    async fn read_request(socket: &mut TcpStream) {
        let mut buffer = Vec::new();
        let mut scratch = [0u8; 1024];
        loop {
            let Ok(read) = socket.read(&mut scratch).await else {
                return;
            };
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&scratch[..read]);
            if let Some(end) = find(&buffer, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buffer.len() >= end + 4 + length {
                    return;
                }
            }
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[tokio::test]
    async fn reads_a_normal_chunked_sse_stream() {
        let body = [
            chunk(&format!(
                "data: {}\n\n",
                r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"你好"}}]}"#
            )),
            chunk(&format!(
                "data: {}\n\n",
                r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#
            )),
            chunk("data: [DONE]\n\n"),
            "0\r\n\r\n".to_owned(),
        ]
        .concat();
        let addr = raw_upstream(format!("{SSE_HEAD}{body}"), Duration::ZERO).await;

        let client = client_for(addr, test_timeouts(Duration::from_secs(5)));
        let mut stream = client
            .open(Protocol::OpenaiCompletions, &request())
            .await
            .expect("开流应成功");

        let mut chunks = Vec::new();
        while let Some(next) = stream.next_chunks().await {
            chunks.extend(next.expect("传输层不该报错"));
        }
        assert_eq!(chunks.len(), 4, "两帧事件 + usage&done 同帧 → {chunks:?}");
        assert!(matches!(&chunks[0], Chunk::Delta { text } if text == "你好"));
        assert!(matches!(
            &chunks[1],
            Chunk::Usage {
                tokens_in: 7,
                tokens_out: 3,
                ..
            }
        ));
        assert!(matches!(&chunks[2], Chunk::Done { .. }));
        assert!(
            matches!(&chunks[3], Chunk::Done { .. }),
            "[DONE] 会再给一个 Done（去重交给 ChunkAssembler）"
        );
    }

    #[tokio::test]
    async fn first_event_timeout_is_reported_as_upstream_timeout() {
        // 响应头 + 半个事件（没有空行）就卡住：这是"推理模型迟迟不吐第一个 token"的样子。
        let half_event = format!("{SSE_HEAD}{}", chunk("data: {\"id\":\"c1\"\n"));
        let addr = raw_upstream(half_event, Duration::from_secs(5)).await;
        let client = client_for(
            addr,
            Timeouts {
                connect: Duration::from_secs(2),
                first_event: Duration::from_millis(400),
                idle: Duration::from_secs(2),
            },
        );
        let mut stream = client
            .open(Protocol::OpenaiCompletions, &request())
            .await
            .expect("响应头已到，开流应成功");

        let error = stream
            .next_chunks()
            .await
            .expect("应有一步结果")
            .expect_err("首事件超时必须报错");
        assert_eq!(error.kind(), &UpstreamErrorKind::Timeout);
        assert_eq!(error.code(), ErrorCode::UpstreamTimeout, "对外应是 504");
        assert!(stream.next_chunks().await.is_none(), "报错之后不应再产出");
    }

    #[tokio::test]
    async fn dropping_the_stream_stops_reading_from_upstream() {
        // 上游一直往外灌大帧；我们 drop 掉响应之后就不该再有人消费这些数据。
        //
        // 注意判据：`reqwest`/`hyper` 在响应体被 drop 后**停止读取**，但不会主动发 FIN，
        // 所以这里量的是"上游还能不能继续写进去"——socket 缓冲被灌满就当住了。
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 mock 上游");
        let addr = listener.local_addr().expect("取 mock 地址");
        let written = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&written);
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            read_request(&mut socket).await;
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
            if socket.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let padding = "x".repeat(32 * 1024);
            for index in 0..400 {
                let frame = format!(
                    "data: {{\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{index}-{padding}\"}}}}]}}\n\n"
                );
                // 写不进去（缓冲满 / 对端关闭）就停：真实厂商同样会在这里背压
                match timeout(Duration::from_secs(2), socket.write_all(frame.as_bytes())).await {
                    Ok(Ok(())) => {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(Err(_)) | Err(_) => return,
                }
            }
        });

        let client = client_for(addr, test_timeouts(Duration::from_secs(5)));
        let mut stream = client
            .open(Protocol::OpenaiCompletions, &request())
            .await
            .expect("开流应成功");
        let first = stream
            .next_chunks()
            .await
            .expect("应有第一帧")
            .expect("第一帧不该出错");
        assert!(matches!(&first[0], Chunk::Delta { .. }), "{first:?}");

        // 客户端断开 = drop 掉响应（生产里是"SSE 收端被 drop"）
        drop(stream);

        // 缓冲灌满后，上游的写入必须停住（说明没有人再读了）
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let settled = written.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(
            written.load(Ordering::SeqCst),
            settled,
            "drop 之后还在继续读上游（数据仍在被消费）"
        );
    }

    #[tokio::test]
    async fn https_upstream_is_supported_and_fails_as_transport_error() {
        // 占一个端口再释放：指向它必然连接被拒，且不涉及 DNS / 外网
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定");
        let addr = listener.local_addr().expect("取地址");
        drop(listener);

        let client = UpstreamClient::new(
            UpstreamConfig::single(format!("https://{addr}"), "test-key"),
            test_timeouts(Duration::from_secs(1)),
        );
        let error = client
            .open(Protocol::OpenaiCompletions, &request())
            .await
            .expect_err("连不上应报错");
        // https 是受支持的：失败原因必须是传输层，而不是「scheme 不支持」
        assert!(
            matches!(
                error.kind(),
                UpstreamErrorKind::Unavailable | UpstreamErrorKind::Timeout
            ),
            "{error:?}"
        );
        assert_eq!(error.code(), ErrorCode::UpstreamError);
        assert!(
            !error.message().contains("只支持"),
            "https 不该被当成不支持的 scheme：{}",
            error.message()
        );
    }

    #[test]
    fn api_key_never_shows_up_in_debug() {
        let provider = UpstreamProvider::new("https://api.example.com", "sk-super-secret");
        let text = format!("{provider:?}");
        assert!(!text.contains("sk-super-secret"), "Key 不能进日志：{text}");
        assert!(text.contains("<redacted>"));
    }

    #[test]
    fn single_config_serves_all_protocols() {
        let config = UpstreamConfig::single("https://api.example.com", "k");
        assert!(!config.is_empty());
        for protocol in [
            Protocol::OpenaiCompletions,
            Protocol::OpenaiResponses,
            Protocol::AnthropicMessages,
        ] {
            assert!(config.provider(protocol).is_some(), "{protocol:?}");
        }
        assert!(UpstreamConfig::default()
            .provider(Protocol::OpenaiCompletions)
            .is_none());
    }

    #[test]
    fn timeout_maps_to_upstream_timeout_code() {
        assert_eq!(
            UpstreamError::timeout("x").code(),
            ErrorCode::UpstreamTimeout
        );
        for error in [
            UpstreamError::unavailable("x"),
            UpstreamError::status(500, "x"),
            UpstreamError::protocol("x"),
        ] {
            assert_eq!(error.code(), ErrorCode::UpstreamError, "{error:?}");
        }
    }
}
