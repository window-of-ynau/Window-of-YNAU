//! SSE 帧解码（`text/event-stream`）——**切分逻辑已收敛到 `yunong_shared::sse`**（issue #18）。
//!
//! 这个文件现在只剩一层薄适配：把共享帧切分器产出的 [`SseEvent`] 转成端侧一直用的
//! [`SseFrame`]（`event` 从 `Option` 变成缺省 `"message"` 的字符串）。
//! 边界规则（`\r\n` / 跨 chunk 半帧 / 注释行 / 多行 data / 收尾帧）全部写在
//! `crates/shared/src/sse.rs`，三处调用点共用同一份用例表。

use yunong_shared::{SseEvent, SseFramer};

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

impl From<SseEvent> for SseFrame {
    fn from(event: SseEvent) -> Self {
        Self {
            event: event.event.unwrap_or_else(|| "message".to_owned()),
            data: event.data,
        }
    }
}

/// 增量式 SSE 解码器（薄封装：真正的切分在 [`SseFramer`]）。
#[derive(Debug, Default)]
pub struct SseDecoder {
    framer: SseFramer,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段字节，返回其中已经完整到达的帧。
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        let mut events = Vec::new();
        self.framer.push(bytes, &mut events);
        events.into_iter().map(SseFrame::from).collect()
    }

    /// 流结束：把"没有以空行收尾"的最后一帧也吐出来。
    ///
    /// 宽松处理：上游偶尔会在 `data:` 之后直接断 TCP（不发空行）。
    pub fn finish(&mut self) -> Vec<SseFrame> {
        let mut events = Vec::new();
        self.framer.finish(&mut events);
        events.into_iter().map(SseFrame::from).collect()
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

    /// 与另外两处调用点**共用**的用例表（`yunong_shared::sse::SSE_CASES`）：
    /// 端侧解码器的行为必须与表里每一条一致，否则收敛就白做了。
    #[test]
    fn shared_case_table_holds_here() {
        for case in yunong_shared::sse::SSE_CASES {
            let (wire, want) = (case.wire, case.frames);
            let mut decoder = SseDecoder::new();
            let mut frames = decoder.push(wire.as_bytes());
            frames.extend(decoder.finish());
            let got: Vec<(String, String)> = frames
                .into_iter()
                .map(|frame| (frame.event, frame.data))
                .collect();
            let expected: Vec<(String, String)> = want
                .iter()
                .map(|(event, data)| {
                    (
                        if event.is_empty() {
                            "message".to_owned()
                        } else {
                            (*event).to_owned()
                        },
                        (*data).to_owned(),
                    )
                })
                .collect();
            assert_eq!(got, expected, "wire={wire:?}");
        }
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
