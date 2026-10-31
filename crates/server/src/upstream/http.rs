//! 手写 HTTP/1.1 客户端里**可单测的那一半**：URL 解析、响应头解析、body 解码。
//!
//! ## 为什么手写
//!
//! 离线构建环境里拿不到可用的 HTTP 客户端：`reqwest` 不在依赖缓存；`hyper` 的
//! `client` feature 需要额外的 `want` crate（同样不在缓存）；TLS 栈（rustls 的
//! crypto provider / tokio-rustls）也不可用。所以这里只做"够用且可测"的最小集：
//!
//! - 只支持 `http://`（TLS 由部署侧的反代终结，见 [`parse_url`] 的报错文案）；
//! - HTTP/1.1，`Connection: close`（一条请求一条连接，代理场景足够）；
//! - body 三种形态：`chunked`、`Content-Length`、读到对端关闭。
//!
//! 网络收发在 [`super::Upstream`] 里，本文件全是纯函数/纯状态机，方便单测。

use super::UpstreamError;

/// 上游地址（只支持明文 HTTP）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Target {
    /// `Host` 请求头（默认端口不写出来）。
    pub fn host_header(&self) -> String {
        if self.port == 80 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// 解析 `http://host[:port][/path]`。
pub fn parse_url(url: &str) -> Result<Target, UpstreamError> {
    let trimmed = url.trim();
    let Some(rest) = trimmed.strip_prefix("http://") else {
        return Err(UpstreamError::unavailable(
            if trimmed.starts_with("https://") {
                "上游只支持 http://（本构建没有 TLS 栈）：请在部署侧用反代终结 TLS，或改指内网 http 上游"
            } else {
                "上游地址必须是 http://host[:port][/path]"
            },
        ));
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(UpstreamError::unavailable("上游地址缺少主机名"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port
                .parse::<u16>()
                .map_err(|_| UpstreamError::unavailable(format!("上游地址端口非法：{port}")))?;
            (host.to_owned(), port)
        }
        _ => (authority.to_owned(), 80),
    };
    Ok(Target {
        host,
        port,
        path: path.to_owned(),
    })
}

/// 把 base_url 的路径与协议端点拼起来（`/gw` + `/v1/messages` → `/gw/v1/messages`）。
pub fn join_path(base: &str, endpoint: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        endpoint.to_owned()
    } else if endpoint.starts_with('/') {
        format!("{base}{endpoint}")
    } else {
        format!("{base}/{endpoint}")
    }
}

/// 解析完的响应头。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// 尝试从缓冲区解析响应头；字节还不够时返回 `None`。
///
/// 返回值里的 `usize` 是响应头占用的字节数，其后的字节属于 body。
pub fn parse_head(buf: &[u8]) -> Option<(usize, Head)> {
    let end = find(buf, b"\r\n\r\n")?;
    let head_len = end + 4;
    let text = std::str::from_utf8(&buf[..end]).ok()?;
    let mut lines = text.split("\r\n");
    let mut status_line = lines.next()?.split_whitespace();
    let _version = status_line.next()?; // HTTP/1.1
    let status = status_line.next()?.parse().ok()?;
    let headers = lines
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect();
    Some((head_len, Head { status, headers }))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// body 解码状态。
#[derive(Debug)]
enum Mode {
    /// `Transfer-Encoding: chunked`
    Chunked(Chunked),
    /// `Content-Length: n`
    Length(u64),
    /// 没有长度信息：读到对端关闭为止
    UntilEof,
}

/// 响应体解码器：喂原始字节，吐解码后的 body 字节。
#[derive(Debug)]
pub struct BodyDecoder {
    mode: Mode,
    done: bool,
}

impl BodyDecoder {
    pub fn new(head: &Head) -> Result<Self, UpstreamError> {
        // RFC 9112：有 Transfer-Encoding 就忽略 Content-Length
        if let Some(encoding) = head.header("transfer-encoding") {
            if encoding.to_ascii_lowercase().contains("chunked") {
                return Ok(Self {
                    mode: Mode::Chunked(Chunked::default()),
                    done: false,
                });
            }
            return Err(UpstreamError::protocol(format!(
                "不支持的 Transfer-Encoding：{encoding}"
            )));
        }
        match head.header("content-length") {
            Some(value) => {
                let length: u64 = value.trim().parse().map_err(|_| {
                    UpstreamError::protocol(format!("Content-Length 非法：{value}"))
                })?;
                Ok(Self {
                    mode: Mode::Length(length),
                    done: length == 0,
                })
            }
            None => Ok(Self {
                mode: Mode::UntilEof,
                done: false,
            }),
        }
    }

    /// 喂入原始字节，把解码出的 body 追加到 `out`。
    ///
    /// 返回 `true` 表示 body 已经完整（chunked 终止块 / `Content-Length` 收满）。
    /// `UntilEof` 永远返回 `false`，由调用方在对端关闭时收尾。
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<bool, UpstreamError> {
        if self.done {
            return Ok(true);
        }
        match &mut self.mode {
            Mode::Chunked(chunked) => {
                self.done = chunked.feed(input, out)?;
            }
            Mode::Length(remaining) => {
                let take = (*remaining).min(input.len() as u64) as usize;
                out.extend_from_slice(&input[..take]);
                *remaining -= take as u64;
                self.done = *remaining == 0;
            }
            Mode::UntilEof => out.extend_from_slice(input),
        }
        Ok(self.done)
    }
}

/// `Transfer-Encoding: chunked` 的解码状态机。
#[derive(Debug, Default)]
struct Chunked {
    pending: Vec<u8>,
    state: ChunkState,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum ChunkState {
    #[default]
    Size,
    Data(usize),
    DataCrlf,
    Trailer,
    Done,
}

impl Chunked {
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<bool, UpstreamError> {
        self.pending.extend_from_slice(input);
        loop {
            match self.state {
                ChunkState::Size => {
                    let Some(end) = find(&self.pending, b"\r\n") else {
                        return Ok(false); // 尺寸行还没收全
                    };
                    let line = &self.pending[..end];
                    let digits = line.split(|byte| *byte == b';').next().unwrap_or_default();
                    let text = std::str::from_utf8(digits)
                        .map_err(|_| UpstreamError::protocol("chunked 尺寸行不是 ASCII"))?;
                    let size = usize::from_str_radix(text.trim(), 16).map_err(|_| {
                        UpstreamError::protocol(format!("chunked 尺寸非法：{text:?}"))
                    })?;
                    self.pending.drain(..end + 2);
                    self.state = if size == 0 {
                        ChunkState::Trailer
                    } else {
                        ChunkState::Data(size)
                    };
                }
                ChunkState::Data(remaining) => {
                    if self.pending.is_empty() {
                        return Ok(false);
                    }
                    let take = remaining.min(self.pending.len());
                    out.extend_from_slice(&self.pending[..take]);
                    self.pending.drain(..take);
                    self.state = if take == remaining {
                        ChunkState::DataCrlf
                    } else {
                        ChunkState::Data(remaining - take)
                    };
                }
                ChunkState::DataCrlf => {
                    if self.pending.len() < 2 {
                        return Ok(false);
                    }
                    if &self.pending[..2] != b"\r\n" {
                        return Err(UpstreamError::protocol("chunked 分块后缺少 CRLF"));
                    }
                    self.pending.drain(..2);
                    self.state = ChunkState::Size;
                }
                ChunkState::Trailer => {
                    // 终止块之后要么直接 CRLF，要么有 trailer 头（以空行结束）
                    if self.pending.starts_with(b"\r\n") {
                        self.pending.drain(..2);
                        self.state = ChunkState::Done;
                    } else if let Some(end) = find(&self.pending, b"\r\n\r\n") {
                        self.pending.drain(..end + 4);
                        self.state = ChunkState::Done;
                    } else {
                        return Ok(false);
                    }
                }
                ChunkState::Done => return Ok(true),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_urls() {
        let target = parse_url("http://127.0.0.1:8081").expect("应能解析");
        assert_eq!(target.host, "127.0.0.1");
        assert_eq!(target.port, 8081);
        assert_eq!(target.path, "/");
        assert_eq!(target.host_header(), "127.0.0.1:8081");

        let target = parse_url("http://gw.internal/api").expect("应能解析");
        assert_eq!(target.port, 80, "默认端口");
        assert_eq!(target.path, "/api");
        assert_eq!(target.host_header(), "gw.internal");
    }

    #[test]
    fn https_is_rejected_with_actionable_message() {
        let error = parse_url("https://api.deepseek.com").expect_err("没有 TLS 栈");
        assert!(error.message().contains("http://"), "{error}");
        assert!(error.message().contains("TLS"), "{error}");
        assert!(
            error.message().contains("反代"),
            "要给出部署侧替代方案：{error}"
        );
    }

    #[test]
    fn malformed_urls_are_rejected() {
        for bad in ["", "127.0.0.1:8080", "http://", "http://host:notaport"] {
            assert!(parse_url(bad).is_err(), "不应通过：{bad:?}");
        }
    }

    #[test]
    fn joins_base_path_and_endpoint() {
        assert_eq!(join_path("/", "/v1/messages"), "/v1/messages");
        assert_eq!(join_path("", "/v1/messages"), "/v1/messages");
        assert_eq!(join_path("/gw", "/v1/messages"), "/gw/v1/messages");
        assert_eq!(join_path("/gw/", "/v1/messages"), "/gw/v1/messages");
    }

    #[test]
    fn parses_response_head_incrementally() {
        assert!(parse_head(b"HTTP/1.1 200 OK\r\n").is_none(), "头没结束");
        let raw = b"HTTP/1.1 201 Created\r\nContent-Type: text/event-stream\r\nX-Request-Id: abc\r\n\r\nbody";
        let (head_len, head) = parse_head(raw).expect("应能解析");
        assert_eq!(head.status, 201);
        assert!(head.is_success());
        assert_eq!(head.header("content-type"), Some("text/event-stream"));
        assert_eq!(head.header("X-REQUEST-ID"), Some("abc"), "头名大小写不敏感");
        assert_eq!(&raw[head_len..], b"body");
    }

    #[test]
    fn non_success_status_is_visible() {
        let head = Head {
            status: 503,
            headers: vec![],
        };
        assert!(!head.is_success());
    }

    #[test]
    fn content_length_body() {
        let head = Head {
            status: 200,
            headers: vec![("content-length".into(), "5".into())],
        };
        let mut decoder = BodyDecoder::new(&head).expect("应能构造");
        let mut out = Vec::new();
        assert!(!decoder.feed(b"hel", &mut out).expect("解码"));
        assert!(decoder.feed(b"lo", &mut out).expect("解码"));
        assert_eq!(out, b"hello");
    }

    #[test]
    fn until_eof_body() {
        let head = Head {
            status: 200,
            headers: vec![],
        };
        let mut decoder = BodyDecoder::new(&head).expect("应能构造");
        let mut out = Vec::new();
        assert!(!decoder.feed(b"abc", &mut out).expect("解码"));
        assert_eq!(out, b"abc");
    }

    #[test]
    fn chunked_body_across_split_points() {
        let head = Head {
            status: 200,
            headers: vec![("transfer-encoding".into(), "chunked".into())],
        };
        let mut decoder = BodyDecoder::new(&head).expect("应能构造");
        // "data: x\n\n" 一个事件，拆成 3 块
        let wire = b"6\r\ndata: \r\n3\r\nx\n\n\r\n0\r\n\r\n";
        let mut out = Vec::new();
        let mut done = false;
        for chunk in wire.chunks(3) {
            done = decoder.feed(chunk, &mut out).expect("解码");
        }
        assert!(done, "终止块之后应报告结束");
        assert_eq!(out, b"data: x\n\n");
    }

    #[test]
    fn chunked_with_extension_and_trailer() {
        let head = Head {
            status: 200,
            headers: vec![("TRANSFER-ENCODING".into(), "Chunked".into())],
        };
        let mut decoder = BodyDecoder::new(&head).expect("应能构造");
        let mut out = Vec::new();
        let done = decoder
            .feed(b"4;ext=1\r\nabcd\r\n0\r\nX-Trailer: 1\r\n\r\n", &mut out)
            .expect("解码");
        assert!(done);
        assert_eq!(out, b"abcd");
    }

    #[test]
    fn chunked_garbage_is_an_error_not_a_panic() {
        let head = Head {
            status: 200,
            headers: vec![("transfer-encoding".into(), "chunked".into())],
        };
        let mut decoder = BodyDecoder::new(&head).expect("应能构造");
        let mut out = Vec::new();
        let error = decoder.feed(b"zz\r\n", &mut out).expect_err("尺寸非法");
        assert!(error.message().contains("chunked"), "{error}");
    }

    #[test]
    fn unsupported_transfer_encoding_is_rejected() {
        let head = Head {
            status: 200,
            headers: vec![("transfer-encoding".into(), "gzip".into())],
        };
        assert!(BodyDecoder::new(&head).is_err());
    }
}
