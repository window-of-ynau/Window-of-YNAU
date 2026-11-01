// 假上游 SSE 服务（W7 P0 端到端验证用）。
//!
//! 用途：**没有真实 Key 也能验**"发消息 → 逐字出现 → 结束显示用量"，并且能进 CI；
//! infra 的"无凭据端到端闭环"（t35）可以直接复用本 example，不要另写一套 mock。
//!
//! ```powershell
//! # 正常流（OpenAI Completions 风格 SSE：逐字 + usage + [DONE]）
//! cargo run -p yunong-core --example fake_sse -- --port 8765
//! # 错误帧（HTTP 429 + 错误信封）
//! cargo run -p yunong-core --example fake_sse -- --port 8765 --mode error
//! # 非 SSE（200 text/html，模拟被代理/登录页劫持）
//! cargo run -p yunong-core --example fake_sse -- --port 8765 --mode html
//! ```
//!
//! 参数：
//! - `--port <u16>`：监听端口（默认 8765，只绑 127.0.0.1）
//! - `--mode ok|error|html`：响应形态（默认 ok）
//! - `--delay-ms <u64>`：每个 chunk 之间的间隔（默认 120ms，模拟逐字）
//! - `--text <str>`：流式输出的文本（默认"你好，我是云农之窗"）
//!
//! 每次请求都会把 **method / path / body** 打到 stdout（便于核对 Dart 侧参数），
//! 只用于本机测试，不记录任何凭据。

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 正常 SSE：逐字 delta + usage + [DONE]
    Ok,
    /// HTTP 429 + 错误信封（模拟限流）
    Error,
    /// 200 但返回 HTML（模拟被登录页/代理劫持）
    Html,
}

#[derive(Debug, Clone)]
struct Config {
    port: u16,
    mode: Mode,
    delay: Duration,
    text: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let config = parse_args();
    let listener = TcpListener::bind(("127.0.0.1", config.port)).await?;
    println!(
        "[fake_sse] listening on http://127.0.0.1:{} mode={:?}（Ctrl+C 退出）",
        config.port, config.mode
    );
    if config.mode == Mode::Ok {
        println!("[fake_sse] 上游地址：http://127.0.0.1:{}/v1", config.port);
    }
    loop {
        let (stream, _addr) = listener.accept().await?;
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(err) = handle(stream, config).await {
                eprintln!("[fake_sse] 连接处理失败：{err}");
            }
        });
    }
}

fn parse_args() -> Config {
    let mut config = Config {
        port: 8765,
        mode: Mode::Ok,
        delay: Duration::from_millis(120),
        text: "你好，我是云农之窗".to_owned(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                if let Some(value) = args.next().and_then(|v| v.parse::<u16>().ok()) {
                    config.port = value;
                }
            }
            "--mode" => {
                config.mode = match args.next().as_deref() {
                    Some("error") => Mode::Error,
                    Some("html") => Mode::Html,
                    _ => Mode::Ok,
                }
            }
            "--delay-ms" => {
                if let Some(value) = args.next().and_then(|v| v.parse::<u64>().ok()) {
                    config.delay = Duration::from_millis(value);
                }
            }
            "--text" => {
                if let Some(value) = args.next() {
                    config.text = value;
                }
            }
            "--help" | "-h" => {
                println!(
                    "用法：fake_sse [--port 8765] [--mode ok|error|html] [--delay-ms 120] [--text ...]"
                );
                std::process::exit(0);
            }
            _ => {}
        }
    }
    config
}

async fn handle(mut stream: TcpStream, config: Config) -> std::io::Result<()> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut read = 0;
    // 先读到头部结束，再按 Content-Length 读 body（够用即可，不做完整 HTTP 解析）
    loop {
        let n = stream.read(&mut buffer[read..]).await?;
        if n == 0 {
            return Ok(());
        }
        read += n;
        if let Some(header_end) = find_headers_end(&buffer[..read]) {
            let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let content_length = parse_content_length(&head);
            let body_start = header_end;
            let mut body = buffer[body_start..read].to_vec();
            while body.len() < content_length {
                let n = stream.read(&mut buffer[..]).await?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buffer[..n]);
            }
            println!(
                "[fake_sse] {} body={} bytes\n{}",
                first_line(&head),
                body.len(),
                String::from_utf8_lossy(&body)
            );
            return respond(stream, config).await;
        }
        if read >= buffer.len() {
            return Ok(());
        }
    }
}

fn find_headers_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
}

fn first_line(head: &str) -> String {
    head.lines().next().unwrap_or_default().trim().to_owned()
}

fn parse_content_length(head: &str) -> usize {
    for line in head.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            return rest.trim().parse().unwrap_or(0);
        }
    }
    0
}

async fn respond(mut stream: TcpStream, config: Config) -> std::io::Result<()> {
    match config.mode {
        Mode::Error => {
            let body = r#"{"ok":false,"error":{"code":"RATE_LIMITED","message":"fake_sse: 限流（模拟）","retry_after":2}}"#;
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            stream.flush().await
        }
        Mode::Html => {
            let body = "<html><body>统一身份认证（fake_sse 模拟劫持）</body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            stream.flush().await
        }
        Mode::Ok => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                )
                .await?;
            stream.flush().await?;

            // 逐字输出（OpenAI Completions 的 delta 形状）
            for (index, ch) in config.text.chars().enumerate() {
                let payload = serde_json::json!({
                    "id": "fake-1",
                    "object": "chat.completion.chunk",
                    "model": "fake-model",
                    "choices": [{ "index": 0, "delta": { "content": ch.to_string() }, "finish_reason": serde_json::Value::Null }]
                });
                write_sse(&mut stream, &format!("data: {payload}\n\n")).await?;
                if index == 0 {
                    println!("[fake_sse] 已发出第一个 delta（逐字流开始）");
                }
                tokio::time::sleep(config.delay).await;
            }
            // 结束帧 + usage（模拟 stream_options.include_usage）
            let finish = serde_json::json!({
                "id": "fake-1",
                "object": "chat.completion.chunk",
                "model": "fake-model",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 11, "completion_tokens": 22, "total_tokens": 33 }
            });
            write_sse(&mut stream, &format!("data: {finish}\n\n")).await?;
            write_sse(&mut stream, "data: [DONE]\n\n").await?;
            stream.flush().await?;
            println!("[fake_sse] 流结束（usage 已发）");
            Ok(())
        }
    }
}

async fn write_sse(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    stream.write_all(text.as_bytes()).await?;
    stream.flush().await
}
