//! 上游转发（套餐通道，ADR-03）：把内部 `ChatRequest` 发到厂商，再把事件流交回来。
//!
//! 分层：
//!
//! ```text
//! api::chat ──► upstream::open() ──► Upstream::next_chunks() ──► ChunkAssembler ──► SSE
//!                │ 选端点/鉴权头          │ HTTP/1.1 + SSE 解码
//!                │ 用 core::protocols::build_body 编请求体
//! ```
//!
//! **不在这里落任何正文**（ADR-02）：请求体发出即忘，响应事件转成 `Chunk` 就走。
//!
//! 上游 Key 只来自服务端环境变量；[`UpstreamProvider`] 的 `Debug` 会把它抹掉，
//! 避免误打进日志。

pub mod http;
pub mod sse;

use std::fmt;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
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

/// 超时参数（由 `Config` 传下来）。
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    pub read: Duration,
}

/// 上游故障分类。对外只有两种错误码：超时 504、其余 502（`docs/03` §1.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamErrorKind {
    /// 连接/首字节/空闲超时
    Timeout,
    /// 连不上、被重置、地址非法、没有 TLS 栈……
    Unavailable,
    /// 上游返回了非 2xx
    BadStatus(u16),
    /// 协议层不对（响应头/分块/编码不合法）
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
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for UpstreamError {}

/// 响应头的最大字节数：超过还不合法就当协议错（防止对端灌垃圾把内存吃掉）。
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// 协议对应的上游端点（`docs/02` §7.1）。
pub fn endpoint(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiCompletions => "/v1/chat/completions",
        Protocol::OpenaiResponses => "/v1/responses",
        Protocol::AnthropicMessages => "/v1/messages",
    }
}

/// 发请求并等到响应头。SSE 事件由 [`Upstream::next_chunks`] 逐个取出。
pub async fn open(
    protocol: Protocol,
    provider: &UpstreamProvider,
    request: &ChatRequest,
    timeouts: Timeouts,
) -> Result<Upstream, UpstreamError> {
    let target = http::parse_url(provider.base_url())?;
    let body = serde_json::to_vec(&yunong_core::protocols::build_body(protocol, request))
        .map_err(|error| UpstreamError::protocol(format!("请求体序列化失败：{error}")))?;

    let mut head = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Accept: text/event-stream\r\nUser-Agent: yunong-server/{}\r\nConnection: close\r\n\
         Content-Length: {}\r\n",
        http::join_path(&target.path, endpoint(protocol)),
        target.host_header(),
        crate::version(),
        body.len()
    );
    match protocol {
        Protocol::OpenaiCompletions | Protocol::OpenaiResponses => {
            head.push_str(&format!("Authorization: Bearer {}\r\n", provider.api_key()))
        }
        Protocol::AnthropicMessages => {
            head.push_str(&format!("x-api-key: {}\r\n", provider.api_key()));
            head.push_str("anthropic-version: 2023-06-01\r\n");
        }
    }
    head.push_str("\r\n");

    let address = format!("{}:{}", target.host, target.port);
    let mut socket = match timeout(timeouts.connect, TcpStream::connect(&address)).await {
        Err(_) => {
            return Err(UpstreamError::timeout(format!(
                "连接上游 {address} 超过 {}s",
                timeouts.connect.as_secs()
            )))
        }
        Ok(Err(error)) => {
            return Err(UpstreamError::unavailable(format!(
                "连接上游 {address} 失败：{error}"
            )))
        }
        Ok(Ok(socket)) => socket,
    };
    socket
        .set_nodelay(true)
        .unwrap_or_else(|error| tracing::debug!(%error, "设置 TCP_NODELAY 失败（不影响功能）"));
    let written = async {
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(&body).await?;
        socket.flush().await
    }
    .await;
    if let Err(error) = written {
        return Err(UpstreamError::unavailable(format!(
            "发送上游请求失败：{error}"
        )));
    }

    // 读响应头（首字节超时 → 504）
    let deadline = Instant::now() + timeouts.read;
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    loop {
        if let Some((head_len, parsed)) = http::parse_head(&buffer) {
            let leftover = buffer[head_len..].to_vec();
            if !parsed.is_success() {
                let snippet = String::from_utf8_lossy(&leftover);
                let snippet: String = snippet.chars().take(200).collect();
                return Err(UpstreamError::status(
                    parsed.status,
                    format!("上游返回 {}：{snippet}", parsed.status),
                ));
            }
            let body_decoder = http::BodyDecoder::new(&parsed)?;
            return Ok(Upstream {
                socket,
                decoder: body_decoder,
                sse: sse::SseDecoder::new(),
                protocol,
                read_timeout: timeouts.read,
                deadline: None,
                raw: leftover,
                pending: std::collections::VecDeque::new(),
                ended: false,
                body_bytes: Vec::new(),
            });
        }
        read_more(&mut socket, &mut buffer, deadline, timeouts.read).await?;
        // 对端不是 HTTP，或者一直发垃圾：别无限往缓冲区里塞
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(UpstreamError::protocol(format!(
                "上游响应头异常（超过 {MAX_HEAD_BYTES} 字节仍不是合法 HTTP）"
            )));
        }
    }
}

async fn read_more(
    socket: &mut TcpStream,
    buffer: &mut Vec<u8>,
    deadline: Instant,
    read_timeout: Duration,
) -> Result<(), UpstreamError> {
    let mut chunk = [0u8; 2048];
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(UpstreamError::timeout(format!(
            "上游 {}s 没有返回响应头",
            read_timeout.as_secs()
        )));
    }
    match timeout(remaining, socket.read(&mut chunk)).await {
        Err(_) => Err(UpstreamError::timeout(format!(
            "上游 {}s 没有返回响应头",
            read_timeout.as_secs()
        ))),
        Ok(Err(error)) => Err(UpstreamError::unavailable(format!("读上游失败：{error}"))),
        Ok(Ok(0)) => Err(UpstreamError::unavailable("上游在响应头之前关闭了连接")),
        Ok(Ok(read)) => {
            buffer.extend_from_slice(&chunk[..read]);
            Ok(())
        }
    }
}

/// 一条已在传输中的上游 SSE 连接。
///
/// **取消就是 drop**：连接随结构体一起关闭，上游会看到客户端断开而停止生成。
pub struct Upstream {
    socket: TcpStream,
    decoder: http::BodyDecoder,
    sse: sse::SseDecoder,
    protocol: Protocol,
    read_timeout: Duration,
    /// 空闲超时的截止时间：跨 `next_chunks` 的取消保持（心跳不能刷新它）
    deadline: Option<Instant>,
    /// 还没解码的原始字节（响应头之后的残留 + 后续读到的）
    raw: Vec<u8>,
    /// 解出来还没交出的事件
    pending: std::collections::VecDeque<Vec<Chunk>>,
    /// 已经解完的 body 字节（复用缓冲）
    body_bytes: Vec<u8>,
    ended: bool,
}

impl Upstream {
    /// 取下一条上游事件对应的 `Chunk` 序列。
    ///
    /// - `None`：上游正常结束（含 `[DONE]` 之后的 EOF）；
    /// - `Some(Err(_))`：传输层出错，调用方应换成 `Chunk::Error` 下发给客户端。
    ///
    /// 这个 future 是**取消安全**的：心跳 `timeout(...)` 打断读时，读到的字节不会丢，
    /// 空闲截止时间也不会被刷新。
    pub async fn next_chunks(&mut self) -> Option<Result<Vec<Chunk>, UpstreamError>> {
        loop {
            if let Some(chunks) = self.pending.pop_front() {
                return Some(Ok(chunks));
            }
            if !self.raw.is_empty() {
                let raw = std::mem::take(&mut self.raw);
                match self.decode(&raw) {
                    Ok(()) => continue,
                    Err(error) => {
                        self.ended = true;
                        return Some(Err(error));
                    }
                }
            }
            if self.ended {
                return None;
            }
            match self.read_raw().await {
                Ok(0) => self.ended = true, // 对端关闭：残留已经在上面解完了
                Ok(_) => {}
                Err(error) => {
                    self.ended = true;
                    return Some(Err(error));
                }
            }
        }
    }

    fn decode(&mut self, raw: &[u8]) -> Result<(), UpstreamError> {
        self.body_bytes.clear();
        let done = self.decoder.feed(raw, &mut self.body_bytes)?;
        if done {
            self.ended = true;
        }
        // body 字节 → SSE 事件 → 统一 Chunk
        let mut payloads = Vec::new();
        let body = std::mem::take(&mut self.body_bytes);
        self.sse.feed(&body, &mut payloads);
        self.body_bytes = body;
        for payload in payloads {
            // 空心跳 / 非 JSON → 空 Vec，直接丢掉（docs/03 §9 的约定）
            let chunks = yunong_core::protocols::parse_events(self.protocol, &payload);
            if !chunks.is_empty() {
                self.pending.push_back(chunks);
            }
        }
        Ok(())
    }

    async fn read_raw(&mut self) -> Result<usize, UpstreamError> {
        let deadline = match self.deadline {
            Some(deadline) => deadline,
            None => {
                let deadline = Instant::now() + self.read_timeout;
                self.deadline = Some(deadline);
                deadline
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(UpstreamError::timeout(format!(
                "上游 {}s 没有新数据",
                self.read_timeout.as_secs()
            )));
        }
        let mut chunk = [0u8; 4096];
        match timeout(remaining, self.socket.read(&mut chunk)).await {
            Err(_) => Err(UpstreamError::timeout(format!(
                "上游 {}s 没有新数据",
                self.read_timeout.as_secs()
            ))),
            Ok(Err(error)) => Err(UpstreamError::unavailable(format!("读上游失败：{error}"))),
            Ok(Ok(0)) => Ok(0),
            Ok(Ok(read)) => {
                self.raw.extend_from_slice(&chunk[..read]);
                // 只要拿到新字节就重置空闲计时
                self.deadline = None;
                Ok(read)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_match_the_contract() {
        assert_eq!(
            endpoint(Protocol::OpenaiCompletions),
            "/v1/chat/completions"
        );
        assert_eq!(endpoint(Protocol::OpenaiResponses), "/v1/responses");
        assert_eq!(endpoint(Protocol::AnthropicMessages), "/v1/messages");
    }

    #[test]
    fn api_key_never_shows_up_in_debug() {
        let provider = UpstreamProvider::new("http://127.0.0.1:9", "sk-super-secret");
        let text = format!("{provider:?}");
        assert!(!text.contains("sk-super-secret"), "Key 不能进日志：{text}");
        assert!(text.contains("<redacted>"));
    }

    #[test]
    fn single_config_serves_all_protocols() {
        let config = UpstreamConfig::single("http://127.0.0.1:9", "k");
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
