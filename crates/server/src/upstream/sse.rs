//! SSE 事件解码：从上游字节流里切出**完整事件**的 `data:` 负载。
//!
//! 只做 wire 层的事（行切分、多行 `data` 合并、注释行忽略），不理解 JSON——
//! 语义交给 `yunong_core::protocols::parse_events`。
//!
//! 为什么还需要它：HTTP 传输交给 `reqwest`（TLS / 分块解码 / 重定向都是它的事），
//! 但它只给"没凑齐一行的字节块"。归一化只要 `data:` 负载——上游的 `event:` 名字
//! 三家都不一样，Completions 干脆不带 `event:`，所以帧切分这一层留着更省事。

/// 增量解码器：可以喂任意切分的字节片段（网络读到的边界不可控）。
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// 还没凑成完整一行的字节
    line: Vec<u8>,
    /// 当前事件已收集的 data（多行按规范用 `\n` 连接）
    data: Option<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一段字节，把**完整事件**的 data 负载追加到 `out`。
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<String>) {
        for &byte in input {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.handle_line(&line, out);
            } else {
                self.line.push(byte);
            }
        }
    }

    fn handle_line(&mut self, line: &[u8], out: &mut Vec<String>) {
        // 空行 = 事件结束
        if line.is_empty() {
            if let Some(data) = self.data.take() {
                out.push(data);
            }
            return;
        }
        // 注释（`: keep-alive`）与 `event:` / `id:` / `retry:` 都不进 data
        let Ok(text) = std::str::from_utf8(line) else {
            // 非 UTF-8 行：丢掉当前事件，别把乱码当 JSON 往下传
            self.data = None;
            return;
        };
        let Some(rest) = text.strip_prefix("data:") else {
            return;
        };
        // 规范允许 `data:x` 与 `data: x` 两种写法
        let value = rest.strip_prefix(' ').unwrap_or(rest);
        match &mut self.data {
            Some(data) => {
                data.push('\n');
                data.push_str(value);
            }
            None => self.data = Some(value.to_owned()),
        }
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

    #[test]
    fn non_utf8_line_drops_instead_of_panicking() {
        // 非 UTF-8 行（上游乱码）不能让服务端 panic
        let mut input = b"data: \xff\xfe\n\n".to_vec();
        input.extend_from_slice(b"data: ok\n\n");
        let out = decode(&input);
        assert_eq!(out, vec!["ok".to_owned()]);
    }
}
