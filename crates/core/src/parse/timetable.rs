//! 课表页 HTML 的分步结构化解析（迁移规格 R1~R7）。
//!
//! **为什么不用一条正则硬啃**：字段嵌在 `<font title>` 里、一个格子能装多门课、
//! 行里还有 rowspan 展开——拿一条正则去匹配嵌套 HTML 必然在改版时碎裂，
//! 而且碎裂方式是**静默丢字段**（旧 Demo 的事故源）。
//! 这里改成按结构分步，每步只做一件事，每步都能单独测：
//!
//! ```text
//! ① 定位 table#timetable / table.timetable           R6     py:145-151
//! ② 切最外层 <tr>                                     R6     py:154-190
//! ③ 切 <td>/<th>（跳过嵌套表格与同名列）               R6/R7  py:183-185
//! ④ 只取 div.kbcontent（显式排除 kbcontent1）           R1     py:30-38
//! ⑤ 按 ------（≥5 个连字符）切课程块                    R2     py:18,40-45
//! ⑥ 块内按 <font title> 抽字段                         R3     py:89-142
//! ⑦ 规范化后去重（周次展开成集合再比）                  R4     py:191-203
//! ```
//!
//! `py:NN` 指旧 Demo 的 `parse_schedule_time.py` 行号，证据见
//! `docs/migration/v0-reuse.md` §1。
//!
//! **与旧实现的三处刻意差异**（都是 §4.1 的静默点，必须改）：
//! - py:44 用 `if "<font" in part` 静默丢块 → 这里每段都进解析器，没 `<font>` 就记 warning；
//! - py:114-119 拿不到课程名/节次时 `return None` → 这里先记 warning 再丢弃该块；
//! - py:183-185 列数不匹配时 `break`（整表错位一格）→ 这里返回 `ColumnMismatch`（R6）。
//!
//! 告警文案**不放课程名/教室/教师**（v0-reuse §4.2 纪律 3：日志不落学生信息明文），
//! 只用"第 N 行·星期X·第 M 块"定位，加上周次/节次原文（不含隐私）。

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::sync::OnceLock;

use regex::Regex;

use super::{ParseError, ParseReport};
use crate::calendar::{parse_weeks, Course};

/// 表头必须恰好 7 个"星期X"（R7）。
const WEEKDAY_COLUMNS: usize = 7;

/// 全天连续编号 1..=13（R11/§3.1）。
const MAX_PERIOD: u8 = 13;

/// 告警条数上限：一张畸形页面不该因为每格都报错就吃掉内存。
const MAX_WARNINGS: usize = 64;

// ---------------------------------------------------------------------------
// 静态正则：字面量写错只能运行期发现，所以集中在一个地方并 `expect` 显式暴露
// ---------------------------------------------------------------------------

/// 任意标签：`<tag ...>` / `</tag>`。只用来做**结构切分**，不负责字段语义。
///
/// 已知边界：属性值里出现 `>`（如 `title="a>b"`）会被提前截断；教务页面不会这样写。
static TAG_RE: OnceLock<Regex> = OnceLock::new();
static ATTR_RE: OnceLock<Regex> = OnceLock::new();
static BLOCK_SEP_RE: OnceLock<Regex> = OnceLock::new();
static BRACKET_RE: OnceLock<Regex> = OnceLock::new();
static PERIOD_SUFFIX_RE: OnceLock<Regex> = OnceLock::new();
static ROW_LABEL_RE: OnceLock<Regex> = OnceLock::new();
static WEEKDAY_ALT_RE: OnceLock<Regex> = OnceLock::new();

fn tag_re() -> &'static Regex {
    TAG_RE.get_or_init(|| {
        Regex::new(r"(?i)<\s*(/?)\s*([a-zA-Z][a-zA-Z0-9]*)\b([^>]*)>")
            .expect("内置正则应始终可编译")
    })
}

fn attr_re() -> &'static Regex {
    ATTR_RE.get_or_init(|| {
        Regex::new(
            r#"(?i)([A-Za-z_][A-Za-z0-9_:.-]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))"#,
        )
        .expect("内置正则应始终可编译")
    })
}

/// R2（py:18）：`------` 是同一格里多门课的分隔符。
///
/// 只要求 **≥5 个**连字符——旧页面写 6 个，写死 6 会在改版后失效（R2 明确要求）。
fn block_sep_re() -> &'static Regex {
    BLOCK_SEP_RE.get_or_init(|| {
        Regex::new(r"(?i)(?:<br\s*/?>\s*)?-{5,}(?:\s*<br\s*/?>)*").expect("内置正则应始终可编译")
    })
}

/// R3（py:102-108）：`周次(节次)` 里的 `[01-02节]`。非贪婪，只截第一对（`\[(.+?)\]`）。
fn bracket_re() -> &'static Regex {
    BRACKET_RE.get_or_init(|| Regex::new(r"\[([^\[\]]*)\]").expect("内置正则应始终可编译"))
}

/// R10a：节次也可能直接写在周次文本里（`3-4节`），不带方括号。
fn period_suffix_re() -> &'static Regex {
    PERIOD_SUFFIX_RE.get_or_init(|| {
        Regex::new(r"(?:第\s*)?\d{1,2}(?:\s*[-~—至]\s*\d{1,2})?\s*节")
            .expect("内置正则应始终可编译")
    })
}

/// 行首 `第N节`（py:21,178）——块内丢了 `[节次]` 时的兜底。
///
/// 同时接受 `第3节` 与 `第1-2节`（有的页面按"大节"标行），区间交给 [`parse_periods`]。
fn row_label_re() -> &'static Regex {
    ROW_LABEL_RE.get_or_init(|| {
        Regex::new(r"第\s*\d{1,2}\s*(?:[-~—至]\s*\d{1,2}\s*)?节").expect("内置正则应始终可编译")
    })
}

/// R7：表头文字可能写 `星期一`、`周一`、`星期一 03/02`；**只用来识别表头行**，
/// 星期值一律取表头下标 + 1（不去猜"三"是几）。
fn weekday_alt_re() -> &'static Regex {
    WEEKDAY_ALT_RE
        .get_or_init(|| Regex::new(r"周[一二三四五六日天]").expect("内置正则应始终可编译"))
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 解析课表页，返回课程 + 告警（契约见 [`super`] 的模块文档）。
pub(super) fn parse_report(html: &str) -> Result<ParseReport, ParseError> {
    let mut warnings = Warnings::default();

    // ① 定位表格（R6 / py:149-151：找不到就失败，绝不产出空课表）
    let table = locate_table(html).ok_or(ParseError::NoTable)?;

    // ② 切行；③ 定位表头（R7 / py:154-169）
    let rows = children(table, |tag| tag_is(tag, "tr"));
    let (header_index, weekday_columns) = find_header(&rows).ok_or(ParseError::NoHeader)?;

    let mut courses: Vec<Course> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let row_no = index + 1;
        // 表头之前的行是标题/学期说明，不可能是课程行，直接跳过（否则会被当成缺列）
        if index <= header_index {
            continue;
        }

        let cells = children(row.inner, is_cell_tag);
        if cells.is_empty() {
            continue;
        }
        // 个别页面会重复输出表头行（rowspan 分块打印）
        if cells
            .iter()
            .filter(|cell| is_weekday_header(&clean_text(cell.inner)))
            .count()
            == WEEKDAY_COLUMNS
        {
            continue;
        }
        // 整行都空的（周末没课）不是错误
        if cells.iter().all(|cell| clean_text(cell.inner).is_empty()) {
            continue;
        }

        let (offset, row_period) = if cells.len() == weekday_columns + 1 {
            // 常规布局：首格是 `<th>第N节</th>`，后 7 格依次对应星期表头
            let label = clean_text(cells[0].inner);
            let period = row_period(&label);
            if period.is_none() && !label.is_empty() {
                warnings.push(format!(
                    "第 {row_no} 行首格不是「第N节」（「{}」）：已按 {weekday_columns} 列对齐继续解析",
                    clip(&label, 16)
                ));
            }
            (1usize, period)
        } else if cells.len() == weekday_columns {
            (0usize, None)
        } else if cells.len() <= 2
            && cells
                .last()
                .and_then(|cell| colspan(cell.attrs))
                .is_some_and(|span| span >= weekday_columns)
        {
            // R6 允许的唯一例外：整行是一个跨列大格（如"国庆放假"，可能还带行首节次单元格）
            warnings.push(format!(
                "第 {row_no} 行是整行跨列单元格：按「无课」处理（R6 例外）"
            ));
            continue;
        } else {
            // R6：列数不匹配必须**失败**——旧实现的 `break` 会让整表静默错位一格（py:183-185）
            return Err(ParseError::ColumnMismatch {
                row: row_no,
                expected: weekday_columns,
                got: cells.len(),
            });
        };

        for (column, cell) in cells[offset..].iter().enumerate() {
            // R7：星期值 = 表头下标 + 1，不用中文数字映射
            let weekday = u8::try_from(column + 1).unwrap_or(0);
            if weekday == 0 || usize::from(weekday) > weekday_columns {
                continue; // 防御：上面已保证列数，不会走到这里
            }
            parse_cell(
                cell,
                row_no,
                weekday,
                row_period,
                &mut courses,
                &mut warnings,
            );
        }
    }

    let courses = dedupe(courses);
    if courses.is_empty() {
        // 空课表只有一种合法来源——服务器明确返回 0 门课（§4.1/§4.2）。
        // 这里解析不出任何课程说明页面结构已变，必须让上层看见。
        return Err(ParseError::Empty);
    }

    Ok(ParseReport {
        courses,
        warnings: warnings.into_vec(),
    })
}

// ---------------------------------------------------------------------------
// ① 定位表格（R6 / py:145-151）
// ---------------------------------------------------------------------------

/// `soup.find("table", id="timetable")`。
///
/// 先按 `id="timetable"` 精确匹配；找不到再退一步接受 `class` 里含 `timetable` 的表格。
/// `id="timetableX"` 必须**找不到**（§4.3 反向用例），所以只做整值/整词比较，不做子串比较。
fn locate_table(html: &str) -> Option<&str> {
    let mut by_class: Option<usize> = None;
    for caps in tag_re().captures_iter(html) {
        let Some(whole) = caps.get(0) else { continue };
        if is_closing(&caps) {
            continue;
        }
        let tag = caps.get(2).map_or("", |m| m.as_str());
        if !tag_is(tag, "table") {
            continue;
        }
        let attrs = caps.get(3).map_or("", |m| m.as_str());
        if attr(attrs, "id").is_some_and(|id| id.trim().eq_ignore_ascii_case("timetable")) {
            return Some(table_inner(html, whole.end()));
        }
        if by_class.is_none()
            && attr(attrs, "class").is_some_and(|class| {
                class
                    .split_whitespace()
                    .any(|t| t.eq_ignore_ascii_case("timetable"))
            })
        {
            by_class = Some(whole.end());
        }
    }
    by_class.map(|start| table_inner(html, start))
}

/// 从表格开标签之后配平 `</table>`（表格里还能嵌表格），返回表格内部 HTML。
fn table_inner(html: &str, start: usize) -> &str {
    let rest = &html[start..];
    let mut depth = 1usize;
    for caps in tag_re().captures_iter(rest) {
        let Some(whole) = caps.get(0) else { continue };
        let tag = caps.get(2).map_or("", |m| m.as_str());
        if !tag_is(tag, "table") {
            continue;
        }
        if is_closing(&caps) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return &rest[..whole.start()];
            }
        } else {
            depth += 1;
        }
    }
    rest // 页面漏了 `</table>`：按到文末处理，后续结构切分仍能工作
}

// ---------------------------------------------------------------------------
// ②③ 行列切分：不做嵌套 HTML 解析，只按深度配平切最外层元素
// ---------------------------------------------------------------------------

/// 一个最外层结构性元素：只留解析需要的三样东西。
#[derive(Debug)]
struct El<'a> {
    /// 开标签里的属性串（`title="教师"` 这些）
    attrs: &'a str,
    /// 开标签之后 .. 闭标签之前
    inner: &'a str,
    /// 整个元素（含开闭标签）在源串里的字节区间，用于"整类元素摘除"
    span: (usize, usize),
}

/// 切出 `region` 里**最外层**满足 `want` 的元素。
///
/// 两处"跳过"保证不越层：
/// - `region` 内部出现的 `<table>` 整段跳过（嵌套表格里的 tr/td 不属于本层）；
/// - 同名元素按深度配平（`td` 里再套 `td` 不会被当成本层第二个单元格）。
fn children(region: &str, want: impl Fn(&str) -> bool) -> Vec<El<'_>> {
    let mut out = Vec::new();
    let mut nested_tables = 0usize;
    let mut depth = 0usize;
    // (attrs, inner 起点, 开标签起点)
    let mut pending: Option<(&str, usize, usize)> = None;

    for caps in tag_re().captures_iter(region) {
        let Some(whole) = caps.get(0) else { continue };
        let tag = caps.get(2).map_or("", |m| m.as_str());

        if tag_is(tag, "table") {
            if is_closing(&caps) {
                nested_tables = nested_tables.saturating_sub(1);
            } else {
                nested_tables += 1;
            }
            continue;
        }
        if nested_tables > 0 || !want(tag) {
            continue;
        }

        if is_closing(&caps) {
            if depth == 0 {
                continue; // 孤儿闭标签：忽略，不要 panic
            }
            depth -= 1;
            if depth == 0 {
                if let Some((attrs, inner_start, element_start)) = pending.take() {
                    out.push(El {
                        attrs,
                        inner: &region[inner_start..whole.start()],
                        span: (element_start, whole.end()),
                    });
                }
            }
        } else {
            if depth == 0 {
                pending = Some((
                    caps.get(3).map_or("", |m| m.as_str()),
                    whole.end(),
                    whole.start(),
                ));
            }
            depth += 1;
        }
    }

    // 尾巴上漏了闭标签（页面偶尔少写 `</td>`）：按"到 region 末尾"处理
    if let Some((attrs, inner_start, element_start)) = pending {
        out.push(El {
            attrs,
            inner: &region[inner_start..],
            span: (element_start, region.len()),
        });
    }
    out
}

/// 把 `region` 里满足 `pred` 的 `tag` 元素整段摘掉（用于排除 `kbcontent1` 简版）。
fn remove_elements(region: &str, tag: &str, pred: impl Fn(&El<'_>) -> bool) -> String {
    let mut out = String::with_capacity(region.len());
    let mut cursor = 0usize;
    for element in children(region, |name| tag_is(name, tag)) {
        if !pred(&element) {
            continue;
        }
        let (start, end) = element.span;
        if start < cursor || end > region.len() {
            continue; // 重叠区间（畸形页面）：跳过，别切出非法 UTF-8 边界
        }
        out.push_str(&region[cursor..start]);
        cursor = end;
    }
    out.push_str(&region[cursor..]);
    out
}

fn is_cell_tag(tag: &str) -> bool {
    tag_is(tag, "td") || tag_is(tag, "th")
}

fn tag_is(tag: &str, want: &str) -> bool {
    tag.eq_ignore_ascii_case(want)
}

fn is_closing(caps: &regex::Captures<'_>) -> bool {
    caps.get(1).is_some_and(|m| m.as_str() == "/")
}

/// ③ 表头（R7 / py:154-169）：恰好 7 个"星期X"所在的行。
fn find_header(rows: &[El<'_>]) -> Option<(usize, usize)> {
    rows.iter().enumerate().find_map(|(index, row)| {
        let cells = children(row.inner, is_cell_tag);
        let count = cells
            .iter()
            .filter(|cell| is_weekday_header(&clean_text(cell.inner)))
            .count();
        (count == WEEKDAY_COLUMNS).then_some((index, count))
    })
}

fn is_weekday_header(text: &str) -> bool {
    text.contains("星期") || weekday_alt_re().is_match(text)
}

fn colspan(attrs: &str) -> Option<usize> {
    attr(attrs, "colspan").and_then(|value| value.trim().parse().ok())
}

// ---------------------------------------------------------------------------
// ④ 单元格 → 课程块（R1 / py:30-38）
// ---------------------------------------------------------------------------

fn parse_cell(
    cell: &El<'_>,
    row_no: usize,
    weekday: u8,
    row_period: Option<(u8, u8)>,
    courses: &mut Vec<Course>,
    warnings: &mut Warnings,
) {
    // R1：一个 kbcontent = 一个"课程块来源"；少数页面把同格的两门课拆成两个 div
    let sources = cell_sources(cell, row_no, weekday, warnings);
    let mut block_no = 0usize;
    for source in &sources {
        for block in split_blocks(source.as_ref()) {
            block_no += 1;
            let loc = Loc {
                row: row_no,
                weekday,
                block: block_no,
            };
            if let Some(course) = parse_block(block, weekday, row_period, loc, warnings) {
                courses.push(course);
            }
        }
    }
}

/// R1（py:30-38）：只取 `div.kbcontent`（完整版），**显式排除 `kbcontent1`**（简版）。
///
/// 旧页面对每门课同时输出简版 + 完整版；两份都解析 → 每门课出现两次，
/// 用户第一反应是"这个 App 数据是错的"。
///
/// 取不到 `div.kbcontent` 时退回整格，但要：
/// 1. 先摘掉 `kbcontent1`（否则退回时又把简版吃进来，等于白做 R1）；
/// 2. 记一条 warning（页面结构变了的信号，不是正常路径）。
fn cell_sources<'a>(
    cell: &El<'a>,
    row_no: usize,
    weekday: u8,
    warnings: &mut Warnings,
) -> Vec<Cow<'a, str>> {
    let mut sources: Vec<Cow<'a, str>> = Vec::new();
    for div in children(cell.inner, |tag| tag_is(tag, "div")) {
        if has_class_token(div.attrs, "kbcontent") {
            sources.push(Cow::Borrowed(div.inner));
        }
    }
    if !sources.is_empty() {
        return sources;
    }
    if clean_text(cell.inner).is_empty() {
        return Vec::new(); // 空课表格：不是错误
    }
    warnings.push(format!(
        "第 {row_no} 行·星期{weekday} 的单元格没有 div.kbcontent：已退回整格解析（R1：疑似页面改版）"
    ));
    vec![Cow::Owned(remove_elements(cell.inner, "div", |el| {
        has_class_token(el.attrs, "kbcontent1")
    }))]
}

/// R2（py:18, 40-45）：按 `------` 切块。
///
/// 旧实现用 `if "<font" in part` 过滤，**静默丢块**（py:44）；
/// 这里每段都送进 [`parse_block`]，没有 `<font>` 的段会落一条 warning（R2/R5）。
fn split_blocks(html: &str) -> Vec<&str> {
    block_sep_re()
        .split(html)
        .filter(|part| !clean_text(part).is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// ⑤⑥ 课程块 → 字段（R3 / py:89-142）
// ---------------------------------------------------------------------------

/// 一门课在页面上能出现的字段，按"首次赋值优先"累积（R3 的 `setdefault` 语义）。
#[derive(Debug, Default)]
struct Fields {
    name: Option<String>,
    teacher: Option<String>,
    room: Option<String>,
    /// 周次原文（`1-16`、`1-5,7-9`、`1-16单`、`4-15(周)`），展开交给 `calendar::parse_weeks`
    weeks: Option<String>,
    period: Option<(u8, u8)>,
}

/// `title` 段名 → 字段（R3 表 + 容错扩展，见 [`apply_field`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Teacher,
    Room,
    Weeks,
    Period,
    Unknown,
}

/// 告警里的定位信息。
///
/// **只放位置与字段名，不放课程名/教室/教师**——v0-reuse §4.2 纪律 3：
/// 告警可能被原样写进日志，而日志不许落学生信息明文。
#[derive(Debug, Clone, Copy)]
struct Loc {
    row: usize,
    weekday: u8,
    block: usize,
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "第 {} 行·星期{}·第 {} 块",
            self.row, self.weekday, self.block
        )
    }
}

fn parse_block(
    block: &str,
    weekday: u8,
    row_period: Option<(u8, u8)>,
    loc: Loc,
    warnings: &mut Warnings,
) -> Option<Course> {
    // 不要求 `<font>` 是块的第一层：`<span><font>` 这类包裹也能抓到
    let fonts = children(block, |tag| tag_is(tag, "font"));
    if fonts.is_empty() {
        warnings.push(format!(
            "{loc}：课程块里没有任何 <font>，已跳过（R2/R5：旧实现会静默丢弃）"
        ));
        return None;
    }

    let mut fields = Fields::default();
    for font in &fonts {
        collect_font(font, &mut fields, loc, warnings);
    }

    // R5（py:114-115）：拿不到课程名 → 丢弃，但要可观测
    let Some(name) = fields.name.filter(|name| !name.is_empty()) else {
        warnings.push(format!(
            "{loc}：拿不到课程名（没有无 title 的 <font>），已丢弃该块（R5/py:114-115）"
        ));
        return None;
    };

    let weeks = fields.weeks.unwrap_or_default();
    if weeks.is_empty() {
        warnings.push(format!(
            "{loc}：缺少周次（title=周次(节次)），已按空周次保留（R5）"
        ));
    } else if parse_weeks(&weeks).is_empty() {
        // §4.1 静默点 6：周次解析失败 → 空集合 → 这门课永远"非本周"
        warnings.push(format!(
            "{loc}：周次原文「{}」无法展开（R8/R9），该课程可能永远显示为非本周",
            clip(&weeks, 24)
        ));
    }

    // 节次优先级：块内 `[01-02节]` → 行首 `第N节`（py:21,178 的 fallback）
    let (start_period, end_period) = match fields.period.or(row_period) {
        Some(range) => range,
        None => {
            warnings.push(format!(
                "{loc}：拿不到节次（[节次] 与行首「第N节」都失败），已丢弃该块（R5/py:117-119）"
            ));
            return None;
        }
    };
    if start_period == 0 || end_period == 0 || start_period > end_period || end_period > MAX_PERIOD
    {
        warnings.push(format!(
            "{loc}：节次区间 {start_period}-{end_period} 越界，已丢弃该块（R10a）"
        ));
        return None;
    }

    Some(Course {
        name,
        weekday,
        start_period,
        end_period,
        weeks,
        room: fields.room,
        teacher: fields.teacher,
    })
}

/// R3（py:89-112）：`title` 决定字段；**没有 title 的 `<font>` 是课程名**。
///
/// `title` 只有 1~3 段时按位置取（`教师|周次(节次)|教室` 这种把三个字段塞进一个 font
/// 的写法也见过），缺的段留 `None`；值里多出来的 `|` 不会 panic（`get` 取不到就是 None）。
fn collect_font(font: &El<'_>, fields: &mut Fields, loc: Loc, warnings: &mut Warnings) {
    let text = clean_text(font.inner);
    let title = attr(font.attrs, "title").unwrap_or_default();
    let names: Vec<&str> = title
        .split(['|', '｜'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();

    if names.is_empty() {
        // py:111-112：仅当课程名尚未赋值
        if text.is_empty() {
            return;
        }
        match &fields.name {
            None => fields.name = Some(text),
            Some(existing) => {
                if existing.as_str() != text {
                    warnings.push(format!(
                        "{loc}：出现第二个无 title 的 <font>，已忽略（R3：疑似页面结构变化）"
                    ));
                }
            }
        }
        return;
    }

    if names.len() == 1 {
        apply_field(fields, loc, warnings, names[0], &text);
        return;
    }

    let values: Vec<&str> = text.split(['|', '｜']).map(str::trim).collect();
    if values.len() != names.len() {
        warnings.push(format!(
            "{loc}：title「{}」有 {} 段、值有 {} 段，按位置取（R3 容错）",
            clip(&title, 24),
            names.len(),
            values.len()
        ));
    }
    for (index, name) in names.iter().enumerate() {
        let value = values
            .get(index)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        if let Some(value) = value {
            apply_field(fields, loc, warnings, name, value);
        }
    }
}

fn apply_field(fields: &mut Fields, loc: Loc, warnings: &mut Warnings, label: &str, value: &str) {
    match classify(label) {
        Field::Name => set_once(&mut fields.name, value, "课程名", loc, warnings),
        Field::Teacher => set_once(&mut fields.teacher, value, "教师", loc, warnings),
        Field::Room => set_once(&mut fields.room, value, "教室", loc, warnings),
        Field::Weeks => {
            let (weeks, period) = split_weeks_and_period(value);
            set_once(&mut fields.weeks, &weeks, "周次", loc, warnings);
            if let Some(text) = period {
                apply_period(fields, &text, loc, warnings);
            }
        }
        Field::Period => apply_period(fields, value, loc, warnings),
        Field::Unknown => warnings.push(format!(
            "{loc}：未知的 title「{}」，已忽略（R3）",
            clip(label, 16)
        )),
    }
}

fn apply_period(fields: &mut Fields, text: &str, loc: Loc, warnings: &mut Warnings) {
    match parse_periods(text) {
        None => warnings.push(format!(
            "{loc}：节次原文「{}」无法解析（R10a），将回退到行首「第N节」",
            clip(text, 16)
        )),
        Some(range) => match fields.period {
            None => fields.period = Some(range),
            Some(existing) if existing != range => {
                warnings.push(format!("{loc}：节次重复出现且不一致（保留首次，R3）"))
            }
            Some(_) => {}
        },
    }
}

fn classify(segment: &str) -> Field {
    // 顺序有意义：`周次(节次)` 同时含"周次"和"节次"，先判周次（它顺带带节次）
    if segment.contains("教师") {
        Field::Teacher
    } else if segment.contains("周次") {
        Field::Weeks
    } else if segment.contains("教室") {
        Field::Room
    } else if segment.contains("节次") {
        Field::Period
    } else if segment.contains("课程") {
        Field::Name
    } else {
        Field::Unknown
    }
}

/// `setdefault` 语义 + R3 的建议：重复出现且值不同 → 记 warn（往往意味着页面结构变了）。
fn set_once(
    slot: &mut Option<String>,
    value: &str,
    label: &str,
    loc: Loc,
    warnings: &mut Warnings,
) {
    if value.is_empty() {
        return;
    }
    match slot {
        None => *slot = Some(value.to_owned()),
        Some(existing) if existing.as_str() != value => warnings.push(format!(
            "{loc}：字段「{label}」重复出现且值不同，保留首次值（R3：疑似页面结构变化）"
        )),
        Some(_) => {}
    }
}

/// R3（py:102-108）：从 `周次(节次)` 里拆出周次原文与节次原文。
///
/// `1-16[01-02节]` → (`1-16`, `01-02节`)；节次不带方括号（`1-16 3-4节`）也能拆。
fn split_weeks_and_period(text: &str) -> (String, Option<String>) {
    let normalized = normalize_brackets(text);
    let period = bracket_re()
        .captures(&normalized)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_owned())
        .filter(|text| !text.is_empty());
    let without_brackets = bracket_re().replace_all(&normalized, " ");

    if period.is_some() {
        return (clean_text(&without_brackets), period);
    }
    if let Some(found) = period_suffix_re().find(&without_brackets) {
        let mut rest = String::with_capacity(without_brackets.len());
        rest.push_str(&without_brackets[..found.start()]);
        rest.push_str(&without_brackets[found.end()..]);
        return (clean_text(&rest), Some(found.as_str().to_owned()));
    }
    (clean_text(&without_brackets), None)
}

/// R10a（py:79-86）：`01-02节`、`1-2节`、`第3节`、`13` 都解析成 `(start, end)`。
///
/// 没有数字（`待定`）或超出 1..=13 → `None`，由调用方记 warning 并回退行首节次；
/// **不 `findall(\d+)` 兜底猜**（§2.2 第 8 条：宁可失败也不产出错误数据）。
fn parse_periods(text: &str) -> Option<(u8, u8)> {
    let numbers: Vec<u8> = number_runs(text)
        .into_iter()
        .filter(|value| (1..=u32::from(MAX_PERIOD)).contains(value))
        .filter_map(|value| u8::try_from(value).ok())
        .collect();
    match numbers.len() {
        0 => None,
        1 => Some((numbers[0], numbers[0])),
        _ => {
            let first = numbers[0];
            let last = numbers[numbers.len() - 1];
            Some((first.min(last), first.max(last)))
        }
    }
}

fn row_period(text: &str) -> Option<(u8, u8)> {
    if let Some(found) = row_label_re().find(text) {
        if let Some(range) = parse_periods(found.as_str()) {
            return Some(range);
        }
    }
    // 有些页面首格只写数字（`1`、`01`）
    let runs = number_runs(text);
    if runs.len() == 1 && text.chars().filter(|c| !c.is_whitespace()).count() <= 4 {
        return u8::try_from(runs[0])
            .ok()
            .filter(|value| (1..=MAX_PERIOD).contains(value))
            .map(|single| (single, single));
    }
    None
}

// ---------------------------------------------------------------------------
// ⑦ 去重（R4 / py:191-203）
// ---------------------------------------------------------------------------

/// R4：同一门课会因 rowspan 在每个小节行重复出现一次，必须按完整信息去重。
///
/// 与旧实现的差异（R4 的"1.0 要修"）：键里的周次用 `calendar::parse_weeks` **展开后的集合**，
/// 而不是原文——否则 `1-16` / `1-16周` 这种写法差异会让去重失效。
fn dedupe(courses: Vec<Course>) -> Vec<Course> {
    let mut seen: HashSet<(String, u8, u8, u8, String, String, String)> =
        HashSet::with_capacity(courses.len());
    let mut out = Vec::with_capacity(courses.len());
    for course in courses {
        let expanded = parse_weeks(&course.weeks);
        let weeks_key = if expanded.is_empty() {
            format!("raw:{}", course.weeks)
        } else {
            expanded
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        let key = (
            course.name.clone(),
            course.weekday,
            course.start_period,
            course.end_period,
            weeks_key,
            course.room.clone().unwrap_or_default(),
            course.teacher.clone().unwrap_or_default(),
        );
        if seen.insert(key) {
            out.push(course);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 文本 / 属性 / 告警工具
// ---------------------------------------------------------------------------

/// R3 的文本清洗（py:24-27）：`get_text("")` 语义——**不插空格**地拼文本节点，
/// 去 `\xa0`（`&nbsp;`），折叠连续空白（但保留词内空格：课程名可能写 `大学 英语`）。
///
/// `<font>高等数学<b>A</b></font>` → `高等数学A`（不是 `高等数学 A`）。
fn clean_text(html: &str) -> String {
    let mut raw = String::with_capacity(html.len());
    let mut cursor = 0usize;
    for caps in tag_re().captures_iter(html) {
        let Some(whole) = caps.get(0) else { continue };
        raw.push_str(&html[cursor..whole.start()]);
        cursor = whole.end();
    }
    raw.push_str(&html[cursor..]);

    let raw = decode_entities(&raw).replace('\u{a0}', "");
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out.trim().to_owned()
}

/// 解常见实体：`&nbsp;` `&amp;` `&lt;` `&gt;` `&quot;` `&apos;` `&#NNN;` `&#xHH;`。
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find('&') {
        out.push_str(&rest[..position]);
        let tail = &rest[position..];
        let Some(end) = tail.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        match entity {
            // 随后会被 `replace('\u{a0}', "")` 统一清掉，这里直接略过
            "nbsp" => {}
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let code = entity.strip_prefix('#').and_then(|number| {
                    if let Some(hex) = number
                        .strip_prefix('x')
                        .or_else(|| number.strip_prefix('X'))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        number.parse::<u32>().ok()
                    }
                });
                match code.and_then(char::from_u32) {
                    Some('\u{a0}') => {}
                    Some(ch) => out.push(ch),
                    None => {
                        out.push('&');
                        out.push_str(entity);
                        out.push(';');
                    }
                }
            }
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// 取标签属性值，支持 `attr="v"`、`attr='v'`、`attr=v` 三种写法。
fn attr(attrs: &str, name: &str) -> Option<String> {
    for caps in attr_re().captures_iter(attrs) {
        let key = caps.get(1).map_or("", |m| m.as_str());
        if !key.eq_ignore_ascii_case(name) {
            continue;
        }
        let value = caps
            .get(2)
            .or_else(|| caps.get(3))
            .or_else(|| caps.get(4))
            .map_or("", |m| m.as_str());
        return Some(decode_entities(value));
    }
    None
}

/// `class` 里是否有某个**整词**（`kbcontent` 不能匹配到 `kbcontent1`）。
fn has_class_token(attrs: &str, class_name: &str) -> bool {
    attr(attrs, "class").is_some_and(|value| {
        value
            .split_whitespace()
            .any(|token| token.eq_ignore_ascii_case(class_name))
    })
}

fn normalize_brackets(text: &str) -> String {
    text.replace(['［', '【'], "[").replace(['］', '】'], "]")
}

/// 扫出所有连续数字（兼容全角数字），供节次解析用；不做跨字段猜测。
fn number_runs(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut current: Option<u32> = None;
    for ch in text.chars() {
        match to_digit(ch) {
            Some(digit) => {
                current = Some(
                    current
                        .unwrap_or_default()
                        .saturating_mul(10)
                        .saturating_add(digit),
                );
            }
            None => {
                if let Some(value) = current.take() {
                    out.push(value);
                }
            }
        }
    }
    if let Some(value) = current {
        out.push(value);
    }
    out
}

fn to_digit(ch: char) -> Option<u32> {
    match ch {
        '0'..='9' => Some(ch as u32 - u32::from(b'0')),
        '０'..='９' => Some(ch as u32 - u32::from('０')),
        _ => None,
    }
}

/// 截短用于告警，别把整段 HTML 塞进日志。
fn clip(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// 告警收集器：上限 [`MAX_WARNINGS`] 条，超出只计数（畸形页面不该刷爆内存）。
#[derive(Debug, Default)]
struct Warnings {
    items: Vec<String>,
    suppressed: usize,
}

impl Warnings {
    fn push(&mut self, message: String) {
        if self.items.len() < MAX_WARNINGS {
            self.items.push(message);
        } else {
            self.suppressed += 1;
        }
    }

    fn into_vec(mut self) -> Vec<String> {
        if self.suppressed > 0 {
            self.items.push(format!(
                "另有 {} 条告警因超过 {MAX_WARNINGS} 条上限被省略",
                self.suppressed
            ));
        }
        self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // fixture 工具：全部内联，不读外部文件（CI 里没有真实页面）
    // -----------------------------------------------------------------

    /// 一张标准课表：表头「节次 + 星期一..星期日」，返回完整页面。
    fn page(id_attr: &str, header: &[&str], rows: &[String]) -> String {
        let mut out = String::from(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>课表</title></head><body>",
        );
        out.push_str(&format!(
            "<table {id_attr} border=\"1\"><thead><tr><th>节次</th>"
        ));
        for label in header {
            out.push_str(&format!("<th>{label}</th>"));
        }
        out.push_str("</tr></thead><tbody>");
        for row in rows {
            out.push_str(row);
        }
        out.push_str("</tbody></table></body></html>");
        out
    }

    /// 一行：行首「第N节」+ 7 个星期格。
    fn period_row(label: &str, cells: [&str; 7]) -> String {
        let mut out = format!("<tr><th>{label}</th>");
        for cell in cells {
            out.push_str(&format!("<td>{cell}</td>"));
        }
        out.push_str("</tr>");
        out
    }

    fn week_headers() -> [&'static str; 7] {
        [
            "星期一",
            "星期二",
            "星期三",
            "星期四",
            "星期五",
            "星期六",
            "星期日",
        ]
    }

    fn parse(html: &str) -> Vec<Course> {
        parse_report(html).expect("fixture 必须解析成功").courses
    }

    fn warnings_of(html: &str) -> Vec<String> {
        parse_report(html).expect("fixture 必须解析成功").warnings
    }

    fn course(
        name: &str,
        weekday: u8,
        start: u8,
        end: u8,
        weeks: &str,
        room: Option<&str>,
        teacher: Option<&str>,
    ) -> Course {
        Course {
            name: name.into(),
            weekday,
            start_period: start,
            end_period: end,
            weeks: weeks.into(),
            room: room.map(str::to_owned),
            teacher: teacher.map(str::to_owned),
        }
    }

    const GAOSHU: &str = concat!(
        "<div class=\"kbcontent\">",
        "<font title=\"教师\">张老师</font>",
        "<font title=\"周次(节次)\">1-16[01-02节]</font>",
        "<font title=\"教室\">东校区1教305</font>",
        "<font>高等数学A</font>",
        "</div>",
        "<div class=\"kbcontent1\">高等数学A<br>张老师<br>1-16[01-02节]<br>东校区1教305</div>"
    );

    const ENGLISH: &str = concat!(
        "<div class=\"kbcontent\">",
        "<font>大学英语</font>",
        "<font title=\"周次(节次)\">1-16(周)[06-07节]</font>",
        "<font title=\"教室\">西校区2教201</font>",
        "<font title=\"教师\">李老师</font>",
        "</div>"
    );

    // -----------------------------------------------------------------
    // 1. 正常两门课：不同星期、不同节次
    // -----------------------------------------------------------------

    #[test]
    fn test_r1_r7_two_courses_with_distinct_weekday_and_period() {
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[
                period_row("第1节", [GAOSHU, "", "", "", "", "", ""]),
                period_row("第6节", ["", "", ENGLISH, "", "", "", ""]),
            ],
        );

        let courses = parse(&html);
        assert_eq!(courses.len(), 2);
        assert_eq!(
            courses[0],
            course(
                "高等数学A",
                1,
                1,
                2,
                "1-16",
                Some("东校区1教305"),
                Some("张老师")
            )
        );
        // 星期来自表头顺序（第 3 列 = 星期三），节次来自 `[06-07节]`
        assert_eq!(
            courses[1],
            course(
                "大学英语",
                3,
                6,
                7,
                "1-16(周)",
                Some("西校区2教201"),
                Some("李老师")
            )
        );
        assert!(warnings_of(&html).is_empty(), "正常页面不该有告警");
    }

    // -----------------------------------------------------------------
    // 2. 一格两门课（`------` 分隔）
    // -----------------------------------------------------------------

    #[test]
    fn test_r2_two_courses_in_one_cell() {
        let cell = concat!(
            "<div class=\"kbcontent\">",
            "<font>高等数学A</font><font title=\"教师\">张老师</font>",
            "<font title=\"周次(节次)\">1-16[01-02节]</font><font title=\"教室\">东305</font>",
            "<br>------<br>",
            "<font>线性代数B</font><font title=\"教师\">王老师</font>",
            "<font title=\"周次(节次)\">1-8[01-02节]</font><font title=\"教室\">东306</font>",
            "</div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第1节", [cell, "", "", "", "", "", ""])],
        );

        let courses = parse(&html);
        assert_eq!(courses.len(), 2, "一格两门课必须切成两条记录");
        assert_eq!(courses[0].name, "高等数学A");
        assert_eq!(courses[0].teacher.as_deref(), Some("张老师"));
        assert_eq!(courses[1].name, "线性代数B");
        assert_eq!(courses[1].teacher.as_deref(), Some("王老师"));
        assert_eq!(courses[1].weeks, "1-8");
        assert!(courses
            .iter()
            .all(|c| c.weekday == 1 && c.start_period == 1));
    }

    // -----------------------------------------------------------------
    // 3. 单双周：原文保留
    // -----------------------------------------------------------------

    #[test]
    fn test_r8_r9_odd_and_even_weeks_are_preserved_verbatim() {
        let odd = concat!(
            "<div class=\"kbcontent\"><font>体育</font>",
            "<font title=\"周次(节次)\">1-16单[03-04节]</font>",
            "<font title=\"教师\">赵老师</font></div>"
        );
        let even = concat!(
            "<div class=\"kbcontent\"><font>形势与政策</font>",
            "<font title=\"周次(节次)\">2-16双[05-06节]</font>",
            "<font title=\"教师\">周老师</font></div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[
                period_row("第3节", ["", "", odd, "", "", "", ""]),
                period_row("第5节", ["", "", even, "", "", "", ""]),
            ],
        );

        let courses = parse(&html);
        assert_eq!(courses.len(), 2);
        // 原文保留：交给 calendar::parse_weeks 展开（存库用原文，排障也看原文）
        assert_eq!(courses[0].weeks, "1-16单");
        assert_eq!(courses[1].weeks, "2-16双");
        // 单双周一旦在剥字符阶段丢掉，展开会变成全部周次（campus_api.py:129-131 的错）
        assert_eq!(parse_weeks("1-16单"), vec![1, 3, 5, 7, 9, 11, 13, 15]);
        assert_eq!(parse_weeks("2-16双"), vec![2, 4, 6, 8, 10, 12, 14, 16]);
    }

    // -----------------------------------------------------------------
    // 4. 无 title 的 font = 课程名
    // -----------------------------------------------------------------

    #[test]
    fn test_r3_font_without_title_is_course_name() {
        let cell = concat!(
            "<div class=\"kbcontent\">",
            "<font title=\"教师\">孙老师</font>",
            "<font title=\"周次(节次)\">4-15(周)[09-10节]</font>",
            "<font>体育（篮球）</font>",
            "</div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第9节", ["", "", "", "", cell, "", ""])],
        );

        let courses = parse(&html);
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].name, "体育（篮球）");
        assert_eq!(courses[0].teacher.as_deref(), Some("孙老师"));
        assert_eq!(courses[0].weeks, "4-15(周)");
        assert_eq!((courses[0].start_period, courses[0].end_period), (9, 10));
    }

    // -----------------------------------------------------------------
    // 5. kbcontent 与 kbcontent1 同时存在时只取 kbcontent
    // -----------------------------------------------------------------

    #[test]
    fn test_r1_only_kbcontent_is_parsed() {
        // 同一格里简版在前、完整版在后：只认完整版
        let cell = format!(
            concat!(
                "<div class=\"kbcontent1\"><font>简版垃圾课</font>",
                "<font title=\"教师\">简版老师</font>",
                "<font title=\"周次(节次)\">1-16[03-04节]</font></div>",
                "{}"
            ),
            GAOSHU
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第3节", [cell.as_str(), "", "", "", "", "", ""])],
        );

        let courses = parse(&html);
        assert_eq!(courses.len(), 1, "同一门课不能出现两次（R1）");
        assert_eq!(courses[0].name, "高等数学A");
        assert_eq!(courses[0].teacher.as_deref(), Some("张老师"));
        assert!(
            courses.iter().all(|c| c.name != "简版垃圾课"),
            "kbcontent1（简版）必须被排除"
        );
    }

    #[test]
    fn test_r1_fallback_without_kbcontent_but_kbcontent1_is_still_excluded() {
        // kbcontent 被包了一层（不是 td 的直接子元素）→ 退回整格，但简版仍要排除
        let cell = concat!(
            "<div class=\"wrap\">",
            "<div class=\"kbcontent\"><font>数据结构</font>",
            "<font title=\"教师\">赵老师</font>",
            "<font title=\"周次(节次)\">1-16[05-06节]</font></div>",
            "</div>",
            "<div class=\"kbcontent1\"><font>另一种写法的简版课</font>",
            "<font title=\"教师\">简版老师</font></div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第5节", [cell, "", "", "", "", "", ""])],
        );

        let courses = parse(&html);
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].name, "数据结构");
        assert!(
            warnings_of(&html)
                .iter()
                .any(|w| w.contains("div.kbcontent")),
            "退回整格必须留下 warning（R1）"
        );
    }

    // -----------------------------------------------------------------
    // 6. 三种失败路径
    // -----------------------------------------------------------------

    #[test]
    fn test_no_table() {
        let html = page(
            "id=\"timetableX\"",
            &week_headers(),
            &[period_row("第1节", [GAOSHU, "", "", "", "", "", ""])],
        );
        assert_eq!(parse_report(&html), Err(ParseError::NoTable));
        assert_eq!(
            parse_report("<html><body><p>还没登录</p></body></html>"),
            Err(ParseError::NoTable)
        );
    }

    #[test]
    fn test_no_header() {
        // 表头只有 6 天（R7：必须恰好 7 个）
        let six = &week_headers()[..6];
        let html = page(
            "id=\"timetable\"",
            six,
            &[period_row("第1节", [GAOSHU, "", "", "", "", "", ""])],
        );
        assert_eq!(parse_report(&html), Err(ParseError::NoHeader));

        // 表头没有"星期/周X"字样
        let html = page(
            "id=\"timetable\"",
            &["一", "二", "三", "四", "五", "六", "日"],
            &[period_row("第1节", [GAOSHU, "", "", "", "", "", ""])],
        );
        assert_eq!(parse_report(&html), Err(ParseError::NoHeader));
    }

    #[test]
    fn test_empty() {
        // 结构完整但一个块都没有 → 不是 Ok(vec![])
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[
                period_row("第1节", ["", "", "", "", "", "", ""]),
                period_row("第2节", ["", "", "", "", "", "", ""]),
            ],
        );
        assert_eq!(parse_report(&html), Err(ParseError::Empty));
    }

    #[test]
    fn test_column_mismatch_fails_instead_of_shifting() {
        // 某行多一个 td（8 个格 + 行首）→ R6 必须失败，绝不按错位一格解析
        let row = format!(
            "<tr><th>第1节</th><td>{GAOSHU}</td>{}<td>多的格</td></tr>",
            "<td></td>".repeat(6)
        );
        let html = page("id=\"timetable\"", &week_headers(), &[row]);
        assert_eq!(
            parse_report(&html),
            Err(ParseError::ColumnMismatch {
                row: 2,
                expected: 7,
                got: 9,
            })
        );
    }

    // -----------------------------------------------------------------
    // 7. 脏数据：节次写成「待定」→ warning，不 panic
    // -----------------------------------------------------------------

    #[test]
    fn test_dirty_period_falls_back_and_warns() {
        let cell = concat!(
            "<div class=\"kbcontent\"><font>形势与政策</font>",
            "<font title=\"周次(节次)\">1-16[待定]</font>",
            "<font title=\"教师\">周老师</font></div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第4节", [cell, "", "", "", "", "", ""])],
        );

        let report = parse_report(&html).expect("脏数据不能中断整张表");
        assert_eq!(report.courses.len(), 1);
        // 回退到行首「第4节」
        assert_eq!(
            (report.courses[0].start_period, report.courses[0].end_period),
            (4, 4)
        );
        assert_eq!(report.courses[0].weeks, "1-16");
        assert!(
            report.warnings.iter().any(|w| w.contains("节次")),
            "脏节次必须留下 warning，实际：{:?}",
            report.warnings
        );
    }

    #[test]
    fn test_dirty_rows_do_not_break_the_whole_table() {
        // 一个块缺课程名、一个块没有 font：都应过期，但另一门课照常解析
        let broken = concat!(
            "<div class=\"kbcontent\">",
            "<font title=\"周次(节次)\">1-16[01-02节]</font>",
            "</div><br>------<br><div class=\"kbcontent\">纯文本没有 font</div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[
                period_row("第1节", [broken, "", "", "", "", "", ""]),
                period_row("第6节", ["", "", ENGLISH, "", "", "", ""]),
            ],
        );

        let report = parse_report(&html).expect("局部脏块不能中断整表（§4.2: 只报不 abort）");
        assert_eq!(report.courses.len(), 1);
        assert_eq!(report.courses[0].name, "大学英语");
        assert!(report.warnings.iter().any(|w| w.contains("课程名")));
        assert!(report.warnings.iter().any(|w| w.contains("<font>")));
    }

    // -----------------------------------------------------------------
    // 字段容错（R3 / R10a）
    // -----------------------------------------------------------------

    #[test]
    fn test_period_spellings() {
        for (text, expected) in [
            ("01-02节", (1u8, 2u8)),
            ("1-2节", (1, 2)),
            ("第3节", (3, 3)),
            ("13", (13, 13)),
            ("10-13节", (10, 13)),
            ("０１-０２节", (1, 2)),
        ] {
            assert_eq!(parse_periods(text), Some(expected), "节次「{text}」");
        }
        assert_eq!(parse_periods("待定"), None);
        assert_eq!(parse_periods(""), None);
        // 越界：一学期不可能超过 13 节
        assert_eq!(parse_periods("第14节"), None);
    }

    #[test]
    fn test_title_with_one_to_three_segments_is_positional() {
        let three = concat!(
            "<div class=\"kbcontent\">",
            "<font>概率论</font>",
            "<font title=\"教师|周次(节次)|教室\">钱老师|1-16[07-08节]|东401</font>",
            "</div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第7节", [three, "", "", "", "", "", ""])],
        );
        let courses = parse(&html);
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].teacher.as_deref(), Some("钱老师"));
        assert_eq!(courses[0].room.as_deref(), Some("东401"));
        assert_eq!(courses[0].weeks, "1-16");

        // 2 段但只给 1 个值：按位置取，缺的字段为 None，不 panic
        let two = concat!(
            "<div class=\"kbcontent\"><font>离散数学</font>",
            "<font title=\"教师|教室\">孙老师</font>",
            "<font title=\"周次(节次)\">1-8[11-12节]</font></div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第11节", [two, "", "", "", "", "", ""])],
        );
        let courses = parse(&html);
        assert_eq!(courses[0].teacher.as_deref(), Some("孙老师"));
        assert_eq!(courses[0].room, None);

        // 值里出现 `|` 不 panic（教师名/教室名可能带分隔符）
        let pipe = concat!(
            "<div class=\"kbcontent\"><font>编译原理</font>",
            "<font title=\"教师\">张|老师</font>",
            "<font title=\"教室\">东|305</font>",
            "<font title=\"周次(节次)\">1-16[01-02节]</font></div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第1节", [pipe, "", "", "", "", "", ""])],
        );
        let courses = parse(&html);
        assert_eq!(courses[0].teacher.as_deref(), Some("张|老师"));
        assert_eq!(courses[0].room.as_deref(), Some("东|305"));
    }

    #[test]
    fn test_nested_inline_tags_and_entities_are_cleaned() {
        let cell = concat!(
            "<div class=\"kbcontent\">",
            "<font>高等数学<b>A</b></font>",
            "<font title=\"教师\"><span>张老师</span></font>",
            "<font title=\"周次(节次)\">1-16&#160;[01-02节]</font>",
            "<font title=\"教室\">东&nbsp;305</font>",
            "</div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第1节", [cell, "", "", "", "", "", ""])],
        );
        let courses = parse(&html);
        assert_eq!(courses[0].name, "高等数学A", "get_text(\"\") 不插空格");
        assert_eq!(courses[0].teacher.as_deref(), Some("张老师"));
        assert_eq!(courses[0].room.as_deref(), Some("东305"));
        assert_eq!(courses[0].weeks, "1-16");
    }

    #[test]
    fn test_weekday_alternative_headers_and_missing_corner_cell() {
        // 表头写"周一"，且数据行没有行首 th（7 个 td）
        let row = format!(
            "<tr><td></td><td></td><td>{GAOSHU}</td><td></td><td></td><td></td><td></td></tr>"
        );
        let html = page(
            "id=\"timetable\"",
            &["周一", "周二", "周三", "周四", "周五", "周六", "周日"],
            &[row],
        );
        let courses = parse(&html);
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].weekday, 3);
        assert_eq!((courses[0].start_period, courses[0].end_period), (1, 2));
    }

    #[test]
    fn test_rowspan_repetition_is_deduped_with_normalised_weeks() {
        // 同一门课在两行里重复（rowspan 展开），且周次写法不同：规范化后仍是同一门
        let first = concat!(
            "<div class=\"kbcontent\"><font>马克思主义原理</font>",
            "<font title=\"教师\">陈老师</font>",
            "<font title=\"周次(节次)\">1-16[01-02节]</font>",
            "<font title=\"教室\">东201</font></div>"
        );
        let second = first.replace("1-16[01-02节]", "1-16周[01-02节]");
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[
                period_row("第1节", [first, "", "", "", "", "", ""]),
                period_row("第2节", [second.as_str(), "", "", "", "", "", ""]),
            ],
        );

        let courses = parse(&html);
        assert_eq!(courses.len(), 1, "R4：去重键用展开后的周次集合");
        assert_eq!(courses[0].weeks, "1-16", "保留首次出现的原文");
    }

    #[test]
    fn test_full_width_row_label_and_colspan_row() {
        // 整行跨列（放假/备注）→ R6 唯一例外：按无课处理 + warning
        let row = format!(
            "<tr><th>第5节</th><td colspan=\"7\">国庆放假</td></tr>{}",
            period_row("第1节", [GAOSHU, "", "", "", "", "", ""])
        );
        let html = page("id=\"timetable\"", &week_headers(), &[row]);
        let report = parse_report(&html).expect("跨列行不该让整表失败");
        assert_eq!(report.courses.len(), 1);
        assert!(report.warnings.iter().any(|w| w.contains("跨列")));
    }

    #[test]
    fn test_table_located_by_class_when_id_absent() {
        let html = page(
            "class=\"timetable table\"",
            &week_headers(),
            &[period_row("第1节", [GAOSHU, "", "", "", "", "", ""])],
        );
        assert_eq!(parse(&html).len(), 1);
    }

    #[test]
    fn test_clean_text_matches_old_get_text_semantics() {
        assert_eq!(clean_text("<font>高等数学<b>A</b></font>"), "高等数学A");
        assert_eq!(clean_text("  大学 &nbsp; 英语 "), "大学 英语");
        assert_eq!(clean_text("<br/><br/>"), "");
        assert_eq!(clean_text("A&amp;B"), "A&B");
    }

    #[test]
    fn test_warnings_are_capped() {
        // 每格都坏的畸形表：告警不能无限增长，也不能让整表失败
        let mut rows: Vec<String> = (0..12)
            .map(|_| period_row("第1节", ["<div class=\"kbcontent\">坏格</div>"; 7]))
            .collect();
        rows.push(period_row("第6节", [GAOSHU, "", "", "", "", "", ""]));

        let html = page("id=\"timetable\"", &week_headers(), &rows);
        let report = parse_report(&html).expect("仍有可解析的格就不能整表失败");
        assert_eq!(report.courses.len(), 1);
        assert!(
            report.warnings.len() <= MAX_WARNINGS + 1,
            "告警必须被截断，实际 {} 条",
            report.warnings.len()
        );
        assert!(report.warnings.last().is_some_and(|w| w.contains("上限")));
    }

    /// 有些页面把一个格子的两门课拆成两个 `div.kbcontent`（各自独立），
    /// 而不是用 `------` 分隔：拼接会把两门课合并成一门，所以按 div 分开解析。
    #[test]
    fn test_two_kbcontent_divs_in_one_cell_are_two_courses() {
        let first = concat!(
            "<div class=\"kbcontent\"><font>大学物理</font>",
            "<font title=\"教师\">吴老师</font>",
            "<font title=\"周次(节次)\">1-16[01-02节]</font>",
            "<font title=\"教室\">东101</font></div>"
        );
        let second = concat!(
            "<div class=\"kbcontent\"><font>工程制图</font>",
            "<font title=\"教师\">郑老师</font>",
            "<font title=\"周次(节次)\">3-15[01-02节]</font>",
            "<font title=\"教室\">东102</font></div>"
        );
        let cell = format!("{first}{second}");
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第1节", [cell.as_str(), "", "", "", "", "", ""])],
        );

        let courses = parse(&html);
        assert_eq!(courses.len(), 2, "两个 kbcontent 应是两门课");
        assert_eq!(courses[0].name, "大学物理");
        assert_eq!(courses[1].name, "工程制图");
        assert_eq!(courses[1].weeks, "3-15");
    }

    /// 行首按"大节"标注（`第1-2节`），块内又丢了 `[节次]`：兜底要能取到区间。
    #[test]
    fn test_row_label_range_is_used_as_fallback() {
        let cell = concat!(
            "<div class=\"kbcontent\"><font>大学生心理健康</font>",
            "<font title=\"教师\">冯老师</font>",
            "<font title=\"周次(节次)\">1-16</font></div>"
        );
        let html = page(
            "id=\"timetable\"",
            &week_headers(),
            &[period_row("第1-2节", ["", "", cell, "", "", "", ""])],
        );

        let report = parse_report(&html).expect("缺 [节次] 时应回退行首");
        assert_eq!(report.courses.len(), 1);
        assert_eq!(
            (report.courses[0].start_period, report.courses[0].end_period),
            (1, 2)
        );
        assert_eq!(report.courses[0].weeks, "1-16");
    }

    /// 表头上面的标题行（单格、无 colspan）不能被当成缺列 → 整表失败。
    #[test]
    fn test_caption_row_above_header_is_skipped() {
        let html = format!(
            "<html><body><table id=\"timetable\">\
             <tr><td>2026 学年春季学期课表</td></tr>\
             <tr><th>节次</th><th>星期一</th><th>星期二</th><th>星期三</th>\
             <th>星期四</th><th>星期五</th><th>星期六</th><th>星期日</th></tr>{}\
             </table></body></html>",
            period_row("第1节", [GAOSHU, "", "", "", "", "", ""])
        );
        let report = parse_report(&html).expect("标题行不该让整表失败");
        assert_eq!(report.courses.len(), 1);
        assert_eq!(report.courses[0].name, "高等数学A");
    }
}
