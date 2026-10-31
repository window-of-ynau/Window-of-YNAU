//! 课表 HTML 解析（契约 + 入口）。
//!
//! 输入：教务系统导出的课表页 HTML（见 `docs/migration/v0-reuse.md` §1 的 R1~R7）。
//! 输出：`Vec<Course>`，可直接喂给 `crate::calendar::expand_course`。
//!
//! **铁律（来自旧 Demo 的教训）**：解析失败必须返回 `Err`，**禁止返回空 Vec**。
//! 旧项目读盘失败时静默返回空列表，导致模型回答"你今天没课"。见 [v0-reuse §4]。
//!
//! # 契约
//!
//! | 输入情况 | 返回 |
//! |---|---|
//! | 找不到 `table#timetable`（`id` 精确匹配，退一步 `class` 整词匹配） | `Err(NoTable)` |
//! | 找不到恰好 7 个"星期X"的表头（R7） | `Err(NoHeader)` |
//! | 某行的单元格数与星期列数不一致（R6） | `Err(ColumnMismatch { .. })` |
//! | 解析后一门课都没有 | `Err(Empty)` |
//! | 个别单元格/课程块解析失败（R5） | `Ok(ParseReport)`，失败项进 `warnings` |
//!
//! 也就是说：**结构性错误整表失败，局部脏数据只降级为告警**——
//! 前者继续解析会产出"错位一格"的错数据（比空表危险），后者丢掉整表则得不偿失。
//!
//! # 与旧实现的差异（有意为之）
//!
//! - 旧实现 3 处 `return None`（`parse_schedule_time.py:44/114/117`）是**无声丢弃**；
//!   这里全部转成 [`ParseReport::warnings`]（人类可读中文，可直接进日志或"解析不完整"提示条）。
//!   告警文案**不含课程名/教室/教师**（v0-reuse §4.2 纪律 3），只带"第 N 行·星期X·第 M 块"定位。
//! - 旧实现的去重键用周次原文（R4 的缺陷）；这里用 `calendar::parse_weeks` 展开后的集合，
//!   使 `1-16` 与 `1-16周` 视作同一门课。`Course.weeks` 本身**仍保留原文**（R8/R9 交给
//!   `calendar::parse_weeks` 展开，展示与排障也看原文）。
//!
//! [v0-reuse §4]: ../../../docs/migration/v0-reuse.md

use thiserror::Error;

use crate::calendar::Course;

mod timetable;

/// 解析错误。每一种都对应一个"用户能看懂"的提示。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("没有找到课表表格：学校页面可能已改版，请反馈")]
    NoTable,
    #[error("没有找到星期表头：学校页面可能已改版，请反馈")]
    NoHeader,
    #[error("解析结果为空：请确认本学期是否已排课，或页面是否改版")]
    Empty,
    /// R6：`td` 数量与表头星期列数不一致。
    ///
    /// 旧实现在这里 `break`，让**整张表按错位一格**解析下去——静默的错数据比空表危险
    /// （用户会按错的课表去上课）。所以这里必须失败。
    #[error("课表第 {row} 行有 {got} 个单元格，与星期列数 {expected} 不一致：课表格式可能已变化，请截图反馈")]
    ColumnMismatch {
        row: usize,
        expected: usize,
        got: usize,
    },
    /// 单元格级别的畸形数据（**课表解析路径不再用它**：R5 要求局部失败只降级为告警，
    /// 所以单元格问题走 [`ParseReport::warnings`]。保留是给后续解析器用的，
    /// 删掉会破坏外部 `match`）。
    #[error("单元格格式异常：{0}")]
    BadCell(String),
    /// 占位：课表解析路径**不会**返回它。
    #[error("未实现：该能力由 W2 交付")]
    NotImplemented,
}

/// 一次解析的结果：课程 + 非致命告警。
///
/// 告警单独拿出来（而不是塞进 [`Course`]）有两个原因：
/// 1. 它属于**这次解析**，不是某门课的属性，落库时不该写进课程表；
/// 2. 它要能直接进日志/提示条，所以是中文的人类可读文本。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseReport {
    pub courses: Vec<Course>,
    /// 人类可读的告警（中文）。条数上限 64 条，超出时最后一条说明省略了多少。
    pub warnings: Vec<String>,
}

/// 把课表 HTML 解析成课程列表。
///
/// - `html`：完整页面源码（UTF-8）
/// - 返回的 `Course.weeks` 保留原始写法（如 `1-16单`），由 `calendar::parse_weeks` 展开
///
/// 这是给"只要课程"的调用方用的便捷入口；需要告警（R5 的可观测性）时用
/// [`parse_timetable_with_report`]。
pub fn parse_timetable(html: &str) -> Result<Vec<Course>, ParseError> {
    parse_timetable_with_report(html).map(|report| report.courses)
}

/// 同 [`parse_timetable`]，但一并返回告警。
///
/// UI 可以据此提示"解析不完整，请核对后反馈"，而**不影响**已解析出的课程可用。
pub fn parse_timetable_with_report(html: &str) -> Result<ParseReport, ParseError> {
    timetable::parse_report(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份最小但结构完整的课表页面（表头 7 天 + 一格一门课）。
    fn minimal_page() -> String {
        let mut page = String::from(
            "<html><body><table id=\"timetable\"><tr><th>节次</th>\
             <th>星期一</th><th>星期二</th><th>星期三</th>\
             <th>星期四</th><th>星期五</th><th>星期六</th><th>星期日</th></tr>\
             <tr><th>第1节</th><td><div class=\"kbcontent\">\
             <font>高等数学A</font>\
             <font title=\"教师\">张老师</font>\
             <font title=\"周次(节次)\">1-16[01-02节]</font>\
             <font title=\"教室\">东305</font>\
             </div></td>",
        );
        for _ in 0..6 {
            page.push_str("<td></td>");
        }
        page.push_str("</tr></table></body></html>");
        page
    }

    #[test]
    fn contract_never_returns_empty_vec() {
        // 空输入、纯文本、页面只改了个 id → 全部显式失败，绝不 Ok(vec![])
        assert!(matches!(parse_timetable(""), Err(ParseError::NoTable)));
        assert!(matches!(
            parse_timetable("<html><body><p>请先登录</p></body></html>"),
            Err(ParseError::NoTable)
        ));
        for html in [
            "",
            "<html><body></body></html>",
            "<table id=\"timetable\"></table>",
            "<table id=\"timetable\"><tr><td>x</td></tr></table>",
        ] {
            if let Ok(courses) = parse_timetable(html) {
                panic!("{html:?} 不允许解析出空列表：{courses:?}");
            }
        }
    }

    #[test]
    fn parse_timetable_is_the_report_entry_without_warnings() {
        let page = minimal_page();
        let courses = parse_timetable(&page).expect("正常页面必须解析成功");
        let report = parse_timetable_with_report(&page).expect("同上");
        assert_eq!(courses, report.courses);
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].name, "高等数学A");
        assert_eq!(courses[0].weekday, 1);
        assert_eq!((courses[0].start_period, courses[0].end_period), (1, 2));
        assert_eq!(courses[0].weeks, "1-16");
        assert_eq!(courses[0].room.as_deref(), Some("东305"));
        assert_eq!(courses[0].teacher.as_deref(), Some("张老师"));
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn error_messages_are_user_facing() {
        // 文案是产品的一部分：三种失败必须能分辨（空课表 vs 改版 vs 格式变化）
        assert!(ParseError::NoTable.to_string().contains("课表表格"));
        assert!(ParseError::NoHeader.to_string().contains("星期表头"));
        assert!(ParseError::Empty.to_string().contains("解析结果为空"));
        let mismatch = ParseError::ColumnMismatch {
            row: 3,
            expected: 7,
            got: 9,
        };
        let text = mismatch.to_string();
        assert!(text.contains("第 3 行") && text.contains('9') && text.contains('7'));
    }
}
