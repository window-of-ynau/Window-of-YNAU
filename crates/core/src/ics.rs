//! 课表 → `.ics` 文件的导出入口（FR-F3）。
//!
//! 分工：[`crate::calendar`] 只做纯计算（周次展开、转义、折行，全被单测覆盖）；
//! 本模块负责"从 SQLite 读学期与课程 → 生成文本 → 落盘"，并守住三个边界：
//!
//! - `terms.starts_on` 为空（用户还没确认开学日）→ [`CoreError::TermStartUnknown`]，
//!   **不生成文件**、**不猜默认日期**：猜错一周，整张课表的日期就全错；
//! - 学期没有课 → 仍然生成**空日历**（`events: 0`）：用户先把文件拿到手，
//!   比"点了导出什么也没发生"清楚；
//! - 课表里写坏的周次 / 星期 → 跳过那一行（事件数变少），**不整体失败**：
//!   一门课的数据脏了不该让整学期的导出失败。
//!
//! # 隐私
//!
//! 导出的是课表本身的字段（课名 / 教师 / 教室 / 周次），没有 Cookie、没有账号信息，
//! 用户自己决定把文件发给谁（ADR-02）。

use std::path::{Path, PathBuf};

use time::Date;

use crate::calendar::{
    expand_course, parse_term_start, render_ics, utc_now_stamp, Course, IcsEvent,
};
use crate::error::{CoreError, CoreResult};
use crate::store::CourseStore;

/// 一次导出的结果（FFI 直接摊平成 `{path, events}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcsExport {
    /// 实际写入的文件路径
    pub path: PathBuf,
    /// 写入的事件数（= 课程 × 周次；0 表示生成了空日历）
    pub events: usize,
}

/// 把某学期的课表导出成 `.ics` 文件。
///
/// 参数顺序按调用方直觉排：**先数据库、后输出文件**（FFI 的 `term/out_path/db_path`
/// 顺序是历史包袱，见 `ffi::yunong_export_ics`）。
pub fn export_term_ics(
    term: &str,
    db_path: impl AsRef<Path>,
    out_path: impl AsRef<Path>,
) -> CoreResult<IcsExport> {
    let term = term.trim();
    let db_path = db_path.as_ref();
    let out_path = out_path.as_ref();

    if term.is_empty() {
        return Err(CoreError::Invalid("学期不能为空".into()));
    }
    if out_path.as_os_str().is_empty() {
        return Err(CoreError::Invalid("导出路径不能为空".into()));
    }
    // 库都不存在 = 从没抓过课表。**不要**顺手建一个空库：那会让"没抓过"和
    // "抓过但没课"长得一模一样。
    if !db_path.exists() {
        return Err(CoreError::TermStartUnknown(format!(
            "本地还没有「{term}」的课表，请先抓取课表并确认开学日期"
        )));
    }

    let store = CourseStore::open(db_path)?;
    let events = build_events(&store, term)?;
    // 先把文本整个拼好再落盘：中途出错就不会留下半份 .ics
    let text = render_ics(term, &events, &utc_now_stamp());
    write_calendar(out_path, &text)?;
    Ok(IcsExport {
        path: out_path.to_path_buf(),
        events: events.len(),
    })
}

/// 从库里读出某学期并展开成事件（不落盘，便于复用与测试）。
pub fn build_events(store: &CourseStore, term: &str) -> CoreResult<Vec<IcsEvent>> {
    let term = term.trim();
    let term_start = term_starts_on(store, term)?;
    let mut events: Vec<IcsEvent> = Vec::new();
    for stored in store.list_courses(term)? {
        // 库里多一列（id/term）在这里被丢掉：导出只需要日程字段
        let course = Course {
            name: stored.name,
            weekday: stored.weekday,
            start_period: stored.start_period,
            end_period: stored.end_period,
            weeks: stored.weeks,
            room: stored.room,
            teacher: stored.teacher,
        };
        events.extend(expand_course(term, term_start, &course));
    }
    // 稳定排序（日期 → UID）：同一个库导出两次，字节级别完全一致，便于 diff 与测试
    events.sort_by(|a, b| (a.date, a.uid.as_str()).cmp(&(b.date, b.uid.as_str())));
    Ok(events)
}

/// 取某学期的开学日（第 1 周周一）。
///
/// 四种"拿不到"的情况都映射成 [`CoreError::TermStartUnknown`]：学期不存在、
/// `starts_on` 为空、日期形状不合法、日期本身不存在（如 2 月 30 日）。
/// 界面上它们的处理是同一个动作：**让用户确认开学日**。
fn term_starts_on(store: &CourseStore, term: &str) -> CoreResult<Date> {
    let terms = store.list_terms()?;
    let Some(record) = terms.iter().find(|record| record.term == term) else {
        return Err(CoreError::TermStartUnknown(format!(
            "本地没有「{term}」这条学期记录，请先抓取课表并确认开学日期"
        )));
    };
    if record.starts_on.trim().is_empty() {
        return Err(CoreError::TermStartUnknown(format!(
            "「{term}」的开学日期还没确认（第 1 周周一）：先确认再导出，否则整张课表的日期都会错"
        )));
    }
    parse_term_start(&record.starts_on).ok_or_else(|| {
        CoreError::TermStartUnknown(format!(
            "「{term}」的开学日期「{}」无法识别，请重新确认",
            record.starts_on.trim()
        ))
    })
}

/// 写文件：UTF-8（无 BOM）、行尾 CRLF 已在文本里。
fn write_calendar(path: &Path, text: &str) -> CoreResult<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .map_err(|err| CoreError::Storage(format!("建导出目录失败：{err}")))?;
        }
    }
    std::fs::write(path, text.as_bytes())
        .map_err(|err| CoreError::Storage(format!("写日历文件失败：{err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CourseStore;
    use time::{Date, Month};

    /// 2026-09-07 是周一。
    const TERM_START: &str = "2026-09-07";
    const TERM: &str = "2026-2027-1";

    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("yunong-ics-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("建临时目录");
            Self { dir }
        }

        fn db(&self) -> PathBuf {
            self.dir.join("timetable.db")
        }

        fn ics(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }

        fn store(&self) -> CourseStore {
            CourseStore::open(self.db()).expect("开库")
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn course(name: &str, weekday: u8, start: u8, end: u8, weeks: &str) -> Course {
        Course {
            name: name.into(),
            weekday,
            start_period: start,
            end_period: end,
            weeks: weeks.into(),
            room: Some("尚农楼1-18".into()),
            teacher: Some("张老师".into()),
        }
    }

    /// 建一个"学期 + 课程"都齐的库。
    fn seeded(sandbox: &Sandbox, term_start: Option<&str>, courses: &[Course]) {
        let mut store = sandbox.store();
        store.upsert_term(TERM, term_start, None).expect("写学期");
        store.replace_term_courses(TERM, courses).expect("写课程");
    }

    fn read(path: &Path) -> String {
        String::from_utf8(std::fs::read(path).expect("读文件")).expect("必须是 UTF-8")
    }

    fn uids(text: &str) -> Vec<String> {
        text.split("\r\n")
            .filter_map(|line| line.strip_prefix("UID:"))
            .map(str::to_owned)
            .collect()
    }

    fn assert_ics_shape(text: &str) {
        for line in text.split("\r\n") {
            assert!(line.len() <= 75, "每行 ≤75 字节：{line}");
        }
        assert!(text.ends_with("\r\n"), "文件必须以 CRLF 结尾");
        let mut rest = text;
        while let Some(pos) = rest.find('\n') {
            assert!(rest[..pos].ends_with('\r'), "不许有裸 LF");
            rest = &rest[pos + 1..];
        }
    }

    // ------------------------------------------------------------------
    // 正常路径
    // ------------------------------------------------------------------

    #[test]
    fn export_expands_each_week_into_its_own_event() {
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[course("高等数学I", 1, 1, 2, "1-16")],
        );
        let out = sandbox.ics("term.ics");
        let report = export_term_ics(TERM, sandbox.db(), &out).expect("导出必须成功");

        assert_eq!(report.events, 16, "1-16 周 → 16 个独立事件");
        assert_eq!(report.path, out);
        let text = read(&out);
        assert_eq!(text.matches("BEGIN:VEVENT\r\n").count(), 16);
        // 第 1 周周一 = 开学日；第 16 周周一 = +15 周
        assert!(text.contains("DTSTART;VALUE=DATE:20260907"), "{text}");
        assert!(text.contains("DTSTART;VALUE=DATE:20261221"), "{text}");
        assert!(
            text.contains("SUMMARY:高等数学I（1-2节）@尚农楼1-18"),
            "{text}"
        );
        assert!(
            text.contains("DESCRIPTION:教师：张老师；周次：1-16"),
            "{text}"
        );
        assert_ics_shape(&text);
    }

    #[test]
    fn broken_week_ranges_export_fifteen_events() {
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[course("大学物理", 3, 3, 4, "1-8,10-16")],
        );
        let out = sandbox.ics("term.ics");
        let report = export_term_ics(TERM, sandbox.db(), &out).expect("导出必须成功");
        assert_eq!(report.events, 15);
        let text = read(&out);
        // 第 9 周周三 = 2026-11-04 必须缺席；第 10 周周三 = 2026-11-11 在场
        assert!(!text.contains("20261104"), "第 9 周不该有事件：{text}");
        assert!(text.contains("DTSTART;VALUE=DATE:20261111"), "{text}");
        assert_ics_shape(&text);
    }

    #[test]
    fn uids_are_identical_when_exporting_twice() {
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[
                course("高等数学I", 1, 1, 2, "1-16"),
                course("大学英语", 4, 5, 6, "1-8,10-16"),
            ],
        );
        let first = sandbox.ics("a.ics");
        let second = sandbox.ics("b.ics");
        export_term_ics(TERM, sandbox.db(), &first).expect("第一次导出");
        export_term_ics(TERM, sandbox.db(), &second).expect("第二次导出");

        assert_eq!(uids(&read(&first)), uids(&read(&second)), "UID 必须稳定");
        // 只有 DTSTAMP 可以不同
        let strip = |text: &str| -> Vec<String> {
            text.split("\r\n")
                .filter(|line| !line.starts_with("DTSTAMP:"))
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(strip(&read(&first)), strip(&read(&second)));
    }

    #[test]
    fn reimport_does_not_change_uids() {
        // 重抓课表会先删后插（courses.id 全变），UID 仍必须一致
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[course("高等数学I", 1, 1, 2, "1-16")],
        );
        let first = sandbox.ics("a.ics");
        export_term_ics(TERM, sandbox.db(), &first).expect("第一次导出");

        seeded(
            &sandbox,
            Some(TERM_START),
            &[course("高等数学I", 1, 1, 2, "1-16")],
        );
        let second = sandbox.ics("b.ics");
        export_term_ics(TERM, sandbox.db(), &second).expect("第二次导出");

        assert_eq!(
            uids(&read(&first)),
            uids(&read(&second)),
            "重抓课表不能让日历里多出一整套重复事件"
        );
    }

    #[test]
    fn empty_term_exports_an_empty_calendar_with_zero_events() {
        let sandbox = Sandbox::new();
        seeded(&sandbox, Some(TERM_START), &[]);
        let out = sandbox.ics("empty.ics");
        let report = export_term_ics(TERM, sandbox.db(), &out).expect("没课也要能导出");
        assert_eq!(report.events, 0);
        let text = read(&out);
        assert!(text.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(text.contains("END:VCALENDAR\r\n"));
        assert!(!text.contains("BEGIN:VEVENT"), "空学期不该有事件：{text}");
    }

    #[test]
    fn garbage_weeks_and_weekdays_are_skipped_not_fatal() {
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[
                course("正常课", 2, 1, 2, "1-2"),
                course("周次乱写", 3, 3, 4, "待定"),
                course("星期越界", 9, 5, 6, "1-2"),
            ],
        );
        let out = sandbox.ics("dirty.ics");
        let report = export_term_ics(TERM, sandbox.db(), &out).expect("脏数据不该让整学期失败");
        assert_eq!(report.events, 2, "只有正常课的两周能出事件");
        let text = read(&out);
        assert!(text.contains("SUMMARY:正常课（1-2节）"), "{text}");
        assert!(!text.contains("周次乱写"), "{text}");
        assert!(!text.contains("星期越界"), "{text}");
    }

    #[test]
    fn output_directory_is_created_on_demand() {
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[course("高等数学I", 1, 1, 2, "1")],
        );
        let out = sandbox.dir.join("nested").join("deeper").join("term.ics");
        let report = export_term_ics(TERM, sandbox.db(), &out).expect("应自动建目录");
        assert_eq!(report.events, 1);
        assert!(out.exists());
    }

    // ------------------------------------------------------------------
    // 边界：开学日未知
    // ------------------------------------------------------------------

    #[test]
    fn unknown_term_start_fails_and_creates_no_file() {
        let sandbox = Sandbox::new();
        seeded(&sandbox, None, &[course("高等数学I", 1, 1, 2, "1-16")]);
        let out = sandbox.ics("should-not-exist.ics");
        let err = export_term_ics(TERM, sandbox.db(), &out).expect_err("没有开学日必须报错");
        assert!(
            matches!(err, CoreError::TermStartUnknown(_)),
            "错误类型必须是 TermStartUnknown，实际：{err:?}"
        );
        assert!(
            !out.exists(),
            "开学日未知时**绝不能**生成文件（更不能拿默认日期凑一份）"
        );
    }

    #[test]
    fn term_without_a_row_fails_and_creates_no_file() {
        let sandbox = Sandbox::new();
        // 库存在，但这个学期从没抓过
        let store = sandbox.store();
        drop(store);
        let out = sandbox.ics("nope.ics");
        let err = export_term_ics("2099-2100-1", sandbox.db(), &out).expect_err("未知学期必须报错");
        assert!(matches!(err, CoreError::TermStartUnknown(_)), "{err:?}");
        assert!(!out.exists());
    }

    #[test]
    fn missing_database_is_reported_as_unknown_start_and_not_created() {
        let sandbox = Sandbox::new();
        let db = sandbox.db();
        let out = sandbox.ics("nope.ics");
        let err = export_term_ics(TERM, &db, &out).expect_err("没库必须报错");
        assert!(matches!(err, CoreError::TermStartUnknown(_)), "{err:?}");
        assert!(!out.exists());
        assert!(
            !db.exists(),
            "不该顺手建一个空库（会让'没抓过'与'抓过但没课'混淆）"
        );
    }

    #[test]
    fn corrupt_start_date_is_reported_as_unknown_start() {
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[course("高等数学I", 1, 1, 2, "1-16")],
        );
        // 直接把库里的 starts_on 改成非法值（模拟旧版本写入的脏数据）
        {
            let mut store = sandbox.store();
            store
                .upsert_term(TERM, Some("2026-02-30"), None)
                .expect("写脏日期");
        }
        let out = sandbox.ics("nope.ics");
        let err = export_term_ics(TERM, sandbox.db(), &out).expect_err("脏日期必须报错");
        assert!(matches!(err, CoreError::TermStartUnknown(_)), "{err:?}");
        assert!(!out.exists());
    }

    #[test]
    fn empty_arguments_are_rejected_before_touching_disk() {
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[course("高等数学I", 1, 1, 2, "1")],
        );
        let out = sandbox.ics("x.ics");
        let err = export_term_ics("   ", sandbox.db(), &out).expect_err("空学期");
        assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");

        let err = export_term_ics(TERM, sandbox.db(), "").expect_err("空路径");
        assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
        assert!(!out.exists());
    }

    // ------------------------------------------------------------------
    // 日期换算
    // ------------------------------------------------------------------

    #[test]
    fn dates_follow_weekday_and_week_number() {
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[
                course("周一课", 1, 1, 2, "1"),
                course("周五课", 5, 3, 4, "1"),
                course("周日课", 7, 5, 6, "2"),
            ],
        );
        let out = sandbox.ics("dates.ics");
        let report = export_term_ics(TERM, sandbox.db(), &out).expect("导出");
        assert_eq!(report.events, 3);
        let text = read(&out);
        assert!(
            text.contains("DTSTART;VALUE=DATE:20260907"),
            "周一第 1 周：{text}"
        );
        assert!(
            text.contains("DTSTART;VALUE=DATE:20260911"),
            "周五第 1 周：{text}"
        );
        // 第 2 周周日 = 开学 + 7 天 + 6 天
        assert!(
            text.contains("DTSTART;VALUE=DATE:20260920"),
            "周日第 2 周：{text}"
        );
    }

    #[test]
    fn sunday_of_week_sixteen_stays_inside_the_term() {
        // 边界：第 16 周的周日 = 开学 + 15*7 + 6 天，不能溢出到下个学期
        let sandbox = Sandbox::new();
        seeded(
            &sandbox,
            Some(TERM_START),
            &[course("晚课", 7, 9, 10, "16")],
        );
        let out = sandbox.ics("sunday.ics");
        export_term_ics(TERM, sandbox.db(), &out).expect("导出");
        let text = read(&out);
        let expected = Date::from_calendar_date(2026, Month::December, 27).unwrap();
        assert!(
            text.contains(&format!(
                "DTSTART;VALUE=DATE:{:04}{:02}{:02}",
                expected.year(),
                expected.month() as u8,
                expected.day()
            )),
            "{text}"
        );
    }
}
