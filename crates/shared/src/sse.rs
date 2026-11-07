//! SSE 帧切分：**全系统唯一实现**（issue #18）。
//!
//! 端侧 BYOK 直连、套餐通道、服务端上游代理三处都要把 `text/event-stream` 的字节流
//! 切成事件帧。以前这套逻辑抄了三份（`core/gateway/sse.rs`、`server/upstream/sse.rs`，
//! 另外 `core/gateway/cloud_sse.rs` 只做业务解析），边界处理（`\r\n` vs `\n`、跨 chunk
//! 的半帧、`data:` 后的空格、收尾帧）迟早会分叉——`base_url` 的 `/v1` 分叉（#51）已经
//! 让我们吃过一次这种苦头。
//!
//! 这里**只做字节 → 帧**，不掺业务：
//!
//! - 行尾 `LF` / `CRLF` / 单独的 `CR` 都认（CRLF 可能被拆在两个 chunk 里）；
//! - `:` 开头的注释行（心跳）丢弃；
//! - `field: value`，冒号后的**一个**空格可有可无；没有冒号的行按"空值字段"处理；
//! - 多行 `data:` 用 `\n` 连接；
//! - 空行 = 派发一帧；`event:` 有值但 `data:` 为空**也算一帧**（套餐通道的心跳就是这样）；
//! - `id:` / `retry:` / 未知字段忽略；
//! - `push` 只吐出**完整**的帧，半行/半帧留在缓冲里；流结束时用 [`SseFramer::finish`]
//!   把"没有空行收尾"的最后一帧也吐出来（上游偶尔直接断流）。
//!
//! 不做 UTF-8 校验切割：先按字节找行尾，再按行解码，所以多字节字符被拆到两个 TCP 包
//! 里也不会损坏。UTF-8 是 lossy 解码（与端侧原有行为一致）；服务端原本"非 UTF-8 行就
//! 丢弃整个事件"，用 [`SseFramer::new_strict_utf8`] 保留那份语义。

/// 一帧 SSE。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// 事件名；上游没写 `event:` 时是 `None`（语义等价于 `"message"`）
    pub event: Option<String>,
    /// 该帧的 `data`（多行已用 `\n` 拼好；可能是空串）
    pub data: String,
    /// 这一帧是否**真的出现过** `data:` 字段。
    ///
    /// 用于区分「`event: x` + 空 data」（`had_data = true`，套餐通道心跳）
    /// 与「只有 `event: x`、根本没有 data」（`had_data = false`，服务端旧逻辑会丢掉它）。
    pub had_data: bool,
}

impl SseEvent {
    /// 是否是"没写事件名"的默认消息（三家厂商的 `data:` 增量帧都是这种）。
    pub fn is_message(&self) -> bool {
        self.event.is_none()
    }

    /// 事件名，没写时按规范当作 `"message"`。
    pub fn event_name(&self) -> &str {
        self.event.as_deref().unwrap_or("message")
    }
}

/// UTF-8 非法行的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InvalidUtf8 {
    /// 按 `U+FFFD` 替换继续（端侧原有行为）
    #[default]
    Lossy,
    /// 丢弃当前事件（服务端原有行为：不把乱码当 JSON 往下传）
    DropEvent,
}

/// 增量式 SSE 帧切分器。
#[derive(Debug, Default)]
pub struct SseFramer {
    /// 还没见到行尾的半行（按字节缓冲，避免切断 UTF-8）
    buf: Vec<u8>,
    /// 当前帧还没派发的事件名
    event: Option<String>,
    /// 当前帧累计的 data
    data: String,
    /// 是否已经收到过 `data:`（区分"空 data"与"根本没有 data"）
    has_data: bool,
    /// UTF-8 非法行怎么处理
    invalid_utf8: InvalidUtf8,
}

impl SseFramer {
    /// lossy 解码（端侧默认）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 严格解码：非 UTF-8 行直接丢弃当前事件（服务端默认）。
    pub fn new_strict_utf8() -> Self {
        Self {
            invalid_utf8: InvalidUtf8::DropEvent,
            ..Self::default()
        }
    }

    /// 喂入一段字节，把其中已经完整到达的帧追加到 `out`。
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<SseEvent>) {
        self.buf.extend_from_slice(chunk);
        while let Some((end, sep)) = line_end(&self.buf) {
            let line: Vec<u8> = self.buf.drain(..end + sep).take(end).collect();
            if let Some(event) = self.handle_bytes(&line) {
                out.push(event);
            }
        }
    }

    /// 流结束：把"没有以空行收尾"的最后一帧也吐出来（宽松处理）。
    pub fn finish(&mut self, out: &mut Vec<SseEvent>) {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            if let Some(event) = self.handle_bytes(&rest) {
                out.push(event);
            }
        }
        if let Some(event) = self.dispatch() {
            out.push(event);
        }
    }

    fn handle_bytes(&mut self, line: &[u8]) -> Option<SseEvent> {
        match std::str::from_utf8(line) {
            Ok(text) => self.handle_line(text),
            Err(_) if self.invalid_utf8 == InvalidUtf8::Lossy => {
                let lossy = String::from_utf8_lossy(line).into_owned();
                self.handle_line(&lossy)
            }
            Err(_) => {
                // 严格模式：整帧作废（清空已累计的 event/data）
                self.event = None;
                self.data.clear();
                self.has_data = false;
                None
            }
        }
    }

    fn handle_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            // 空行 = 派发一帧
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

    fn dispatch(&mut self) -> Option<SseEvent> {
        if !self.has_data && self.event.is_none() {
            return None;
        }
        let event = SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data),
            had_data: self.has_data,
        };
        self.has_data = false;
        Some(event)
    }
}

/// 找第一行结束位置：返回 `(行内字节数, 分隔符字节数)`。
///
/// 行尾只有 `\r` 且**后面还没来数据**时返回 `None`——不能把 CRLF 拆成两行，
/// 否则第二个包里的 `\n` 会被当成一个空行，凭空多派发一帧。
fn line_end(buf: &[u8]) -> Option<(usize, usize)> {
    let index = buf
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')?;
    match buf[index] {
        b'\n' => Some((index, 1)),
        _ => match buf.get(index + 1) {
            Some(b'\n') => Some((index, 2)),
            Some(_) => Some((index, 1)),
            None => None,
        },
    }
}

/// 一条共享用例：`wire` 是原始字节（UTF-8 文本表示），`frames` 是期望派发的帧
/// （事件名空串 = 没写 `event:`），`needs_finish` 表示这帧**没有空行收尾**、
/// 要靠 [`SseFramer::finish`] 才吐。
#[derive(Debug, Clone, Copy)]
pub struct SseCase {
    pub wire: &'static str,
    pub frames: &'static [(&'static str, &'static str)],
    pub needs_finish: bool,
}

/// 三处调用点**共用**的用例表：`\r\n`/`\n` 混用、半帧跨 chunk、空 data、多行 data、
/// 注释行、`[DONE]` 后多余字节、未收尾帧。
///
/// 任何一处换了切分实现（或不按共享语义处理），这些用例就会红——收敛的目的正在于此。
#[doc(hidden)]
pub const SSE_CASES: &[SseCase] = &[
    SseCase {
        wire: "data: hello\n\n",
        frames: &[("", "hello")],
        needs_finish: false,
    },
    SseCase {
        wire: "event: delta\r\ndata: {\"a\":1}\r\n\r\n",
        frames: &[("delta", "{\"a\":1}")],
        needs_finish: false,
    },
    // 空 data 也要派发（套餐通道心跳）
    SseCase {
        wire: "event: ping\ndata:\n\n",
        frames: &[("ping", "")],
        needs_finish: false,
    },
    // 多行 data 用 \n 拼接
    SseCase {
        wire: "data: line1\ndata: line2\n\n",
        frames: &[("", "line1\nline2")],
        needs_finish: false,
    },
    // `data:` 后面没有空格
    SseCase {
        wire: "data:\"x\"\n\n",
        frames: &[("", "\"x\"")],
        needs_finish: false,
    },
    // 注释行/心跳不产生帧；未知字段忽略
    SseCase {
        wire: ": ping\nid: 42\nretry: 3000\nevent: delta\ndata: x\n\n",
        frames: &[("delta", "x")],
        needs_finish: false,
    },
    // 只有注释 → 没有任何帧
    SseCase {
        wire: ": keep-alive\n\n",
        frames: &[],
        needs_finish: false,
    },
    // 两帧连发
    SseCase {
        wire: "data: a\n\ndata: b\n\n",
        frames: &[("", "a"), ("", "b")],
        needs_finish: false,
    },
    // CRLF 空行
    SseCase {
        wire: "data: x\r\n\r\n",
        frames: &[("", "x")],
        needs_finish: false,
    },
    // `[DONE]` 之后的多余空行不产生空帧
    SseCase {
        wire: "data: [DONE]\n\n\n\n",
        frames: &[("", "[DONE]")],
        needs_finish: false,
    },
    // 未收尾：`push` 不吐，`finish` 才吐（服务端历史上没有 finish，见其单测）
    SseCase {
        wire: "data: tail",
        frames: &[("", "tail")],
        needs_finish: true,
    },
];
#[cfg(test)]
mod tests {
    use super::*;

    fn render(events: &[SseEvent]) -> Vec<(String, String)> {
        events
            .iter()
            .map(|event| (event.event.clone().unwrap_or_default(), event.data.clone()))
            .collect()
    }

    fn expected(case: &[(&str, &str)]) -> Vec<(String, String)> {
        case.iter()
            .map(|(event, data)| ((*event).to_owned(), (*data).to_owned()))
            .collect()
    }

    /// 整段一次性喂入，用例表逐条成立。
    #[test]
    fn shared_cases_hold_in_one_push() {
        for case in SSE_CASES {
            let mut framer = SseFramer::new();
            let mut events = Vec::new();
            framer.push(case.wire.as_bytes(), &mut events);
            if case.needs_finish {
                assert!(events.is_empty(), "未收尾的帧 push 不该吐：{case:?}");
            }
            framer.finish(&mut events);
            assert_eq!(render(&events), expected(case.frames), "case={case:?}");
        }
    }

    /// 同样的用例，**逐字节**喂入（模拟最恶劣的网络分片）也必须一样。
    #[test]
    fn shared_cases_hold_byte_by_byte() {
        for case in SSE_CASES {
            let mut framer = SseFramer::new();
            let mut events = Vec::new();
            for byte in case.wire.as_bytes() {
                framer.push(&[*byte], &mut events);
            }
            framer.finish(&mut events);
            assert_eq!(
                render(&events),
                expected(case.frames),
                "逐字节喂入时必须一致：case={case:?}"
            );
        }
    }

    /// CRLF 被拆在两个 chunk 之间时，不能凭空多派发一帧。
    #[test]
    fn split_crlf_does_not_emit_phantom_frame() {
        let mut framer = SseFramer::new();
        let mut events = Vec::new();
        framer.push(b"data: x\r", &mut events);
        assert!(events.is_empty(), "CR 之后要先等下一个字节");
        framer.push(b"\n\r\n", &mut events);
        assert_eq!(render(&events), vec![("".into(), "x".into())]);
        let before = events.len();
        framer.push(b"\r\n", &mut events);
        assert_eq!(events.len(), before, "再来的空行不能产生第二帧");
    }

    /// 多字节字符被切在两个包里也不能损坏。
    #[test]
    fn utf8_split_across_chunks_survives() {
        let mut framer = SseFramer::new();
        let mut events = Vec::new();
        for byte in "event: delta\ndata: 下午\n\n".as_bytes() {
            framer.push(&[*byte], &mut events);
        }
        assert_eq!(render(&events), vec![("delta".into(), "下午".into())]);
    }

    /// lossy：非 UTF-8 字节被替换成 U+FFFD，事件照常派发。
    #[test]
    fn lossy_mode_keeps_the_event() {
        let mut framer = SseFramer::new();
        let mut events = Vec::new();
        framer.push(b"data: \xff\xfe\n\n", &mut events);
        assert_eq!(events.len(), 1);
        assert!(events[0].data.contains('\u{FFFD}'), "{:?}", events[0]);
    }

    /// 严格模式（服务端原有语义）：非 UTF-8 行 → 丢掉当前事件，后续正常帧不受影响。
    #[test]
    fn strict_mode_drops_the_bad_event() {
        let mut framer = SseFramer::new_strict_utf8();
        let mut events = Vec::new();
        framer.push(b"data: \xff\xfe\n\ndata: ok\n\n", &mut events);
        assert_eq!(render(&events), vec![("".into(), "ok".into())]);
    }

    /// `finish` 只能吐一次，不会重复派发。
    #[test]
    fn finish_is_idempotent() {
        let mut framer = SseFramer::new();
        let mut events = Vec::new();
        framer.push(b"data: tail", &mut events);
        assert!(events.is_empty());
        framer.finish(&mut events);
        assert_eq!(render(&events), vec![("".into(), "tail".into())]);
        framer.finish(&mut events);
        assert_eq!(events.len(), 1, "第二次 finish 不能再吐");
    }

    /// `[DONE]` 之后的多余字节不该产生空帧（OpenAI 流尾常见）。
    #[test]
    fn trailing_bytes_after_done_do_not_produce_frames() {
        let mut framer = SseFramer::new();
        let mut events = Vec::new();
        framer.push(b"data: [DONE]\n\n\n\n", &mut events);
        framer.finish(&mut events);
        assert_eq!(render(&events), vec![("".into(), "[DONE]".into())]);
    }
}
