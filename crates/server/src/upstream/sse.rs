//! SSE 事件解码：从上游字节流里切出**完整事件**的 `data:` 负载
//! —— 切分逻辑已收敛到 `yunong_shared::sse`（issue #18）。
//!
//! 这里只剩一层薄适配：把共享帧切分器产出的事件映射成"纯 `data:` 负载"。
//! 服务端**不需要**事件名（三家上游的 `event:` 叫法都不一样，Completions 干脆没有），
//! 也不理解 JSON——语义交给 `yunong_core::protocols::parse_events`。
//!
//! 与端侧唯一的有意差异：这里用 [`SseFramer::new_strict_utf8`]（非 UTF-8 行丢弃整个事件），
//! 保留服务端原有语义；端侧用的是 lossy。

use yunong_shared::SseFramer;

/// 增量解码器：可以喂任意切分的字节片段（网络读到的边界不可控）。
#[derive(Debug, Default)]
pub struct SseDecoder {
    framer: SseFramer,
}

impl SseDecoder {
    /// 严格 UTF-8 版（服务端语义：乱码行不往下传）。
    pub fn new() -> Self {
        Self {
            framer: SseFramer::new_strict_utf8(),
        }
    }

    /// 喂一段字节，把**完整事件**的 data 负载追加到 `out`。
    ///
    /// 只有真的出现过 `data:` 的帧才输出（`event: x` 单独出现时不上报，与旧行为一致）。
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<String>) {
        let mut events = Vec::new();
        self.framer.push(input, &mut events);
        out.extend(
            events
                .into_iter()
                .filter(|event| event.had_data)
                .map(|event| event.data),
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn decode(input: &[u8]) -> Vec<String> {
        let mut decoder = SseDecoder::new();
        let mut out = Vec::new();
        decoder.feed(input, &mut out);
        out
    }

    #[test]
    fn single_event_is_extracted() {
        let out = decode(b"event: delta\ndata: {\"a\":1}\n\n");
        assert_eq!(out, vec!["{\"a\":1}".to_owned()]);
    }

    #[test]
    fn crlf_and_comment_lines_are_handled() {
        let out = decode(b": ping\r\n\r\ndata: [DONE]\r\n\r\n");
        assert_eq!(out, vec!["[DONE]".to_owned()], "注释行不能变成事件");
    }

    #[test]
    fn multi_line_data_is_joined_with_newline() {
        let out = decode(b"data: {\"a\":\ndata: 1}\n\n");
        assert_eq!(out, vec!["{\"a\":\n1}".to_owned()]);
    }

    #[test]
    fn data_without_space_is_accepted() {
        let out = decode(b"data:\"x\"\n\n");
        assert_eq!(out, vec!["\"x\"".to_owned()]);
    }

    #[test]
    fn split_across_chunks_still_decodes() {
        let mut decoder = SseDecoder::new();
        let mut out = Vec::new();
        // 一次只喂一个字节，模拟网络分片
        for byte in b"data: {\"n\":1}\n\ndata: [DONE]\n\n" {
            decoder.feed(&[*byte], &mut out);
        }
        assert_eq!(out, vec!["{\"n\":1}".to_owned(), "[DONE]".to_owned()]);
    }

    #[test]
    fn unfinished_event_is_not_emitted() {
        let out = decode(b"data: {\"a\":1}");
        assert!(out.is_empty(), "没有空行收尾，事件还不完整");
    }

    /// 与端侧**共用**的用例表：服务端只关心 `data`，但切分结果必须与表一致。
    #[test]
    fn shared_case_table_holds_here() {
        for case in yunong_shared::sse::SSE_CASES {
            let mut decoder = SseDecoder::new();
            let mut out = Vec::new();
            decoder.feed(case.wire.as_bytes(), &mut out);
            if case.needs_finish {
                // **有意的语义差异**：服务端历史上没有"收尾吐最后一帧"（流被截断就丢掉），
                // 这里保持原样，不去顺手改上游代理解析行为。
                assert!(out.is_empty(), "服务端不 flush 未收尾帧：{case:?}");
                continue;
            }
            let expected: Vec<String> = case
                .frames
                .iter()
                .map(|(_, data)| (*data).to_owned())
                .collect();
            assert_eq!(out, expected, "case={case:?}");
        }
    }

    #[test]
    fn non_utf8_line_drops_instead_of_panicking() {
        // 非 UTF-8 行（上游乱码）不能让服务端 panic
        let mut input = b"data: \xff\xfe\n\n".to_vec();
        input.extend_from_slice(b"data: ok\n\n");
        let out = decode(&input);
        assert_eq!(out, vec!["ok".to_owned()]);
    }
}
