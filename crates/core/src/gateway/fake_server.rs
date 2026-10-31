//! 测试用的假 SSE 服务（`tokio::net::TcpListener` 手写，**不引 wiremock**）。
//!
//! 只接一个连接，按脚本把 SSE 帧写回去。
//!
//! **两点必须记住**：
//! 1. 流式响应必须用 `transfer-encoding: chunked`：真实 SSE 服务（axum）就是这么发的。
//!    反过来（只写响应头、不写 `content-length` 也不写 chunked），`reqwest::send()` 会
//!    一直等到连接关闭才返回，超时测试就全废了。
//! 2. 判断"客户端是不是真的停止消费上游"不能靠等 FIN：`reqwest` 在响应被 drop 之后
//!    不保证立刻发 FIN（实测 Windows 上连接会继续挂着）。可靠的做法是**继续灌数据**
//!    （[`Step::Flood`]）：客户端一旦停止读取，TCP 窗口很快就满，服务端的写就会卡住。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpListener;

/// 服务端按脚本发送的动作。
#[derive(Debug, Clone)]
pub enum Step {
    /// 写一段原始字节（通常是一到几帧 SSE）
    Write(String),
    /// 睡一会儿（模拟慢首帧 / 中途停顿）
    Sleep(Duration),
    /// 灌 `bytes` 字节的 SSE 注释帧（探测客户端还读不读）
    Flood {
        /// 目标字节数
        bytes: usize,
    },
    /// 立刻结束响应体并关闭连接（模拟"没有 Done 就断了"）
    Close,
}

/// 响应体的分帧方式。
#[derive(Debug, Clone, Copy)]
enum Framing {
    /// `transfer-encoding: chunked`（流式响应）
    Chunked,
    /// `content-length`（错误体、HTML 这类已知长度的响应）
    Length,
}

/// 抓到的一次请求（断言鉴权头/路径/请求体用）。
#[derive(Debug, Clone, Default)]
pub struct CapturedRequest {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl CapturedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn path(&self) -> String {
        self.request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("请求体应当是 JSON")
    }
}

pub struct FakeServer {
    pub addr: SocketAddr,
    request: Arc<Mutex<Option<CapturedRequest>>>,
    /// `Step::Flood` 已经写完的字节数
    flooded: Arc<AtomicUsize>,
    /// `Step::Flood` 的目标字节数
    flood_target: usize,
    /// Flood 是否已经写完（= 客户端仍在消费）
    flood_done: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

impl FakeServer {
    /// 起一个"流式响应"的假服务（`chunked` 帧，只接一个连接）。
    pub async fn start(
        status: u16,
        content_type: &'static str,
        extra_headers: Vec<(&'static str, String)>,
        steps: Vec<Step>,
    ) -> Self {
        Self::spawn(status, content_type, extra_headers, steps, Framing::Chunked).await
    }

    /// 起一个"回单个已知长度响应"的假服务（错误体、HTML 页）。
    pub async fn reply(status: u16, content_type: &'static str, body: &'static str) -> Self {
        Self::reply_with(status, content_type, Vec::new(), body).await
    }

    /// 同上，但可以带额外响应头（如 `retry-after`）。
    pub async fn reply_with(
        status: u16,
        content_type: &'static str,
        extra_headers: Vec<(&'static str, String)>,
        body: &'static str,
    ) -> Self {
        Self::spawn(
            status,
            content_type,
            extra_headers,
            vec![Step::Write(body.to_owned()), Step::Close],
            Framing::Length,
        )
        .await
    }

    async fn spawn(
        status: u16,
        content_type: &'static str,
        extra_headers: Vec<(&'static str, String)>,
        steps: Vec<Step>,
        framing: Framing,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let request = Arc::new(Mutex::new(None));
        let flooded = Arc::new(AtomicUsize::new(0));
        let flood_done = Arc::new(AtomicBool::new(false));
        let flood_target: usize = steps
            .iter()
            .map(|step| match step {
                Step::Flood { bytes } => *bytes,
                _ => 0,
            })
            .sum();

        let request_slot = Arc::clone(&request);
        let flooded_slot = Arc::clone(&flooded);
        let flood_done_slot = Arc::clone(&flood_done);
        let handle = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let Ok(captured) = read_request(&mut socket).await else {
                return;
            };
            *request_slot.lock().unwrap() = Some(captured);

            // 响应头
            let framing_header = match framing {
                Framing::Chunked => "transfer-encoding: chunked\r\n".to_owned(),
                Framing::Length => {
                    let len: usize = steps
                        .iter()
                        .map(|step| match step {
                            Step::Write(data) => data.len(),
                            _ => 0,
                        })
                        .sum();
                    format!("content-length: {len}\r\n")
                }
            };
            let mut head = format!(
                "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\nconnection: close\r\n{framing_header}",
                reason(status)
            );
            for (name, value) in &extra_headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            // 空行：头部结束
            head.push_str("\r\n");
            if socket.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let _ = socket.flush().await;
            let (_read_half, mut write_half) = socket.into_split();

            let mut closed = false;
            for step in steps {
                match step {
                    Step::Write(data) => {
                        if write_body(&mut write_half, framing, data.as_bytes())
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Step::Sleep(duration) => tokio::time::sleep(duration).await,
                    Step::Flood { bytes } => {
                        // 一帧 SSE 注释 ≈ 16KB 有效载荷
                        let chunk = format!(": {}\n\n", "x".repeat(16 * 1024));
                        let mut written = 0;
                        while written < bytes {
                            if write_body(&mut write_half, framing, chunk.as_bytes())
                                .await
                                .is_err()
                            {
                                return;
                            }
                            written += chunk.len();
                            flooded_slot.store(written, Ordering::SeqCst);
                        }
                        flood_done_slot.store(true, Ordering::SeqCst);
                    }
                    Step::Close => {
                        closed = true;
                        break;
                    }
                }
            }

            if closed && matches!(framing, Framing::Chunked) {
                // 正常结束响应体（chunked 要写终止块）
                let _ = write_half.write_all(b"0\r\n\r\n").await;
                let _ = write_half.flush().await;
            }
            // 连接一直挂着，直到 FakeServer 被 drop（测试结束）
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        Self {
            addr,
            request,
            flooded,
            flood_target,
            flood_done,
            handle,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn captured(&self) -> CapturedRequest {
        self.request
            .lock()
            .unwrap()
            .clone()
            .expect("服务端应当已经收到请求")
    }

    /// Flood 是否已经写完（写完 = 客户端仍在持续消费上游数据）。
    pub fn flood_finished(&self) -> bool {
        self.flood_done.load(Ordering::SeqCst)
    }

    /// 已经灌出去的字节数（诊断用）。
    pub fn flooded_bytes(&self) -> usize {
        self.flooded.load(Ordering::SeqCst)
    }

    /// Flood 目标字节数（诊断用）。
    pub fn flood_target(&self) -> usize {
        self.flood_target
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn write_body(
    write_half: &mut OwnedWriteHalf,
    framing: Framing,
    data: &[u8],
) -> std::io::Result<()> {
    match framing {
        Framing::Chunked => {
            write_half
                .write_all(format!("{:X}\r\n", data.len()).as_bytes())
                .await?;
            write_half.write_all(data).await?;
            write_half.write_all(b"\r\n").await?;
        }
        Framing::Length => write_half.write_all(data).await?,
    }
    write_half.flush().await
}

/// 读一个 HTTP 请求（头 + 按 `content-length` 读体）。
async fn read_request(socket: &mut tokio::net::TcpStream) -> std::io::Result<CapturedRequest> {
    let mut raw = Vec::new();
    let mut tmp = [0_u8; 1024];
    loop {
        let read = socket.read(&mut tmp).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "客户端在请求头之前断开",
            ));
        }
        raw.extend_from_slice(&tmp[..read]);
        if raw.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let body_start = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..body_start]).into_owned();
    let mut captured = CapturedRequest {
        request_line: head.lines().next().unwrap_or_default().trim().to_owned(),
        ..Default::default()
    };
    for line in head.lines().skip(1) {
        if let Some((key, value)) = line.split_once(':') {
            captured
                .headers
                .push((key.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let need = captured
        .header("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body: Vec<u8> = raw[body_start..].to_vec();
    while body.len() < need {
        let read = socket.read(&mut tmp).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..read]);
    }
    captured.body = String::from_utf8_lossy(&body).into_owned();
    Ok(captured)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        402 => "Payment Required",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}
