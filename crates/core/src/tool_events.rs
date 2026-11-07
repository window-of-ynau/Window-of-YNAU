//! 工具事件的 wire 形状（Rust → Dart，t102/t103 契约）。
//!
//! 工具循环（[`crate::tool_loop`]）内部用 [`crate::tool_loop::LoopEvent`]；
//! **对外**（FFI 回调流、SSE）只发下面这两种 JSON，字段名是契约的一部分，逐字匹配：
//!
//! ```json
//! {"type":"tool_call","call_id":"c1","name":"timetable_query","args":{"week":8}}
//! {"type":"tool_result","call_id":"c1","ok":true,"summary":"5 条结果"}
//! {"type":"tool_result","call_id":"c1","ok":false,"summary":"超时 10s","code":"TIMEOUT"}
//! ```
//!
//! # 四条硬约束（照抄任务契约）
//!
//! 1. `args` **原样透传**：模型给的对象/字符串原样塞进去，不改写、不重排；
//! 2. `summary` **必须是短句**（≤ [`SUMMARY_MAX_CHARS`] 字符）：大 JSON 不进对话流，
//!    工具结果本体留在 Rust 侧/记忆里；
//! 3. 失败必须 `ok:false` + 可读原因 + `code`（`TIMEOUT` / `BAD_ARGS` / `NOT_ALLOWED` /
//!    `CANCELLED` / `TOOL_ERROR`），不能只有一句"失败"；
//! 4. **配对永不悬空**：同一 `call_id` 的 `tool_call` 一定在 `tool_result` 之前，
//!    一个 `call_id` 只有一对；并发调用按实际发生顺序。
//!
//! 第 4 条由 [`PairGuard`] 兜底：无论流以什么方式结束（正常/取消/超时/轮次上限/上游错误），
//! [`PairGuard::close_dangling`] 都会把还没回结果的调用补一条 `ok:false` 的 result。

use serde_json::Value;

/// `summary` 的字符上限（超出截断并加 `…`）。
pub const SUMMARY_MAX_CHARS: usize = 200;

/// 失败原因 → 契约错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCode {
    Timeout,
    BadArgs,
    NotAllowed,
    Cancelled,
    /// 工具循环轮次用满仍未收敛（BYOK 24 / 套餐 4）
    RoundLimit,
    ToolError,
}

impl FailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "TIMEOUT",
            Self::BadArgs => "BAD_ARGS",
            Self::NotAllowed => "NOT_ALLOWED",
            Self::Cancelled => "CANCELLED",
            Self::RoundLimit => "ROUND_LIMIT",
            Self::ToolError => "TOOL_ERROR",
        }
    }

    /// 从工具失败文案里猜一个**稳定**的错误码（工具层的文案是固定前缀，见 `tool_loop`）。
    pub fn from_message(message: &str) -> Self {
        if message.contains("超时") || message.contains("timeout") {
            Self::Timeout
        } else if message.contains("已取消") || message.contains("未执行") {
            Self::Cancelled
        } else if message.contains("未知工具") || message.contains("不允许") {
            Self::NotAllowed
        } else if message.contains("参数") || message.contains("JSON") {
            Self::BadArgs
        } else {
            Self::ToolError
        }
    }
}

/// `tool_call` 事件。
///
/// `args` 原样嵌入（对象/字符串/数组都行），`index` 只在并行调用时出现（与 `Chunk` 一致：
/// `None` 不序列化，老客户端不受影响）。
pub fn tool_call_json(call_id: &str, name: &str, args: &Value, index: Option<u32>) -> String {
    let mut object = serde_json::Map::new();
    object.insert("type".to_owned(), Value::String("tool_call".to_owned()));
    object.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
    object.insert("name".to_owned(), Value::String(name.to_owned()));
    object.insert("args".to_owned(), args.clone());
    if let Some(index) = index {
        object.insert("index".to_owned(), Value::from(index));
    }
    Value::Object(object).to_string()
}

/// `tool_result` 事件（`code` 只在失败时出现）。
pub fn tool_result_json(
    call_id: &str,
    ok: bool,
    summary: &str,
    code: Option<FailureCode>,
) -> String {
    let mut object = serde_json::Map::new();
    object.insert("type".to_owned(), Value::String("tool_result".to_owned()));
    object.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
    object.insert("ok".to_owned(), Value::Bool(ok));
    object.insert("summary".to_owned(), Value::String(summarize(summary)));
    if let Some(code) = code {
        object.insert("code".to_owned(), Value::String(code.as_str().to_owned()));
    }
    Value::Object(object).to_string()
}

/// 把工具输出压成一句人话：取首行、合并空白、截到 [`SUMMARY_MAX_CHARS`]。
///
/// 大 JSON 不进对话流——这里只留一句可读的话，原文留在 Rust 侧（记忆/日志）。
pub fn summarize(content: &str) -> String {
    let first_line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let squeezed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    clip_chars(&squeezed, SUMMARY_MAX_CHARS)
}

fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 配对守卫：保证"发出去的每个 `tool_call` 都有且只有一个 `tool_result`"。
///
/// 用法：调用 `tool_call` 时 [`PairGuard::opened`]，收到结果时 [`PairGuard::closed`]；
/// 流无论怎么结束都要调 [`PairGuard::drain`] 把剩下的补上。
#[derive(Debug, Default)]
pub struct PairGuard {
    open: Vec<String>,
    closed: usize,
}

impl PairGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一个已发出的 `tool_call`（同一个 `call_id` 只记一次）。
    pub fn opened(&mut self, call_id: &str) {
        if !self.open.iter().any(|id| id == call_id) {
            self.open.push(call_id.to_owned());
        }
    }

    /// 记一个已发出的 `tool_result`。
    pub fn closed(&mut self, call_id: &str) {
        self.open.retain(|id| id != call_id);
        self.closed += 1;
    }

    /// 还没回结果的调用（按发出顺序）。
    pub fn dangling(&self) -> &[String] {
        &self.open
    }

    /// 已发出的 result 条数（测试断言用）。
    pub fn closed_count(&self) -> usize {
        self.closed
    }

    /// 把还没回结果的调用全部补成 `ok:false`，返回要发出去的 JSON。
    ///
    /// `reason`/`code` 说明为什么没等到结果（取消、轮次上限、上游错误…）。
    pub fn drain(&mut self, reason: &str, code: FailureCode) -> Vec<String> {
        let ids = std::mem::take(&mut self.open);
        ids.iter()
            .map(|id| {
                self.closed += 1;
                tool_result_json(id, false, reason, Some(code))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_json_matches_the_contract_verbatim() {
        let json = tool_call_json(
            "c1",
            "timetable_query",
            &serde_json::json!({ "week": 8 }),
            None,
        );
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "tool_call");
        assert_eq!(value["call_id"], "c1");
        assert_eq!(value["name"], "timetable_query");
        // args 原样：对象就是对象，键没被改名
        assert_eq!(value["args"], serde_json::json!({ "week": 8 }));
        // 没有并行下标时不出现该字段
        assert!(value.get("index").is_none(), "{value}");
    }

    #[test]
    fn tool_call_args_are_passed_through_unchanged() {
        // 模型给的字符串参数不能被"顺手"解析成对象
        let raw = serde_json::json!("{\"week\":8}");
        let value: Value =
            serde_json::from_str(&tool_call_json("c9", "tool", &raw, Some(2))).unwrap();
        assert_eq!(value["args"], serde_json::json!("{\"week\":8}"));
        assert_eq!(value["index"], 2);
    }

    #[test]
    fn tool_result_json_ok_and_failure_shapes() {
        let ok: Value =
            serde_json::from_str(&tool_result_json("c1", true, "5 条结果", None)).unwrap();
        assert_eq!(ok["type"], "tool_result");
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["summary"], "5 条结果");
        assert!(ok.get("code").is_none(), "成功不该带 code：{ok}");

        let failed: Value = serde_json::from_str(&tool_result_json(
            "c1",
            false,
            "超时 10s",
            Some(FailureCode::Timeout),
        ))
        .unwrap();
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["summary"], "超时 10s");
        assert_eq!(failed["code"], "TIMEOUT");
    }

    #[test]
    fn failure_codes_are_stable_and_readable() {
        assert_eq!(
            FailureCode::from_message("工具执行超时（10s），已放弃本次结果"),
            FailureCode::Timeout
        );
        assert_eq!(
            FailureCode::from_message("已取消，未执行"),
            FailureCode::Cancelled
        );
        assert_eq!(
            FailureCode::from_message("未知工具：foo"),
            FailureCode::NotAllowed
        );
        assert_eq!(FailureCode::from_message("参数缺失"), FailureCode::BadArgs);
        assert_eq!(
            FailureCode::from_message("磁盘炸了"),
            FailureCode::ToolError
        );
        assert_eq!(FailureCode::Timeout.as_str(), "TIMEOUT");
    }

    #[test]
    fn summary_is_one_short_line() {
        // 多行 + 巨长的 JSON：只留首行、截到 200 字
        let big = format!("{}{}", "结果：5 条\n", "x".repeat(5_000));
        let summary = summarize(&big);
        assert_eq!(summary, "结果：5 条");
        assert!(!summary.contains('\n'));

        let long = "字".repeat(500);
        let summary = summarize(&long);
        assert!(
            summary.chars().count() <= SUMMARY_MAX_CHARS,
            "{}",
            summary.chars().count()
        );
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn pair_guard_never_leaves_a_dangling_call() {
        let mut guard = PairGuard::new();
        guard.opened("c1");
        guard.opened("c2");
        guard.closed("c1");
        assert_eq!(guard.dangling(), ["c2".to_owned()]);

        let drained = guard.drain("流已结束，工具未返回结果", FailureCode::Cancelled);
        assert_eq!(drained.len(), 1, "{drained:?}");
        let value: Value = serde_json::from_str(&drained[0]).unwrap();
        assert_eq!(value["call_id"], "c2");
        assert_eq!(value["ok"], false);
        assert_eq!(value["code"], "CANCELLED");
        assert!(guard.dangling().is_empty());
        // 再 drain 一次不该重复补
        assert!(guard.drain("再来一次", FailureCode::Cancelled).is_empty());
        assert_eq!(guard.closed_count(), 2);
    }
}
