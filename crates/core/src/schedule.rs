//! 门户课表接口（`queryAWeekSchedule`）的周数据合并。
//!
//! 真实形态（owner 抓包确认，W6）：门户是 React 单页，课表来自
//! `GET /api/uppcard/kbsz/queryAWeekSchedule?years=<学期>&results=<周>&cardId=<卡片>`；
//! 响应 `data.resultsJsonArr` 是 **7 天 × 10 节**的二维数组，每格是
//! `"课程名,教室,班级;,教师"` 或空串。**同一门课在连续节次里重复出现**，
//! 所以"拉完 20 周 → 直接入库"会得到一堆 1 节课的碎片，必须合并：
//!
//! ```text
//! ① 逐周逐格解析 → (课程名, 教室, 教师) 为键
//! ② 同一天里相邻节次且**周集合相同**才合并成节次区间（周一 1-3 节）
//! ③ 该课出现在哪些周 → 压缩成 `1-16` / `1-8,10-16`
//! ```
//!
//! 为什么按"周集合相同"才合并：同一门课可能周三 1-2 节上周 1-8、1-10 节上周 9-16，
//! 这两段虽然节次相邻但周次不同，混成一段会让"本周有没有课"判错。
//!
//! 隐私：告警里只出现"第 N 周·周X·第 M 节"这类定位，**不出现课程名/教室/教师**。

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use crate::calendar::Course;

/// 门户接口固定 7 天。
pub const MAX_DAYS: usize = 7;
/// 门户接口固定 10 节（上午 4 / 下午 4 / 晚上 2）。
pub const MAX_PERIODS: usize = 10;
/// 告警条数上限（与 HTML 解析保持一致），避免畸形数据吃内存。
const MAX_WARNINGS: usize = 64;

/// 端侧入库的输入：Dart 侧把每周响应原样带上，合并交给 Rust（可测）。
#[derive(Debug, Clone, Deserialize)]
pub struct SchedulePayload {
    /// 成功拉到的周：`{ "week": 3, "data": <门户 data 字段> }`
    #[serde(default)]
    pub weeks: Vec<WeekPayload>,
    /// 拉取失败的周号（由 Dart 侧如实报告，不参与合并但要透传到结果里）
    #[serde(default)]
    pub missing_weeks: Vec<u32>,
    /// 该学期总周数（门户 `data.weekcount`），用于校验与展示
    #[serde(default)]
    pub weekcount: Option<u32>,
}

/// 单周的门户响应。
#[derive(Debug, Clone, Deserialize)]
pub struct WeekPayload {
    pub week: u32,
    #[serde(default)]
    pub data: Value,
}

/// 合并结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedSchedule {
    pub courses: Vec<Course>,
    pub warnings: Vec<String>,
    /// 实际参与合并的周（升序去重）
    pub weeks_seen: Vec<u32>,
    /// 缺失的周（升序去重）
    pub missing_weeks: Vec<u32>,
    pub weekcount: Option<u32>,
}

impl MergedSchedule {
    /// 是否部分成功（有周没拉到，但已有周照常入库）。
    pub fn is_partial(&self) -> bool {
        !self.missing_weeks.is_empty()
    }
}

/// 一行/一格里的课程信息。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct CourseKey {
    name: String,
    room: Option<String>,
    teacher: Option<String>,
}

/// 合并入口：把"每周的 portal data"折成 `courses` 表要的行。
pub fn merge(payload: &SchedulePayload) -> MergedSchedule {
    let mut warnings: Vec<String> = Vec::new();
    // key → 周几 → 第几节(0 基) → 周集合
    let mut slots: BTreeMap<CourseKey, BTreeMap<u8, BTreeMap<u8, BTreeSet<u32>>>> = BTreeMap::new();
    let mut weeks_seen: BTreeSet<u32> = BTreeSet::new();

    let mut weeks: Vec<&WeekPayload> = payload.weeks.iter().collect();
    weeks.sort_by_key(|week| week.week);

    for week in weeks {
        let week_no = week.week;
        weeks_seen.insert(week_no);
        let Some(cells) = week.data.get("resultsJsonArr").and_then(Value::as_array) else {
            push_warning(
                &mut warnings,
                format!("第 {week_no} 周缺少 resultsJsonArr，已跳过"),
            );
            continue;
        };
        if cells.len() < MAX_DAYS {
            push_warning(
                &mut warnings,
                format!(
                    "第 {week_no} 周只有 {} 天（应为 {MAX_DAYS} 天），按现有数据处理",
                    cells.len()
                ),
            );
        }
        for (day_index, day) in cells.iter().enumerate().take(MAX_DAYS) {
            let Some(periods) = day.as_array() else {
                push_warning(
                    &mut warnings,
                    format!(
                        "第 {week_no} 周·周{} 的数据不是数组，已跳过",
                        cn_weekday(day_index)
                    ),
                );
                continue;
            };
            if periods.len() < MAX_PERIODS {
                push_warning(
                    &mut warnings,
                    format!(
                        "第 {week_no} 周·周{} 只有 {} 节（应为 {MAX_PERIODS} 节），按现有数据处理",
                        cn_weekday(day_index),
                        periods.len()
                    ),
                );
            }
            for (period_index, cell) in periods.iter().enumerate().take(MAX_PERIODS) {
                let Some(raw) = cell.as_str() else {
                    continue;
                };
                let Some(parts) = parse_cell(raw) else {
                    continue;
                };
                slots
                    .entry(parts)
                    .or_default()
                    .entry(day_index as u8)
                    .or_default()
                    .entry(period_index as u8)
                    .or_default()
                    .insert(week_no);
            }
        }
    }

    // 汇总：同一天里相邻且周集合相同的节次合并成区间
    let mut courses: Vec<Course> = Vec::new();
    for (key, by_day) in slots {
        for (day, periods) in by_day {
            let mut iter = periods.into_iter().peekable();
            let mut range_start: u8 = 0;
            let mut range_end: u8 = 0;
            let mut range_weeks: BTreeSet<u32> = BTreeSet::new();
            let mut open = false;
            while let Some((period, weeks)) = iter.next() {
                if open && period == range_end + 1 && weeks == range_weeks {
                    range_end = period;
                    continue;
                }
                if open {
                    courses.push(build_course(
                        &key,
                        day,
                        range_start,
                        range_end,
                        &range_weeks,
                    ));
                }
                range_start = period;
                range_end = period;
                range_weeks = weeks;
                open = true;
            }
            if open {
                courses.push(build_course(
                    &key,
                    day,
                    range_start,
                    range_end,
                    &range_weeks,
                ));
            }
        }
    }

    // 稳定顺序：周几 → 节次 → 课程名（UI 直接渲染，不用再排）
    courses.sort_by(|a, b| {
        (a.weekday, a.start_period, a.end_period, a.name.as_str()).cmp(&(
            b.weekday,
            b.start_period,
            b.end_period,
            b.name.as_str(),
        ))
    });

    let mut missing = payload.missing_weeks.clone();
    missing.sort_unstable();
    missing.dedup();

    MergedSchedule {
        courses,
        warnings,
        weeks_seen: weeks_seen.into_iter().collect(),
        missing_weeks: missing,
        weekcount: payload.weekcount,
    }
}

fn build_course(key: &CourseKey, day: u8, start: u8, end: u8, weeks: &BTreeSet<u32>) -> Course {
    Course {
        name: key.name.clone(),
        weekday: day + 1,
        // 入库用 1 基节次（与 `courses.start_period` 对齐）
        start_period: start + 1,
        end_period: end + 1,
        weeks: compress_weeks(weeks),
        room: key.room.clone(),
        teacher: key.teacher.clone(),
    }
}

/// 把周集合压缩成区间字符串：`{1..16}` → `1-16`，`{1..8,10..16}` → `1-8,10-16`，
/// `{3}` → `3`。
pub fn compress_weeks(weeks: &BTreeSet<u32>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut iter = weeks.iter().copied().peekable();
    while let Some(start) = iter.next() {
        let mut end = start;
        while let Some(next) = iter.peek().copied() {
            if next == end + 1 {
                end = next;
                iter.next();
            } else {
                break;
            }
        }
        if end > start {
            parts.push(format!("{start}-{end}"));
        } else {
            parts.push(start.to_string());
        }
    }
    parts.join(",")
}

/// 解析一格：`"高等数学I,东校区1教305,班级;,张老师"`。
///
/// 容错：字段少于 2 个 → 只有课名；`班级` 字段（可能带 `;`）忽略；
/// 空串 / `-` / 纯空白 → `None`（视为没课）。
fn parse_cell(raw: &str) -> Option<CourseKey> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "-" || trimmed == "--" {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(',').map(str::trim).collect();
    let name = parts.first().copied().unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let room = parts
        .get(1)
        .filter(|part| !part.is_empty())
        .map(|part| (*part).to_owned());
    // 最后一列是教师；但如果只有 3 列且第 3 列是班级（以 ; 结尾），就没有教师
    let teacher = match parts.len() {
        0..=2 => None,
        _ => {
            let last = parts.last()?.trim();
            if last.is_empty() || last.ends_with(';') {
                None
            } else {
                Some(last.to_owned())
            }
        }
    };
    Some(CourseKey {
        name: name.to_owned(),
        room,
        teacher,
    })
}

fn cn_weekday(day_index: usize) -> &'static str {
    const NAMES: [&str; MAX_DAYS] = ["一", "二", "三", "四", "五", "六", "日"];
    NAMES.get(day_index).copied().unwrap_or("?")
}

fn push_warning(warnings: &mut Vec<String>, message: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(message);
    } else if warnings.len() == MAX_WARNINGS {
        warnings.push("（其余告警已省略）".to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 造一周的 `resultsJsonArr`：7 天 × 10 节，默认全空。
    fn empty_week() -> Vec<Vec<String>> {
        vec![vec![String::new(); MAX_PERIODS]; MAX_DAYS]
    }

    fn set(cells: &mut [Vec<String>], day: usize, period: usize, value: &str) {
        cells[day][period] = value.to_owned();
    }

    fn week(week_no: u32, cells: Vec<Vec<String>>) -> WeekPayload {
        WeekPayload {
            week: week_no,
            data: json!({ "resultsJsonArr": cells, "weekcount": "20" }),
        }
    }

    fn payload(weeks: Vec<WeekPayload>) -> SchedulePayload {
        SchedulePayload {
            weeks,
            missing_weeks: Vec::new(),
            weekcount: Some(20),
        }
    }

    #[test]
    fn adjacent_periods_of_same_course_are_merged() {
        // 高等数学I 在周一占 1/2/3 节（连续三格），应合并成 1-3 节
        let mut cells = empty_week();
        for period in 0..3 {
            set(&mut cells, 0, period, "高等数学I,东校区1教305,班级;,张老师");
        }
        let merged = merge(&payload(vec![week(1, cells)]));
        assert_eq!(merged.courses.len(), 1, "{:?}", merged.courses);
        let course = &merged.courses[0];
        assert_eq!(course.name, "高等数学I");
        assert_eq!(course.weekday, 1);
        assert_eq!(course.start_period, 1);
        assert_eq!(course.end_period, 3);
        assert_eq!(course.weeks, "1");
        assert_eq!(course.room.as_deref(), Some("东校区1教305"));
        assert_eq!(course.teacher.as_deref(), Some("张老师"));
    }

    #[test]
    fn non_adjacent_periods_stay_separate() {
        let mut cells = empty_week();
        set(&mut cells, 1, 0, "数据结构,信息楼A203,班级;,王老师");
        set(&mut cells, 1, 4, "数据结构,信息楼A203,班级;,王老师");
        let merged = merge(&payload(vec![week(1, cells)]));
        assert_eq!(merged.courses.len(), 2, "{:?}", merged.courses);
        assert_eq!(
            (merged.courses[0].start_period, merged.courses[0].end_period),
            (1, 1)
        );
        assert_eq!(
            (merged.courses[1].start_period, merged.courses[1].end_period),
            (5, 5)
        );
    }

    #[test]
    fn weeks_are_compressed_to_ranges() {
        // 1..16 周的周一第 1-2 节 → weeks = "1-16"
        let weeks: Vec<WeekPayload> = (1..=16)
            .map(|week_no| {
                let mut cells = empty_week();
                set(&mut cells, 0, 0, "大学英语,东校区2教101,班级;,李老师");
                set(&mut cells, 0, 1, "大学英语,东校区2教101,班级;,李老师");
                week(week_no, cells)
            })
            .collect();
        let merged = merge(&payload(weeks));
        assert_eq!(merged.courses.len(), 1, "{:?}", merged.courses);
        assert_eq!(merged.courses[0].weeks, "1-16");
        assert_eq!(merged.weeks_seen, (1..=16).collect::<Vec<u32>>());
        assert!(!merged.is_partial());
    }

    #[test]
    fn week_set_with_gap_compresses_to_two_ranges() {
        // 1-8 周 + 10-16 周 → "1-8,10-16"
        let weeks: Vec<WeekPayload> = (1..=16)
            .filter(|week_no| *week_no != 9)
            .map(|week_no| {
                let mut cells = empty_week();
                set(&mut cells, 2, 2, "操作系统,信息楼B101,班级;,赵老师");
                week(week_no, cells)
            })
            .collect();
        let merged = merge(&payload(weeks));
        assert_eq!(merged.courses.len(), 1, "{:?}", merged.courses);
        assert_eq!(merged.courses[0].weeks, "1-8,10-16");
    }

    #[test]
    fn adjacent_periods_with_different_week_sets_are_not_merged() {
        // 同一门课：周一第 1 节上周 1-8，第 2 节上周 9-16 → 两段不能合并
        let mut weeks: Vec<WeekPayload> = Vec::new();
        for week_no in 1..=16 {
            let mut cells = empty_week();
            if week_no <= 8 {
                set(&mut cells, 0, 0, "体育,操场,班级;,孙老师");
            } else {
                set(&mut cells, 0, 1, "体育,操场,班级;,孙老师");
            }
            weeks.push(week(week_no, cells));
        }
        let merged = merge(&payload(weeks));
        assert_eq!(merged.courses.len(), 2, "{:?}", merged.courses);
        assert_eq!(merged.courses[0].weeks, "1-8");
        assert_eq!(merged.courses[1].weeks, "9-16");
    }

    #[test]
    fn same_name_different_room_are_separate_courses() {
        let mut cells = empty_week();
        set(&mut cells, 0, 0, "高等数学I,东305,班级;,张老师");
        set(&mut cells, 1, 0, "高等数学I,东306,班级;,张老师");
        let merged = merge(&payload(vec![week(1, cells)]));
        assert_eq!(merged.courses.len(), 2, "{:?}", merged.courses);
    }

    #[test]
    fn partial_weeks_still_import_and_report_missing() {
        let mut cells = empty_week();
        set(&mut cells, 0, 0, "高等数学I,东305,班级;,张老师");
        let merged = merge(&SchedulePayload {
            weeks: vec![week(1, cells.clone()), week(2, cells)],
            missing_weeks: vec![3, 4],
            weekcount: Some(4),
        });
        assert_eq!(merged.courses.len(), 1);
        assert_eq!(merged.courses[0].weeks, "1-2");
        assert_eq!(merged.missing_weeks, vec![3, 4]);
        assert!(merged.is_partial());
    }

    #[test]
    fn empty_schedule_yields_no_courses_and_no_warnings() {
        let merged = merge(&payload(vec![week(1, empty_week())]));
        assert!(merged.courses.is_empty(), "{:?}", merged.courses);
        assert!(merged.warnings.is_empty(), "{:?}", merged.warnings);
    }

    #[test]
    fn short_rows_and_short_days_are_tolerated() {
        // 只有 3 天、每天只有 2 节：仍然解析，但给出降级告警
        let cells = vec![
            vec!["高等数学I,东305,班级;,张老师".to_owned(), String::new()],
            vec![String::new(), String::new()],
            vec![String::new(), String::new()],
        ];
        let merged = merge(&payload(vec![week(1, cells)]));
        assert_eq!(merged.courses.len(), 1, "{:?}", merged.courses);
        assert_eq!(merged.courses[0].start_period, 1);
        assert_eq!(merged.courses[0].end_period, 1);
        assert!(
            merged
                .warnings
                .iter()
                .any(|warning| warning.contains("只有 3 天")),
            "{:?}",
            merged.warnings
        );
        assert!(
            merged
                .warnings
                .iter()
                .any(|warning| warning.contains("只有 2 节")),
            "{:?}",
            merged.warnings
        );
    }

    #[test]
    fn missing_results_json_arr_is_skipped_with_warning() {
        let merged = merge(&payload(vec![WeekPayload {
            week: 7,
            data: json!({ "weekcount": "20" }),
        }]));
        assert!(merged.courses.is_empty());
        assert!(merged
            .warnings
            .iter()
            .any(|warning| warning.contains("resultsJsonArr")));
    }

    #[test]
    fn warnings_never_leak_course_names() {
        let cells = vec![vec![String::new(); 2]; 2];
        let merged = merge(&payload(vec![week(1, cells)]));
        for warning in &merged.warnings {
            assert!(!warning.contains("高等数学"), "{warning}");
            assert!(!warning.contains("东305"), "{warning}");
        }
    }

    #[test]
    fn cell_parsing_tolerates_missing_fields() {
        let only_name = parse_cell("大学物理").expect("只有课名也要能解析");
        assert_eq!(only_name.name, "大学物理");
        assert_eq!(only_name.room, None);
        assert_eq!(only_name.teacher, None);

        let with_class_only = parse_cell("大学物理,东305,班级;").expect("三列");
        assert_eq!(with_class_only.room.as_deref(), Some("东305"));
        assert_eq!(with_class_only.teacher, None, "第三列是班级就不当教师");

        assert!(parse_cell("").is_none());
        assert!(parse_cell("   ").is_none());
        assert!(parse_cell("-").is_none());
    }

    #[test]
    fn compress_weeks_cases() {
        let set = |values: &[u32]| values.iter().copied().collect::<BTreeSet<u32>>();
        assert_eq!(compress_weeks(&set(&[3])), "3");
        assert_eq!(compress_weeks(&set(&[1, 2, 3, 4, 5, 6, 7, 8])), "1-8");
        assert_eq!(
            compress_weeks(&set(&[1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 16])),
            "1-8,10-16"
        );
        assert_eq!(compress_weeks(&set(&[1, 3, 5])), "1,3,5");
        assert_eq!(compress_weeks(&BTreeSet::new()), "");
    }
}
