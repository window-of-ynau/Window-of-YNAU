//! SSE 帧解码（`text/event-stream`）。
//!
//! 只做**字节 → 帧**的切分，不掺业务：三家厂商的 BYOK 直连与我们自己的套餐通道
//! 都走这一套解码器，事件怎么解释由各自的解析层负责（`protocols::parse_events` /
//! [`super::cloud_sse::parse_cloud_event`]）。
//!
//! 依据 WHATWG 的 SSE 规则实现子集：
//! - 行尾 `CRLF` / `LF` / 单独的 `CR` 都认；
//! - `:` 开头的注释行（心跳/保活）直接丢弃；
//! - `field: value`，冒号后的**一个**空格可有可无；
//! - 多行 `data:` 用 `\n` 连接（一帧一个 payload）；
//! - 空行 = 派发一帧；
//! - 未识别的字段（`id:` / `retry:`）忽略；
//! - 跨 `push` 的半行、半帧留在内部缓冲里，`push` 只吐出**完整**的帧。
//!
//! 不在这里做 UTF-8 校验：按行（以字节找换行）切出来之后才解码，
//! 所以一个多字节字符即使被切在两个 TCP 包里也不会损坏。

/// 一帧 SSE。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// 事件名；上游没写 `event:` 时是 `"message"`（OpenAI 的 `data: [DONE]` 就是这种）
    pub event: String,
    /// 该帧的 `data`（多行已用 `\n` 拼好）
    pub data: String,
}

impl SseFrame {
    /// 是否是"没写事件名"的默认消息（三家厂商的 `data:` 增量帧都是这种）。
    pub fn is_message(&self) -> bool {
        self.event == "message"
    }
}

/// 增量式 SSE 解码器。
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// 还没见到行尾的半行（按字节缓冲，避免切断 UTF-8）
    buf: Vec<u8>,
    /// 当前帧还没派发的事件名
    event: Option<String>,
    /// 当前帧累计的 data
    data: String,
    /// 是否已经收到过 `data:`（用来区分"空 data"与"根本没有 data"）
    has_data: bool,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段字节，返回其中已经完整到达的帧。
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        self.buf.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some((end, sep)) = line_end(&self.buf) {
            let line: Vec<u8> = self.buf.drain(..end + sep).take(end).collect();
            let line = String::from_utf8_lossy(&line).into_owned();
            if let Some(frame) = self.handle_line(&line) {
                frames.push(frame);
            }
        }
        frames
    }

    /// 流结束：把"没有以空行收尾"的最后一帧也吐出来。
    ///
    /// 宽松处理：上游偶尔会在 `data:` 之后直接断 TCP（不发空行）。
    pub fn finish(&mut self) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        if !self.buf.is_empty() {
            let rest: Vec<u8> = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest).into_owned();
            if let Some(frame) = self.handle_line(&line) {
                frames.push(frame);
            }
        }
        if let Some(frame) = self.dispatch() {
            frames.push(frame);
        }
        frames
    }

    fn handle_line(&mut self, line: &str) -> Option<SseFrame> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            // 注释行 / 心跳：不产生帧
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            // 没有冒号的行按"空值字段"处理（SSE 规范）
            None => (line, ""),
        };
        match field {
            "event" => {
                self.event = Some(value.to_owned());
                None
            }
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
                None
            }
            // `id:` / `retry:` / 未知字段：我们对断线续传不感兴趣，忽略
            _ => None,
        }
    }

    fn dispatch(&mut self) -> Option<SseFrame> {
        if !self.has_data && self.event.is_none() {
            return None;
        }
        let frame = SseFrame {
            event: self.event.take().unwrap_or_else(|| "message".to_owned()),
            data: std::mem::take(&mut self.data),
        };
        self.has_data = false;
        Some(frame)
    }
}

/// 找第一行结束位置：返回 `(行内字节数, 分隔符字节数)`。
///
/// 行尾只有 `\r` 且**后面还没来数据**时返回 `None`——不能把 CRLF 拆成两行，
/// 否则第二个包里的 `\n` 会被当成一个空行，凭空多派发一帧。
fn line_end(buf: &[u8]) -> Option<(usize, usize)> {
    let i = buf.iter().position(|b| *b == b'\n' || *b == b'\r')?;
    match buf[i] {
        b'\n' => Some((i, 1)),
        _ => match buf.get(i + 1) {
            Some(b'\n') => Some((i, 2)),
            Some(_) => Some((i, 1)),
            None => None,
        },
    }
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

    #[test]
    fn data_only_frame_is_a_default_message() {
        let mut d = SseDecoder::new();
        assert_eq!(d.push(b"data: hello\n\n"), vec![frame("message", "hello")]);
        assert!(d.push(b"\n\n").is_empty(), "空行不产生帧");
    }

    #[test]
    fn named_event_with_crlf() {
        let mut d = SseDecoder::new();
        let frames = d.push(b"event: delta\r\ndata: {\"text\":\"hi\"}\r\n\r\n");
        assert_eq!(frames, vec![frame("delta", "{\"text\":\"hi\"}")]);
    }

    #[test]
    fn multiline_data_is_joined_with_newline() {
        let mut d = SseDecoder::new();
        let frames = d.push(b"data: line1\ndata: line2\n\n");
        assert_eq!(frames, vec![frame("message", "line1\nline2")]);
    }

    #[test]
    fn comment_lines_are_dropped() {
        let mut d = SseDecoder::new();
        // 只有心跳（`: ping`）：不产生帧
        assert!(d.push(b": ping\n\n").is_empty());
        // 心跳后面紧跟真帧：只有真帧出来
        let frames = d.push(b": keep-alive\nevent: ping\ndata: {}\n\n");
        assert_eq!(frames, vec![frame("ping", "{}")]);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut d = SseDecoder::new();
        let frames = d.push(b"id: 42\nretry: 3000\ndata: x\n\n");
        assert_eq!(frames, vec![frame("message", "x")]);
    }

    #[test]
    fn split_across_chunks_is_reassembled() {
        let mut d = SseDecoder::new();
        let wire = b"event: delta\ndata: \xe4\xb8\x8b\xe5\x8d\x88\n\n";
        let mut frames = Vec::new();
        for byte in wire {
            frames.extend(d.push(&[*byte]));
        }
        // 逐字节喂：UTF-8 不能被切坏
        assert_eq!(frames, vec![frame("delta", "下午")]);
    }

    #[test]
    fn crlf_split_between_pushes_does_not_emit_phantom_frame() {
        let mut d = SseDecoder::new();
        assert!(d.push(b"data: x\r").is_empty(), "CR 之后要先等下一个字节");
        assert_eq!(d.push(b"\n\r\n"), vec![frame("message", "x")]);
        assert!(d.push(b"\r\n").is_empty(), "再来的空行不能产生第二帧");
    }

    #[test]
    fn finish_flushes_unterminated_frame() {
        let mut d = SseDecoder::new();
        assert!(d.push(b"data: tail").is_empty());
        assert_eq!(d.finish(), vec![frame("message", "tail")]);
        assert!(d.finish().is_empty(), "只能吐一次");
    }

    #[test]
    fn empty_data_frame_is_still_a_frame() {
        // `event: ping` + 空 data：套餐通道的心跳就是这样
        let mut d = SseDecoder::new();
        assert_eq!(d.push(b"event: ping\ndata:\n\n"), vec![frame("ping", "")]);
    }

    #[test]
    fn bare_lf_and_cr_both_work() {
        let mut d = SseDecoder::new();
        // LF 结尾：正常派发
        assert_eq!(d.push(b"data: a\n\n"), vec![frame("message", "a")]);
        // 单独的 CR 结尾：最后一个字节是 CR 时无法判断是不是 CRLF，要等下一次 push
        assert!(d.push(b"data: b\r\r").is_empty());
        assert_eq!(d.finish(), vec![frame("message", "b")]);
    }
}
