//! 课表 → 日历：周次展开 + ICS 导出（FR-F1 / FR-F3）。
//!
//! 两条来自旧项目的教训（见 `docs/migration/v0-reuse.md`）：
//! 1. **先识别"单/双"再剥离括号**，否则奇偶周信息丢失，单周课会被当成每周都上；
//! 2. 解析失败必须**显式报错**，绝不能静默产出一张空课表。
//!
//! # 为什么导出的是"全天事件"，不是带时刻的事件
//!
//! 教务接口只给节号（例如 `1-2节`），**没有作息表**。自己编一个 `08:00-09:35`
//! 出来，一旦学校换了作息，用户日历里每个事件都是错的，而且错得无法分辨——
//! 用户没法知道是"学校改了作息"还是"App 猜错了"。猜错比不写更糟，所以这里
//! **只写日期**（`DTSTART;VALUE=DATE`），节次进摘要：`高等数学I（1-2节）@尚农楼1-18`。
//! 等接入真实作息表配置之后，时刻才是**事实**，那时再加定时事件。
//!
//! # 为什么不用 RRULE
//!
//! 周次经常断档（`1-8,10-16`）与单双周（`1-16单`）。RRULE 表达断档要拆多条规则，
//! 单双周要 `INTERVAL=2` 且起点对齐，各家导入器（Outlook / Google / 系统日历）
//! 的容错差异很大；展开成独立 `VEVENT` 文件虽然大一些，但**任何**客户端都能正确
//! 显示，用户也能单独删掉某一周。因此：**一门课一门周 = 一个独立事件**。
//!
//! # UID 为什么用内容哈希，而不是 `courses.id`
//!
//! `courses.id` 是 `AUTOINCREMENT`，每次重抓课表（`replace_term_courses` 先删后插）
//! 都会变；拿它做 UID，重抓一次日历里就会多出一整套重复事件（日历靠 UID 去重）。
//! 所以 UID 用**内容**做种子：学期 / 课名 / 星期 / 起止节 / 教室 / 第几周，
//! 与 `docs/02-系统设计文档.md` §6.6 的 `sha1(term / name / weekday / start_period / room)`
//! 一致。重复导入同一个 `.ics`、重抓课表，都不会产生重复条目。

use sha2::{Digest, Sha256};
use time::macros::format_description;
use time::{Date, Duration, OffsetDateTime};

/// 一门课（来自课表解析结果，也可由 `store::StoredCourse` 转换而来）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Course {
    pub name: String,
    /// 1 = 周一 … 7 = 周日
    pub weekday: u8,
    pub start_period: u8,
    pub end_period: u8,
    /// 原始周次写法，如 `1-16单`
    pub weeks: String,
    pub room: Option<String>,
    pub teacher: Option<String>,
}

/// 展开后的一次上课：**全天**事件（没有具体时刻，见模块文档）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcsEvent {
    /// 稳定 UID（同一课同一周同一个起节 → 同一个 UID）
    pub uid: String,
    pub date: Date,
    /// 摘要：课程名 + 节次 + 教室
    pub summary: String,
    /// 教室（没有就省略 `LOCATION` 行）
    pub location: Option<String>,
    /// 描述：教师 + 周次原文（空串则省略 `DESCRIPTION` 行）
    pub description: String,
}

/// 把教务的周次写法展开成周次列表。
///
/// 支持：`1-16`、`1-5,7-9`、`1-16单`、`2-16双`、`4-15(周)`、`1，3，5`
///
/// 返回升序去重的周次；无法解析的片段被忽略（调用方应校验结果非空）。
pub fn parse_weeks(spec: &str) -> Vec<u16> {
    let text = spec
        .replace('（', "(")
        .replace('）', ")")
        .replace(['周', '第'], "");

    // 关键：先判断奇偶，再剥离标记
    let odd_only = text.contains('单');
    let even_only = text.contains('双');
    let cleaned: String = text
        .chars()
        .filter(|c| !matches!(c, '单' | '双' | '(' | ')' | ' '))
        .collect();

    let mut weeks: Vec<u16> = Vec::new();
    for token in cleaned.split([',', '，', '、', ';', '；']).map(str::trim) {
        if token.is_empty() {
            continue;
        }
        let (start, end) = match token.split_once(['-', '~', '—', '至']) {
            Some((a, b)) => match (a.trim().parse::<u16>(), b.trim().parse::<u16>()) {
                (Ok(a), Ok(b)) => {
                    if a <= b {
                        (a, b)
                    } else {
                        (b, a)
                    }
                }
                _ => continue,
            },
            None => match token.parse::<u16>() {
                Ok(v) => (v, v),
                Err(_) => continue,
            },
        };
        for w in start..=end {
            if w == 0 {
                continue;
            }
            if odd_only && w % 2 == 0 {
                continue;
            }
            if even_only && w % 2 != 0 {
                continue;
            }
            weeks.push(w);
        }
    }
    weeks.sort_unstable();
    weeks.dedup();
    weeks
}

/// 解析 `terms.starts_on`（`YYYY-MM-DD`）。
///
/// 空串 / 非法 / 不存在的日期一律返回 `None`：**绝不回退到默认日期**——
/// 猜错开学日，整张课表的日期就全错，而用户很难发现是差了一周。
pub fn parse_term_start(starts_on: &str) -> Option<Date> {
    let text = starts_on.trim();
    if text.is_empty() {
        return None;
    }
    Date::parse(text, format_description!("[year]-[month]-[day]")).ok()
}

/// 把一门课按 `term_start`（第 1 周**周一**）展开成每周一个全天事件。
///
/// 星期或周次写坏的行**跳过**（不 panic、不猜），调用方看到的只是事件数变少。
pub fn expand_course(term: &str, term_start: Date, course: &Course) -> Vec<IcsEvent> {
    if !(1..=7).contains(&course.weekday) {
        return Vec::new();
    }
    let summary = summary_for(course);
    let description = description_for(course);
    // UID 种子：内容 + 第几周（不含日期——用户修正开学日时，事件是被"改期"而不是被复制）
    let seed = format!(
        "{}|{}|{}|{}|{}|{}",
        term.trim(),
        course.name.trim(),
        course.weekday,
        course.start_period,
        course.end_period,
        course.room.as_deref().unwrap_or("").trim()
    );

    parse_weeks(&course.weeks)
        .into_iter()
        .filter_map(|week| {
            let offset = (week as i64 - 1) * 7 + (course.weekday as i64 - 1);
            let date = term_start.checked_add(Duration::days(offset))?;
            Some(IcsEvent {
                uid: format!("{}@yunong", short_hash(&format!("{seed}|w{week}"))),
                date,
                summary: summary.clone(),
                location: course.room.clone(),
                description: description.clone(),
            })
        })
        .collect()
}

/// 渲染成完整 `.ics` 文本：UTF-8 + CRLF 行尾，每行 ≤75 字节（续行以一个空格开头）。
///
/// `dtstamp` 形如 `20261031T095240Z`（UTC）；测试里传固定值以便逐字节比对。
pub fn render_ics(term: &str, events: &[IcsEvent], dtstamp: &str) -> String {
    let mut out = String::with_capacity(256 + events.len() * 320);
    push_line(&mut out, "BEGIN:VCALENDAR");
    push_line(&mut out, "VERSION:2.0");
    push_line(&mut out, "PRODID:-//yunong//timetable//ZH");
    push_line(&mut out, "CALSCALE:GREGORIAN");
    push_line(&mut out, "METHOD:PUBLISH");
    if !term.trim().is_empty() {
        // 导入器用它给日历命名（Google / Apple 都认这个扩展属性）
        push_line(
            &mut out,
            &format!("X-WR-CALNAME:{}", escape_text(term.trim())),
        );
    }

    for event in events {
        push_line(&mut out, "BEGIN:VEVENT");
        push_line(&mut out, &format!("UID:{}", event.uid));
        push_line(&mut out, &format!("DTSTAMP:{dtstamp}"));
        push_line(&mut out, &format!("DTSTART;VALUE=DATE:{}", ymd(event.date)));
        // 全天事件的 DTEND 是**排他**的：结束日 = 当天 + 1 天
        if let Some(next) = event.date.checked_add(Duration::days(1)) {
            push_line(&mut out, &format!("DTEND;VALUE=DATE:{}", ymd(next)));
        }
        push_line(
            &mut out,
            &format!("SUMMARY:{}", escape_text(&event.summary)),
        );
        if let Some(room) = event
            .location
            .as_deref()
            .map(str::trim)
            .filter(|room| !room.is_empty())
        {
            push_line(&mut out, &format!("LOCATION:{}", escape_text(room)));
        }
        if !event.description.trim().is_empty() {
            push_line(
                &mut out,
                &format!("DESCRIPTION:{}", escape_text(&event.description)),
            );
        }
        push_line(&mut out, "END:VEVENT");
    }

    push_line(&mut out, "END:VCALENDAR");
    out
}

/// 当前 UTC 时间的 `DTSTAMP`（`YYYYMMDDTHHMMSSZ`）。
pub fn utc_now_stamp() -> String {
    utc_stamp(OffsetDateTime::now_utc())
}

fn utc_stamp(time: OffsetDateTime) -> String {
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        time.year(),
        time.month() as u8,
        time.day(),
        time.hour(),
        time.minute(),
        time.second()
    )
}

/// `YYYYMMDD`（`DTSTART;VALUE=DATE` 的日期部分）。
fn ymd(date: Date) -> String {
    format!(
        "{:04}{:02}{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

/// 摘要：课程名 +（节次）+ @教室，例如 `高等数学I（1-2节）@尚农楼1-18`。
fn summary_for(course: &Course) -> String {
    let periods = period_label(course.start_period, course.end_period);
    match course
        .room
        .as_deref()
        .map(str::trim)
        .filter(|room| !room.is_empty())
    {
        Some(room) => format!("{}（{}）@{}", course.name.trim(), periods, room),
        None => format!("{}（{}）", course.name.trim(), periods),
    }
}

/// `1-2节` / `3节`（节次写反了也不输出 `5-3节`）。
fn period_label(start: u8, end: u8) -> String {
    if end > start {
        format!("{start}-{end}节")
    } else {
        format!("{start}节")
    }
}

/// 描述：教师 + 周次原文（教务给的原文，不做二次解释）。
fn description_for(course: &Course) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(teacher) = course
        .teacher
        .as_deref()
        .map(str::trim)
        .filter(|teacher| !teacher.is_empty())
    {
        parts.push(format!("教师：{teacher}"));
    }
    let weeks = course.weeks.trim();
    if !weeks.is_empty() {
        parts.push(format!("周次：{weeks}"));
    }
    parts.join("；")
}

fn short_hash(seed: &str) -> String {
    let digest = Sha256::digest(seed.as_bytes());
    let hex = format!("{digest:x}");
    hex[..16].to_owned()
}

/// ICS 文本转义（RFC 5545 §3.3.11 TEXT）：`\` `;` `,` 与换行。
///
/// 顺序很重要：反斜杠必须**先**转，否则后面补出来的反斜杠会被二次转义。
/// 原生的 `\r\n` / `\r` 也要压成 `\n`——属性值里出现裸换行会让整行被截断，
/// 这是导入失败最常见的原因之一。
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                // CRLF 与裸 CR 都归一成一个 \n
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// 折行（RFC 5545 §3.1）：内容行每行不超过 **75 个八位组**，续行以单个空格开头。
///
/// 按**字符**边界切，不能按字节硬切——把多字节 UTF-8 字符劈成两半会让客户端直接拒绝。
/// 续行的行首空格也算在这 75 字节里，所以续行内容最多 74 字节。
fn fold_line(line: &str) -> String {
    const LIMIT: usize = 75;
    let mut out = String::with_capacity(line.len() + 8);
    let mut used = 0usize;
    let mut first = true;
    for ch in line.chars() {
        let width = ch.len_utf8();
        // 续行要先占掉一个空格
        let room = if first { LIMIT } else { LIMIT - 1 };
        if used + width > room {
            out.push_str("\r\n ");
            used = 0;
            first = false;
        }
        out.push(ch);
        used += width;
    }
    out
}

/// 折行 + 行尾 CRLF（RFC 5545 硬性要求：LF-only 会让部分客户端整份乱码）。
fn push_line(out: &mut String, line: &str) {
    out.push_str(&fold_line(line));
    out.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    /// 2026-09-07 是周一，正好当"第 1 周周一"。
    fn term_start() -> Date {
        Date::from_calendar_date(2026, Month::September, 7).unwrap()
    }

    fn date(y: i32, m: Month, d: u8) -> Date {
        Date::from_calendar_date(y, m, d).unwrap()
    }

    fn course(weeks: &str) -> Course {
        Course {
            name: "高等数学A".into(),
            weekday: 3, // 周三
            start_period: 6,
            end_period: 7,
            weeks: weeks.into(),
            room: Some("东校区1教305".into()),
            teacher: Some("张老师".into()),
        }
    }

    fn expand(weeks: &str) -> Vec<IcsEvent> {
        expand_course("2026-2027-1", term_start(), &course(weeks))
    }

    /// 逻辑行 = 把折行还原（CRLF + 空格 → 空串）。
    fn unfold(ics: &str) -> Vec<String> {
        let mut logical: Vec<String> = Vec::new();
        for physical in ics.split("\r\n") {
            if physical.is_empty() {
                continue;
            }
            if let Some(rest) = physical.strip_prefix(' ') {
                logical
                    .last_mut()
                    .expect("续行必须跟在某一行之后")
                    .push_str(rest);
            } else {
                logical.push(physical.to_owned());
            }
        }
        logical
    }

    fn uids(ics: &str) -> Vec<String> {
        ics.lines()
            .filter_map(|line| line.strip_prefix("UID:"))
            .map(str::to_owned)
            .collect()
    }

    // ------------------------------------------------------------------
    // 周次展开
    // ------------------------------------------------------------------

    #[test]
    fn parses_week_ranges() {
        assert_eq!(parse_weeks("1-16").len(), 16);
        assert_eq!(
            parse_weeks("4-15(周)"),
            vec![4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
        assert_eq!(parse_weeks("1-5,7-9").len(), 8);
        assert_eq!(parse_weeks("1-16单"), vec![1, 3, 5, 7, 9, 11, 13, 15]);
        assert_eq!(parse_weeks("2-16双"), vec![2, 4, 6, 8, 10, 12, 14, 16]);
        assert_eq!(parse_weeks("1，3，5"), vec![1, 3, 5]);
        assert_eq!(parse_weeks("乱七八糟"), Vec::<u16>::new());
    }

    #[test]
    fn continuous_weeks_expand_to_one_event_per_week() {
        let events = expand("1-16");
        assert_eq!(
            events.len(),
            16,
            "1-16 必须展开成 16 个独立事件（不是 RRULE）"
        );
        // 第 1 周周三 = 2026-09-09
        assert_eq!(events[0].date, date(2026, Month::September, 9));
        // 每周相隔 7 天
        assert_eq!(events[1].date, date(2026, Month::September, 16));
        assert_eq!(events[15].date, date(2026, Month::December, 23));
    }

    #[test]
    fn broken_week_ranges_skip_the_gap_week() {
        let events = expand("1-8,10-16");
        assert_eq!(events.len(), 15, "1-8 加 10-16 = 15 周，第 9 周必须缺席");
        let dates: Vec<Date> = events.iter().map(|e| e.date).collect();
        // 第 9 周周三 = 2026-09-09 + 8 周
        assert!(
            !dates.contains(&date(2026, Month::November, 4)),
            "第 9 周不该有事件：{dates:?}"
        );
        // 第 10 周周三照常在
        assert!(dates.contains(&date(2026, Month::November, 11)));
    }

    #[test]
    fn single_week_expands_to_exactly_one_event() {
        let events = expand("5");
        assert_eq!(events.len(), 1);
        // 第 5 周周三 = 第 1 周周三 + 28 天
        assert_eq!(events[0].date, date(2026, Month::October, 7));
    }

    #[test]
    fn garbage_weeks_expand_to_nothing_without_panicking() {
        for spec in ["乱写", "", "0", "-", "1-", "abc,def"] {
            assert!(expand(spec).is_empty(), "{spec} 应展开成 0 个事件");
        }
    }

    #[test]
    fn invalid_weekday_is_skipped() {
        for weekday in [0u8, 8, 255] {
            let mut bad = course("1-16");
            bad.weekday = weekday;
            assert!(expand_course("t", term_start(), &bad).is_empty());
        }
    }

    #[test]
    fn week_one_lands_on_the_given_monday() {
        let mut monday = course("1");
        monday.weekday = 1;
        let events = expand_course("t", term_start(), &monday);
        assert_eq!(events[0].date, term_start(), "第 1 周周一就是开学当天");

        let mut sunday = course("1");
        sunday.weekday = 7;
        let events = expand_course("t", term_start(), &sunday);
        assert_eq!(events[0].date, date(2026, Month::September, 13));
    }

    // ------------------------------------------------------------------
    // 全天事件 / 字段
    // ------------------------------------------------------------------

    #[test]
    fn events_are_all_day_and_never_guess_a_time_of_day() {
        let ics = render_ics("2026-2027-1", &expand("1"), "20261031T095240Z");
        assert!(ics.contains("DTSTART;VALUE=DATE:20260909"), "{ics}");
        assert!(
            ics.contains("DTEND;VALUE=DATE:20260910"),
            "全天事件 DTEND 排他：{ics}"
        );
        // 绝不能出现 TZID / 具体时刻 / RRULE
        assert!(!ics.contains("TZID"), "不该带时区时刻：{ics}");
        assert!(!ics.contains("RRULE"), "不用 RRULE：{ics}");
        assert!(
            !ics.contains("DTSTART:2026"),
            "DTSTART 必须是 VALUE=DATE：{ics}"
        );
    }

    #[test]
    fn summary_carries_periods_and_room() {
        let ics = render_ics("2026-2027-1", &expand("1"), "20261031T095240Z");
        assert!(
            ics.contains("SUMMARY:高等数学A（6-7节）@东校区1教305"),
            "{ics}"
        );
        assert!(ics.contains("LOCATION:东校区1教305"), "{ics}");
        assert!(ics.contains("DESCRIPTION:教师：张老师；周次：1"), "{ics}");
    }

    #[test]
    fn single_period_course_uses_a_compact_label() {
        let mut one = course("1");
        one.start_period = 3;
        one.end_period = 3;
        let ics = render_ics(
            "t",
            &expand_course("t", term_start(), &one),
            "20261031T095240Z",
        );
        assert!(
            ics.contains("SUMMARY:高等数学A（3节）@东校区1教305"),
            "{ics}"
        );
    }

    #[test]
    fn missing_room_and_teacher_omit_the_property_lines() {
        let mut bare = course("1");
        bare.room = None;
        bare.teacher = None;
        bare.weeks = "1".into();
        let ics = render_ics(
            "t",
            &expand_course("t", term_start(), &bare),
            "20261031T095240Z",
        );
        assert!(ics.contains("SUMMARY:高等数学A（6-7节）"), "{ics}");
        assert!(!ics.contains("LOCATION:"), "{ics}");
        assert!(ics.contains("DESCRIPTION:周次：1"), "{ics}");
    }

    // ------------------------------------------------------------------
    // 转义
    // ------------------------------------------------------------------

    #[test]
    fn escapes_comma_semicolon_and_backslash_in_text_fields() {
        let mut tricky = course("1");
        tricky.name = r"高数,一;二\三".into();
        tricky.room = Some(r"1教,305;东".into());
        tricky.teacher = Some(r"张\老师,兼;辅导员".into());
        let ics = render_ics(
            "t",
            &expand_course("t", term_start(), &tricky),
            "20261031T095240Z",
        );
        let logical = unfold(&ics);
        let summary = logical
            .iter()
            .find(|line| line.starts_with("SUMMARY:"))
            .expect("必须有 SUMMARY");
        assert_eq!(summary, r"SUMMARY:高数\,一\;二\\三（6-7节）@1教\,305\;东");
        let location = logical
            .iter()
            .find(|line| line.starts_with("LOCATION:"))
            .expect("必须有 LOCATION");
        assert_eq!(location, r"LOCATION:1教\,305\;东");
        let description = logical
            .iter()
            .find(|line| line.starts_with("DESCRIPTION:"))
            .expect("必须有 DESCRIPTION");
        assert_eq!(
            description,
            r"DESCRIPTION:教师：张\\老师\,兼\;辅导员；周次：1"
        );
    }

    #[test]
    fn newlines_inside_text_become_escaped_backslash_n() {
        let mut multiline = course("1");
        multiline.name = "高数\r\n上;下".into();
        let ics = render_ics(
            "t",
            &expand_course("t", term_start(), &multiline),
            "20261031T095240Z",
        );
        let logical = unfold(&ics);
        let summary = logical
            .iter()
            .find(|line| line.starts_with("SUMMARY:"))
            .expect("必须有 SUMMARY");
        assert_eq!(summary, r"SUMMARY:高数\n上\;下（6-7节）@东校区1教305");
        assert!(!ics.contains('\r') || ics.split("\r\n").all(|line| !line.contains('\r')));
    }

    // ------------------------------------------------------------------
    // 折行 / 行尾
    // ------------------------------------------------------------------

    #[test]
    fn every_line_is_at_most_75_octets_and_continuations_start_with_a_space() {
        let mut long = course("1-3");
        // 一个足够长的课程名，前面再加上节次与教室，必然要折行
        long.name = "高等数学（理科实验班·双语教学）与数学建模基础理论".repeat(3);
        long.room = Some("尚农楼1-18（东校区新教学楼）".repeat(2));
        let ics = render_ics(
            "2026-2027-1",
            &expand_course("t", term_start(), &long),
            "20261031T095240Z",
        );

        for line in ics.split("\r\n") {
            assert!(
                line.len() <= 75,
                "每行不得超过 75 字节（实际 {}）：{line}",
                line.len()
            );
        }
        // 折行必须产生续行（以空格开头），且还原后仍是一整行
        let continuations: Vec<&str> = ics
            .split("\r\n")
            .filter(|line| line.starts_with(' '))
            .collect();
        assert!(
            !continuations.is_empty(),
            "长 SUMMARY 必须折行，否则会被客户端截断：{ics}"
        );
        let logical = unfold(&ics);
        let summary = logical
            .iter()
            .find(|line| line.starts_with("SUMMARY:"))
            .expect("必须有 SUMMARY");
        assert!(summary.contains(&long.name), "折行不能丢内容：{summary}");
        // 还原后每行内容照旧（逻辑行可以超过 75 字节）
        assert!(summary.len() > 75);
    }

    #[test]
    fn folding_never_splits_a_multibyte_char() {
        // 4 字节一个字符（U+1F600 等）时，按字节硬切会切出非法 UTF-8
        let mut emoji = course("1");
        emoji.name = "📚".repeat(40);
        let ics = render_ics(
            "t",
            &expand_course("t", term_start(), &emoji),
            "20261031T095240Z",
        );
        for line in ics.split("\r\n") {
            assert!(line.len() <= 75);
            // 每行都必须是合法 UTF-8（split 已保证是 &str，这里再验一次字节边界）
            assert!(std::str::from_utf8(line.as_bytes()).is_ok());
        }
        assert!(unfold(&ics).iter().any(|line| line.contains(&emoji.name)));
    }

    #[test]
    fn lines_end_with_crlf_and_never_bare_lf() {
        let ics = render_ics("2026-2027-1", &expand("1-2"), "20261031T095240Z");
        assert!(ics.ends_with("\r\n"), "文件必须以 CRLF 结尾");
        let mut rest = ics.as_str();
        while let Some(pos) = rest.find('\n') {
            let before = &rest[..pos];
            assert!(before.ends_with('\r'), "出现裸 LF：{:?}", &rest[..pos + 1]);
            rest = &rest[pos + 1..];
        }
        // 行首是 CRLF 结构，不能有孤立的 \r
        assert_eq!(ics.matches('\r').count(), ics.matches("\r\n").count());
    }

    #[test]
    fn calendar_is_valid_utf8_with_begin_and_end_markers() {
        let ics = render_ics("2026-2027-1", &expand("1"), "20261031T095240Z");
        assert!(std::str::from_utf8(ics.as_bytes()).is_ok());
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.trim_end().ends_with("END:VCALENDAR"));
        assert_eq!(ics.matches("BEGIN:VEVENT\r\n").count(), 1);
        assert_eq!(ics.matches("END:VEVENT\r\n").count(), 1);
    }

    // ------------------------------------------------------------------
    // UID 稳定性
    // ------------------------------------------------------------------

    #[test]
    fn uid_is_stable_across_renders_with_different_dtstamp() {
        let events = expand("1-3");
        let first = render_ics("2026-2027-1", &events, "20261031T095240Z");
        let second = render_ics("2026-2027-1", &events, "20270101T000000Z");
        assert_eq!(uids(&first), uids(&second), "UID 不能随 DTSTAMP 变化");
        // 同一输入渲染两次，除 DTSTAMP 外完全一致
        let strip = |text: &str| -> Vec<String> {
            unfold(text)
                .into_iter()
                .filter(|line| !line.starts_with("DTSTAMP:"))
                .collect()
        };
        assert_eq!(strip(&first), strip(&second));
    }

    #[test]
    fn uid_does_not_depend_on_insertion_order_or_row_id() {
        // 同一门课换个顺序（重抓课表后 id 全变）→ UID 不变
        let events_a = expand_course("2026-2027-1", term_start(), &course("1-2"));
        let mut renamed_id = course("1-2");
        renamed_id.name = "高等数学A".into(); // 内容相同，模拟 id 变化
        let events_b = expand_course("2026-2027-1", term_start(), &renamed_id);
        let a: Vec<&str> = events_a.iter().map(|e| e.uid.as_str()).collect();
        let b: Vec<&str> = events_b.iter().map(|e| e.uid.as_str()).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn uid_differs_per_week() {
        let events = expand("1-16");
        let ids: std::collections::HashSet<&str> =
            events.iter().map(|event| event.uid.as_str()).collect();
        assert_eq!(ids.len(), 16, "16 周必须有 16 个不同 UID");
    }

    #[test]
    fn uid_differs_per_course() {
        let math = expand_course("2026-2027-1", term_start(), &course("1"));
        let mut other = course("1");
        other.start_period = 8;
        let physics = expand_course("2026-2027-1", term_start(), &other);
        assert_ne!(math[0].uid, physics[0].uid);
        // 不同学期同理
        let next_term = expand_course("2026-2027-2", term_start(), &course("1"));
        assert_ne!(math[0].uid, next_term[0].uid);
    }

    // ------------------------------------------------------------------
    // 学期开始日
    // ------------------------------------------------------------------

    #[test]
    fn term_start_parsing_never_falls_back_to_a_default_date() {
        assert_eq!(
            parse_term_start("2026-09-07"),
            Some(date(2026, Month::September, 7))
        );
        for bad in [
            "",
            "   ",
            "2026-9-7",
            "2026/09/07",
            "20260907",
            "2026-02-30",
            "乱写",
        ] {
            assert_eq!(parse_term_start(bad), None, "{bad} 不能解析出日期");
        }
    }

    #[test]
    fn empty_course_list_renders_an_empty_but_valid_calendar() {
        let ics = render_ics("2026-2027-1", &[], "20261031T095240Z");
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(ics.contains("END:VCALENDAR\r\n"));
        assert!(!ics.contains("BEGIN:VEVENT"), "没有课就不该有事件");
    }
}
