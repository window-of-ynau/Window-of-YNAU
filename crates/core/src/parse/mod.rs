//! 课表 HTML 解析（契约已锁定，实现由 `agent/timetable` 在 W2 补齐）。
//!
//! 输入：教务系统导出的课表页 HTML（见 `docs/migration/v0-reuse.md` §2 的 R1~R12）。
//! 输出：`Vec<Course>`，可直接喂给 `crate::calendar::expand_course`。
//!
//! **铁律（来自旧 Demo 的教训）**：解析失败必须返回 `Err`，**禁止返回空 Vec**。
//! 旧项目读盘失败时静默返回空列表，导致模型回答"你今天没课"。见 [v0-reuse §4]。

use thiserror::Error;

use crate::calendar::Course;

/// 解析错误。每一种都对应一个"用户能看懂"的提示。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("没有找到课表表格：学校页面可能已改版，请反馈")]
    NoTable,
    #[error("没有找到星期表头：学校页面可能已改版，请反馈")]
    NoHeader,
    #[error("解析结果为空：请确认本学期是否已排课，或页面是否改版")]
    Empty,
    #[error("单元格格式异常：{0}")]
    BadCell(String),
    #[error("未实现：该能力由 W2 交付")]
    NotImplemented,
}

/// 把课表 HTML 解析成课程列表。
///
/// - `html`：完整页面源码（UTF-8）
/// - 返回的 `Course.weeks` 保留原始写法（如 `1-16单`），由 `calendar::parse_weeks` 展开
pub fn parse_timetable(_html: &str) -> Result<Vec<Course>, ParseError> {
    // TODO(W2 / agent/timetable)：按 v0-reuse R1~R12 实现，并补齐 fixture 测试
    Err(ParseError::NotImplemented)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unimplemented_must_be_explicit_error_not_empty_vec() {
        // 这条测试在实现落地后应当改为真实 fixture 测试；
        // 它现在的作用是**锁住契约**：绝不能返回 Ok(vec![]) 骗人。
        assert!(matches!(
            parse_timetable("<html></html>"),
            Err(ParseError::NotImplemented)
        ));
    }
}
