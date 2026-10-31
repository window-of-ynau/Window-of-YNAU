//! 课表 → 日历：周次展开 + ICS 导出（FR-F1 / FR-F3）。
//!
//! 两条来自旧项目的教训（见 `docs/migration/v0-reuse.md`）：
//! 1. **先识别"单/双"再剥离括号**，否则奇偶周信息丢失，单周课会被当成每周都上；
//! 2. 解析失败必须**显式报错**，绝不能静默产出一张空课表。

use sha2::{Digest, Sha256};
use time::{Date, Duration, OffsetDateTime};

/// 导出时使用的时区（学校在本地理时区）。
pub const TZ: &str = "Asia/Shanghai";

/// 一节的时间（分钟偏移，从 00:00 起算）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodTime {
    pub period: u8,
    pub start_minute: u16,
    pub end_minute: u16,
}

/// 默认作息：每节 45 分钟、间隔 5 分钟，从 08:00 起。
///
/// **这是占位默认值**，真实作息由学校配置覆盖（W2 从配置读取）。
pub fn default_periods() -> Vec<PeriodTime> {
    let mut out = Vec::with_capacity(13);
    let mut start: u16 = 8 * 60;
    for period in 1..=13_u8 {
        out.push(PeriodTime {
            period,
            start_minute: start,
            end_minute: start + 45,
        });
        start += 50;
    }
    out
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

/// 一门课（来自课表解析结果）。
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

/// 展开后的一次上课。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub date: Date,
    pub start_minute: u16,
    pub end_minute: u16,
    pub name: String,
    pub room: Option<String>,
    /// 稳定 UID 的种子：重复导入不产生重复事件
    pub uid_seed: String,
}

/// 把一门课展开成具体日期。
///
/// `term_start` 必须是**第 1 周的周一**（调用方负责校验；不是周一也能跑，
/// 但日期会整体偏移）。
pub fn expand_course(term_start: Date, course: &Course, periods: &[PeriodTime]) -> Vec<Occurrence> {
    if !(1..=7).contains(&course.weekday) {
        return vec![];
    }
    let start = periods
        .iter()
        .find(|p| p.period == course.start_period)
        .map(|p| p.start_minute);
    let end = periods
        .iter()
        .find(|p| p.period == course.end_period)
        .map(|p| p.end_minute);
    let (Some(start_minute), Some(end_minute)) = (start, end) else {
        return vec![];
    };

    parse_weeks(&course.weeks)
        .into_iter()
        .filter_map(|week| {
            let offset = (week as i64 - 1) * 7 + (course.weekday as i64 - 1);
            let date = term_start.checked_add(Duration::days(offset))?;
            Some(Occurrence {
                date,
                start_minute,
                end_minute,
                name: course.name.clone(),
                room: course.room.clone(),
                uid_seed: format!(
                    "{}|{}|{}|{}|{}",
                    course.name,
                    course.weekday,
                    course.start_period,
                    course.end_period,
                    course.room.clone().unwrap_or_default()
                ),
            })
        })
        .collect()
}

/// 生成 ICS。`dtstamp` 形如 `20261031T095240Z`。
pub fn to_ics_with_stamp(occurrences: &[Occurrence], dtstamp: &str) -> String {
    let mut out = String::with_capacity(256 + occurrences.len() * 220);
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//yunong//calendar//ZH\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");
    out.push_str("METHOD:PUBLISH\r\n");
    out.push_str(&format!("X-WR-TIMEZONE:{TZ}\r\n"));

    for occ in occurrences {
        let ymd = format!(
            "{:04}{:02}{:02}",
            occ.date.year(),
            occ.date.month() as u8,
            occ.date.day()
        );
        let uid = format!("{}@yunong", uid_hash(&occ.uid_seed, &ymd));
        out.push_str("BEGIN:VEVENT\r\n");
        out.push_str(&format!("UID:{uid}\r\n"));
        out.push_str(&format!("DTSTAMP:{dtstamp}\r\n"));
        out.push_str(&format!(
            "DTSTART;TZID={TZ}:{ymd}T{:02}{:02}00\r\n",
            occ.start_minute / 60,
            occ.start_minute % 60
        ));
        out.push_str(&format!(
            "DTEND;TZID={TZ}:{ymd}T{:02}{:02}00\r\n",
            occ.end_minute / 60,
            occ.end_minute % 60
        ));
        out.push_str(&format!("SUMMARY:{}\r\n", escape_ics(&occ.name)));
        if let Some(room) = &occ.room {
            out.push_str(&format!("LOCATION:{}\r\n", escape_ics(room)));
        }
        out.push_str("END:VEVENT\r\n");
    }

    out.push_str("END:VCALENDAR\r\n");
    out
}

/// 生成 ICS，DTSTAMP 取当前时间。
pub fn to_ics(occurrences: &[Occurrence]) -> String {
    to_ics_with_stamp(occurrences, &utc_stamp(OffsetDateTime::now_utc()))
}

fn utc_stamp(t: OffsetDateTime) -> String {
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

fn uid_hash(seed: &str, ymd: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update(b"|");
    hasher.update(ymd.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    digest[..16].to_owned()
}

/// ICS 文本转义：反斜杠、分号、逗号、换行。
fn escape_ics(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    fn never() -> Date {
        Date::from_calendar_date(2026, Month::September, 7).unwrap() // 周一
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
    fn single_week_course_only_lands_on_odd_weeks() {
        let periods = default_periods();
        let occ = expand_course(never(), &course("1-16单"), &periods);
        assert_eq!(occ.len(), 8);
        // 第 1 周周三 = 2026-09-09
        assert_eq!(
            occ[0].date,
            Date::from_calendar_date(2026, Month::September, 9).unwrap()
        );
        // 第 3 周周三 = 2026-09-23
        assert_eq!(
            occ[1].date,
            Date::from_calendar_date(2026, Month::September, 23).unwrap()
        );
        assert_eq!(occ[0].start_minute, 8 * 60 + 5 * 50);
    }

    #[test]
    fn ics_is_well_formed() {
        let periods = default_periods();
        let occ = expand_course(never(), &course("1-2"), &periods);
        let ics = to_ics_with_stamp(&occ, "20261031T095240Z");
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        assert_eq!(ics.matches("BEGIN:VEVENT\r\n").count(), 2);
        assert_eq!(ics.matches("UID:").count(), 2);
        assert!(ics.contains("DTSTART;TZID=Asia/Shanghai:20260909T"));
        assert!(ics.contains("SUMMARY:高等数学A\r\n"));
        // CRLF 行尾
        assert!(!ics.contains("\n\n"));
        for line in ics.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(!line.contains('\n'));
        }
    }

    #[test]
    fn ics_escapes_special_chars() {
        assert_eq!(escape_ics("a,b;c\\d"), "a\\,b\\;c\\\\d");
        let mut c = course("1");
        c.name = "数学,分析;导论".into();
        let ics = to_ics_with_stamp(
            &expand_course(never(), &c, &default_periods()),
            "20261031T095240Z",
        );
        assert!(ics.contains("SUMMARY:数学\\,分析\\;导论\r\n"));
    }

    #[test]
    fn uid_is_stable_across_regeneration() {
        let periods = default_periods();
        let a = to_ics_with_stamp(
            &expand_course(never(), &course("1-2"), &periods),
            "20261031T095240Z",
        );
        let b = to_ics_with_stamp(
            &expand_course(never(), &course("1-2"), &periods),
            "20270101T000000Z",
        );
        let uid = |s: &str| s.lines().find(|l| l.starts_with("UID:")).map(str::to_owned);
        assert_eq!(uid(&a), uid(&b));
    }
}
