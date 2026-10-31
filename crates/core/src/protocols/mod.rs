//! 三协议适配（契约已锁定，实现由 `agent/protocols` 在 W2 补齐）。
//!
//! 这一层只做**纯数据变换**：把内部 `ChatRequest` 编成各家请求体，
//! 把各家的流式事件解析成统一的 `Chunk`（ADR-13）。
//!
//! 刻意**不在这里发网络请求**：网络交给 `gateway`，这样适配逻辑可以用 fixture 单测，
//! 不需要真实 Key、不需要联网。

use serde_json::Value;
use yunong_shared::{ChatRequest, Chunk, Protocol};

/// 构造对应协议的请求体。
///
/// 必须处理的差异（见 `docs/03-接口文档.md` §9）：
/// - 系统提示位置：`messages[0]` / `instructions` / 顶层 `system`
/// - 工具声明：`tools[].function` / `tools[]` / `tools[].input_schema`
/// - 流式开关字段名
pub fn build_body(_protocol: Protocol, _request: &ChatRequest) -> Value {
    unimplemented!("W2 / agent/protocols：按 docs/03-接口文档.md §9 实现")
}

/// 解析一条流式事件（`data: ` 前缀已剥离；可能是 `[DONE]`）。
///
/// 返回 `None` 表示该事件不需要暴露给上层（例如 keep-alive、空 delta）。
pub fn parse_event(_protocol: Protocol, _data: &str) -> Option<Chunk> {
    unimplemented!("W2 / agent/protocols：按 docs/03-接口文档.md §9 实现")
}

#[cfg(test)]
mod tests {
    use yunong_shared::Protocol;

    /// 契约测试骨架：实现落地后在这里对三协议跑同一组 fixture。
    #[test]
    fn all_three_protocols_must_be_covered() {
        // 提醒实现者：三种协议各要一组 fixture，缺一个就是缺口。
        let protocols = [
            Protocol::OpenaiCompletions,
            Protocol::OpenaiResponses,
            Protocol::AnthropicMessages,
        ];
        assert_eq!(protocols.len(), 3);
    }
}
