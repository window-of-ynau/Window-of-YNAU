//! C ABI 出口：Flutter 通过 `dart:ffi` 直接调用（**不引** `flutter_rust_bridge` codegen）。
//!
//! # 约定
//!
//! - 所有函数返回 **malloc 出来的 UTF-8 JSON**，调用方必须用 [`yunong_free_string`] 释放；
//! - 信封：成功 `{"ok":true,"data":…}`，失败 `{"ok":false,"code":"…","message":"…"}`；
//! - 入参 `*const c_char` 是空指针或非 UTF-8 → 返回失败信封，**绝不 panic 跨 FFI**（那是 UB）；
//! - 返回值里**不含** Cookie / API Key / 课表 HTML 原文（隐私边界，ADR-02 / ADR-11）。
//!
//! # 为什么手写而不是 flutter_rust_bridge
//!
//! 端侧只需要 4 个函数、返回 JSON 就够；codegen 会带来一行构建依赖 + 版本耦合，
//! 而离线环境里"少一层生成"就是少一个坏点。等接口涨到几十个再换不迟。
//!
//! # 错误码（Dart 侧按它分支）
//!
//! | code | 含义 | 用户看到 |
//! |---|---|---|
//! | `BAD_INPUT` | 空参数 / 非 UTF-8 / 空 HTML / 参数非法（记忆类型写错、正文超 64 KiB…） | "内容为空，请重新抓取" |
//! | `PARSE_FAILED` | 结构性解析失败（找不到表/表头/列数错位） | "学校页面可能已更新，请反馈" |
//! | `PARSE_EMPTY` | 解析成功但一门课都没有（**不落库**） | "未解析到课程，请确认是否已排课" |
//! | `STORAGE` | SQLite 读写失败（课表 / 本地记忆） | "本地数据保存失败" |
//! | `TERM_START_UNKNOWN` | 学期开学日未确认/无法识别（**绝不猜默认日期**） | "请先确认本学期开学日期" |
//! | `INTERNAL` | 兜底 | "内部错误" |

use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::error::CoreError;
use crate::memory_items::{MemoryDraft, MemoryItemStore, MemoryKind};
use crate::parse::{ParseError, ParseReport};
use crate::schedule::{merge, SchedulePayload};
use crate::store::CourseStore;

/// 成功信封。
fn ok_json(data: Value) -> Value {
    json!({ "ok": true, "data": data })
}

/// 失败信封。
fn err_json(code: &str, message: impl Into<String>) -> Value {
    json!({ "ok": false, "code": code, "message": message.into() })
}

/// 把 JSON 变成 C 字符串（所有权交给调用方）。
fn into_c_string(value: &Value) -> *mut c_char {
    // 序列化失败只可能是 Value 里有非有限浮点，我们的信封不会有；
    // 真失败也要给调用方一个可解析的失败信封，而不是空指针。
    let text = serde_json::to_string(value)
        .unwrap_or_else(|_| r#"{"ok":false,"code":"INTERNAL","message":"序列化失败"}"#.to_owned());
    // 内容里不会有 NUL（JSON 序列化会转义），但防御性处理一次。
    let sanitized = text.replace('\0', "");
    match CString::new(sanitized) {
        Ok(cstr) => cstr.into_raw(),
        // **绝不返回 NULL**（Dart 侧拿到 NULL 就是段错误）：退回一个必然合法的失败信封。
        // 这里的字面量不含 NUL，`CString::new` 不会失败。
        Err(_) => CString::new(r#"{"ok":false,"code":"INTERNAL","message":"序列化失败"}"#)
            .expect("常量信封不可能包含 NUL")
            .into_raw(),
    }
}

/// 执行一段逻辑并把结果/错误包成信封再转 C 字符串。
///
/// **每个导出函数都必须经过这里**：任何 panic 都在这里被兜住——跨 FFI 的 panic 是 UB。
fn respond(body: impl FnOnce() -> Value) -> *mut c_char {
    let value = catch_unwind(AssertUnwindSafe(body))
        .unwrap_or_else(|_| err_json("INTERNAL", "内部错误：解析或存储时发生 panic"));
    into_c_string(&value)
}

/// 读一个 C 字符串入参。
///
/// # Safety
///
/// 调用方保证指针要么是 null，要么指向以 NUL 结尾、本函数调用期间有效的 UTF-8 字符串。
unsafe fn read_arg<'a>(ptr: *const c_char, name: &str) -> Result<&'a str, String> {
    if ptr.is_null() {
        return Err(format!("{name} 是空指针"));
    }
    let cstr = CStr::from_ptr(ptr);
    cstr.to_str()
        .map_err(|err| format!("{name} 不是 UTF-8：{err}"))
}

/// 把内部错误映射成 (code, message)。
fn map_core_error(err: CoreError) -> (String, String) {
    match err {
        CoreError::Invalid(message) | CoreError::Storage(message) => {
            ("STORAGE".to_owned(), message)
        }
        // 单独一个码：Dart 侧要能把用户直接引到"确认开学日"这一步，
        // 混进 STORAGE 会显示成"本地课表保存失败"，把人带偏。
        CoreError::TermStartUnknown(message) => ("TERM_START_UNKNOWN".to_owned(), message),
        other => ("INTERNAL".to_owned(), other.to_string()),
    }
}

/// 把解析错误映射成 (code, message)。**文案直接面向用户**。
fn map_parse_error(err: ParseError) -> (String, String) {
    ("PARSE_FAILED".to_owned(), err.to_string())
}

/// 解析课表 HTML 并落库。
///
/// 返回 `{"ok":true,"data":{"term":"…","imported":N,"warnings":[…]}}`。
///
/// # Safety
///
/// 三个参数都必须是有效的 NUL 结尾 UTF-8 字符串（见 [`read_arg`]）。
#[no_mangle]
pub unsafe extern "C" fn yunong_import_timetable(
    html: *const c_char,
    term: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (html, term, db_path) = match (
            read_arg(html, "html"),
            read_arg(term, "term"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(html), Ok(term), Ok(db_path)) => (html, term, db_path),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                return err_json("BAD_INPUT", err)
            }
        };
        if html.trim().is_empty() {
            return err_json("BAD_INPUT", "内容为空，请重新抓取");
        }
        if term.trim().is_empty() {
            return err_json("BAD_INPUT", "学期不能为空");
        }

        // 1) 解析（结构性错误 → PARSE_FAILED，绝不返回空表冒充成功）
        let report = match crate::parse::parse_timetable_with_report(html) {
            Ok(report) => report,
            Err(err) => {
                let (code, message) = map_parse_error(err);
                return err_json(&code, message);
            }
        };
        // 2) 空结果**不落库**（否则等于把旧课表清空）
        if report.courses.is_empty() {
            return err_json(
                "PARSE_EMPTY",
                "未解析到课程：请确认本学期是否已排课，或学校页面是否改版",
            );
        }
        // 3) 落库
        let mut store = match CourseStore::open(db_path) {
            Ok(store) => store,
            Err(err) => {
                let (code, message) = map_core_error(err);
                return err_json(&code, message);
            }
        };
        match store.import_report(term, &report) {
            Ok(outcome) => ok_json(json!({
                "term": outcome.term,
                "imported": outcome.imported,
                "warnings": outcome.warnings,
            })),
            Err(err) => {
                let (code, message) = map_core_error(err);
                err_json(&code, message)
            }
        }
    })
}

/// 查某学期的课表。
///
/// 返回 `{"ok":true,"data":{"term":"…","courses":[…]}}`。
///
/// # Safety
///
/// 入参必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_list_courses(
    term: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (term, db_path) = match (read_arg(term, "term"), read_arg(db_path, "db_path")) {
            (Ok(term), Ok(db_path)) => (term, db_path),
            (Err(err), _) | (_, Err(err)) => return err_json("BAD_INPUT", err),
        };
        let store = match CourseStore::open(db_path) {
            Ok(store) => store,
            Err(err) => {
                let (code, message) = map_core_error(err);
                return err_json(&code, message);
            }
        };
        match store.list_courses(term) {
            Ok(courses) => ok_json(json!({ "term": term, "courses": courses })),
            Err(err) => {
                let (code, message) = map_core_error(err);
                err_json(&code, message)
            }
        }
    })
}

/// 列出所有学期（UI 的学期下拉）。
///
/// # Safety
///
/// 入参必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_list_terms(db_path: *const c_char) -> *mut c_char {
    respond(|| {
        let db_path = match read_arg(db_path, "db_path") {
            Ok(path) => path,
            Err(err) => return err_json("BAD_INPUT", err),
        };
        let store = match CourseStore::open(db_path) {
            Ok(store) => store,
            Err(err) => {
                let (code, message) = map_core_error(err);
                return err_json(&code, message);
            }
        };
        match store.list_terms() {
            Ok(terms) => ok_json(json!({ "terms": terms })),
            Err(err) => {
                let (code, message) = map_core_error(err);
                err_json(&code, message)
            }
        }
    })
}

/// 门户课表接口的 JSON 入库（W6 新形态，保留 HTML 路径作兜底）。
///
/// 入参 `json` 形如：
/// ```json
/// {"weeks":[{"week":1,"data":{...portal data...}}],
///  "missing_weeks":[3,4],"weekcount":20}
/// ```
/// 合并规则（相邻节次、周集合压缩）在 [`crate::schedule::merge`] 里，这里只负责
/// 解析 → 落库 → 回报。部分周失败**不整体失败**：已成功的周照常入库，
/// 结果里给 `partial:true` 与 `missing_weeks`。
///
/// # Safety
///
/// 三个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_import_schedule_json(
    json: *const c_char,
    term: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (json, term, db_path) = match (
            read_arg(json, "json"),
            read_arg(term, "term"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(json), Ok(term), Ok(db_path)) => (json, term, db_path),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                return err_json("BAD_INPUT", err)
            }
        };
        if json.trim().is_empty() {
            return err_json("BAD_INPUT", "内容为空，请重新抓取");
        }
        if term.trim().is_empty() {
            return err_json("BAD_INPUT", "学期不能为空");
        }

        let payload: SchedulePayload = match serde_json::from_str(json) {
            Ok(payload) => payload,
            Err(err) => {
                return err_json("BAD_INPUT", format!("课表 JSON 解析失败：{err}"));
            }
        };
        let merged = merge(&payload);
        // 合并出 0 门课：不落库（否则等于把旧课表清空）
        if merged.courses.is_empty() {
            return err_json(
                "PARSE_EMPTY",
                "未解析到课程：请确认本学期是否已排课，或接口返回是否为空",
            );
        }

        let mut store = match CourseStore::open(db_path) {
            Ok(store) => store,
            Err(err) => {
                let (code, message) = map_core_error(err);
                return err_json(&code, message);
            }
        };
        let report = ParseReport {
            courses: merged.courses.clone(),
            warnings: merged.warnings.clone(),
        };
        match store.import_report(term, &report) {
            Ok(outcome) => ok_json(json!({
                "term": outcome.term,
                "imported": outcome.imported,
                "warnings": outcome.warnings,
                "weeks_seen": merged.weeks_seen,
                "missing_weeks": merged.missing_weeks,
                "partial": merged.is_partial(),
                "weekcount": merged.weekcount,
            })),
            Err(err) => {
                let (code, message) = map_core_error(err);
                err_json(&code, message)
            }
        }
    })
}
/// 设置/修正学期开始日（第 1 周周一），持久化到 `terms.starts_on`。
///
/// 为什么要单独一个入口：学期的真实开学日**无法从接口可靠推断**（`years` 是不透明标识，
/// `zs` 只说明"这次响应是第几周"）。端侧只能"按当前周推算一个候选值 → 让用户确认/修正 →
/// 持久化"，之后日历换算都用它。推算错一周，整张课表的日期就全错一周，所以宁可让用户多点一下。
///
/// 入参 `starts_on` 必须是 `YYYY-MM-DD`；空串表示清空（回到"未知"）。
///
/// # Safety
///
/// 三个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_set_term_start(
    term: *const c_char,
    starts_on: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (term, starts_on, db_path) = match (
            read_arg(term, "term"),
            read_arg(starts_on, "starts_on"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(term), Ok(starts_on), Ok(db_path)) => (term, starts_on, db_path),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                return err_json("BAD_INPUT", err)
            }
        };
        if term.trim().is_empty() {
            return err_json("BAD_INPUT", "学期不能为空");
        }
        let starts_on = starts_on.trim();
        if !starts_on.is_empty() && !is_iso_date(starts_on) {
            return err_json("BAD_INPUT", "开学日期格式必须是 YYYY-MM-DD");
        }
        let mut store = match CourseStore::open(db_path) {
            Ok(store) => store,
            Err(err) => {
                let (code, message) = map_core_error(err);
                return err_json(&code, message);
            }
        };
        // 空串 = 清空（`upsert_term` 对空串的处理是"保留旧值"，所以清空要直接写库）
        let result = if starts_on.is_empty() {
            store.upsert_term(term, Some(""), None)
        } else {
            store.upsert_term(term, Some(starts_on), None)
        };
        match result {
            Ok(()) => ok_json(json!({ "term": term, "starts_on": starts_on })),
            Err(err) => {
                let (code, message) = map_core_error(err);
                err_json(&code, message)
            }
        }
    })
}

/// 粗略校验 `YYYY-MM-DD`（只做形状 + 月日范围，不做日历校验）。
fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let year = value[0..4].parse::<i32>();
    let month = value[5..7].parse::<u32>();
    let day = value[8..10].parse::<u32>();
    match (year, month, day) {
        (Ok(year), Ok(month), Ok(day)) => {
            (1900..=2999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day)
        }
        _ => false,
    }
}

/// 把某学期的课表导出成 `.ics` 日历文件（W7）。
///
/// 返回 `{"ok":true,"data":{"path":"…","events":N}}`；`events` 是展开后的**独立事件数**
/// （= 课程 × 周次，见 `calendar::expand_course`），0 表示这个学期还没排课。
///
/// 学期开学日未确认 → `TERM_START_UNKNOWN`：**不生成文件**，也不用默认日期凑——
/// 猜错一周，整张课表的日期就全错。
///
/// # Safety
///
/// 三个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_export_ics(
    term: *const c_char,
    out_path: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (term, out_path, db_path) = match (
            read_arg(term, "term"),
            read_arg(out_path, "out_path"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(term), Ok(out_path), Ok(db_path)) => (term, out_path, db_path),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                return err_json("BAD_INPUT", err)
            }
        };
        if term.trim().is_empty() {
            return err_json("BAD_INPUT", "学期不能为空");
        }
        if out_path.trim().is_empty() {
            return err_json("BAD_INPUT", "导出路径不能为空");
        }
        match crate::ics::export_term_ics(term, db_path, out_path) {
            Ok(report) => ok_json(json!({
                "path": report.path.to_string_lossy(),
                "events": report.events,
            })),
            Err(err) => {
                let (code, message) = map_core_error(err);
                err_json(&code, message)
            }
        }
    })
}

// -------------------------------------------------------------------------
// 本地记忆（W7-3）：`memory_items` 的增删查。正文只在本机，不上云（ADR-02）。
// -------------------------------------------------------------------------

/// 记忆相关的错误映射：**校验类**问题统一 `BAD_INPUT`。
///
/// 共享的 [`map_core_error`] 把 `Invalid` 归到 `STORAGE`（课表那几个函数的历史行为，
/// 改动它等于改既有契约）。但"记忆类型写错"不是存储故障：界面若显示
/// "本地记忆保存失败"，用户会去查磁盘而不是改输入。所以这里只给记忆这三个函数
/// 单独把 `Invalid` 映射成 `BAD_INPUT`。
fn memory_error(err: CoreError) -> Value {
    match err {
        CoreError::Invalid(message) => err_json("BAD_INPUT", message),
        other => {
            let (code, message) = map_core_error(other);
            err_json(&code, message)
        }
    }
}

/// 解析可选的 id（空串 = 新增一条）。
fn parse_optional_id(text: &str) -> Result<Option<i64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<i64>() {
        Ok(id) if id > 0 => Ok(Some(id)),
        _ => Err(format!("id 必须是正整数：{text}")),
    }
}

/// 解析 `list` 的条数（空串 = 默认 [`crate::memory_items::DEFAULT_LIST_LIMIT`]）。
fn parse_list_limit(text: &str) -> Result<usize, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(crate::memory_items::DEFAULT_LIST_LIMIT);
    }
    match text.parse::<usize>() {
        Ok(limit) if (1..=crate::memory_items::DEFAULT_MAX_ITEMS).contains(&limit) => Ok(limit),
        _ => Err(format!(
            "limit 必须是 1..={} 的整数：{text}",
            crate::memory_items::DEFAULT_MAX_ITEMS
        )),
    }
}

/// 写一条本地记忆（新增或更新）。
///
/// - `kind`：`note` / `summary` / `fact`（其它值 → `BAD_INPUT`）；
/// - `id_or_empty`：空串 = 新增，非空 = 更新该 id（不存在 → `BAD_INPUT`，不会静默新增）；
/// - 返回 `{"ok":true,"data":{"id":12,"evicted":0}}`，`evicted` = 为守容量删掉的行数。
///
/// 单条正文超过 64 KiB → `BAD_INPUT`（不静默截断：截断等于丢用户内容）。
///
/// # Safety
///
/// 六个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_memory_upsert(
    kind: *const c_char,
    title: *const c_char,
    content: *const c_char,
    source: *const c_char,
    id_or_empty: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (kind, title, content, source, id_or_empty, db_path) = match (
            read_arg(kind, "kind"),
            read_arg(title, "title"),
            read_arg(content, "content"),
            read_arg(source, "source"),
            read_arg(id_or_empty, "id"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(kind), Ok(title), Ok(content), Ok(source), Ok(id), Ok(db)) => {
                (kind, title, content, source, id, db)
            }
            (Err(err), ..)
            | (_, Err(err), ..)
            | (_, _, Err(err), ..)
            | (_, _, _, Err(err), ..)
            | (_, _, _, _, Err(err), _)
            | (_, _, _, _, _, Err(err)) => return err_json("BAD_INPUT", err),
        };
        let kind = match MemoryKind::parse(kind) {
            Ok(kind) => kind,
            Err(err) => return memory_error(err),
        };
        let id = match parse_optional_id(id_or_empty) {
            Ok(id) => id,
            Err(err) => return err_json("BAD_INPUT", err),
        };
        let mut store = match MemoryItemStore::open(db_path) {
            Ok(store) => store,
            Err(err) => return memory_error(err),
        };
        let draft = MemoryDraft {
            kind,
            title: Some(title),
            content,
            source: Some(source),
            expires_at: None,
        };
        match store.upsert(&draft, id) {
            Ok(write) => ok_json(json!({ "id": write.id, "evicted": write.evicted })),
            Err(err) => memory_error(err),
        }
    })
}

/// 列出本地记忆（按 `updated_at` 倒序）。
///
/// - `kind_or_empty`：空串 = 全部，否则必须是 `note` / `summary` / `fact`；
/// - `limit`：空串 = 默认 100，显式值必须落在 `1..=5000`；
/// - 返回 `{"ok":true,"data":[{...}, ...]}`（到期的条目不会出现）。
///
/// # Safety
///
/// 三个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_memory_list(
    kind_or_empty: *const c_char,
    limit: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (kind_or_empty, limit, db_path) = match (
            read_arg(kind_or_empty, "kind"),
            read_arg(limit, "limit"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(kind), Ok(limit), Ok(db_path)) => (kind, limit, db_path),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                return err_json("BAD_INPUT", err)
            }
        };
        let kind = match kind_or_empty.trim() {
            "" => None,
            text => match MemoryKind::parse(text) {
                Ok(kind) => Some(kind),
                Err(err) => return memory_error(err),
            },
        };
        let limit = match parse_list_limit(limit) {
            Ok(limit) => limit,
            Err(err) => return err_json("BAD_INPUT", err),
        };
        let store = match MemoryItemStore::open(db_path) {
            Ok(store) => store,
            Err(err) => return memory_error(err),
        };
        match store.list(kind, limit) {
            Ok(items) => ok_json(json!(items)),
            Err(err) => memory_error(err),
        }
    })
}

/// 删除一条本地记忆。
///
/// 返回 `{"ok":true,"data":{"deleted":1}}`；删除是幂等的，删不存在的 id 返回 0
/// 而不是报错（界面重复点击不该弹错）。
///
/// # Safety
///
/// 两个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_memory_delete(
    id: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (id, db_path) = match (read_arg(id, "id"), read_arg(db_path, "db_path")) {
            (Ok(id), Ok(db_path)) => (id, db_path),
            (Err(err), _) | (_, Err(err)) => return err_json("BAD_INPUT", err),
        };
        // 空 id 当 0 处理会"静默什么也没删"，比报错更难排查
        let id = match parse_optional_id(id) {
            Ok(Some(id)) => id,
            Ok(None) => return err_json("BAD_INPUT", "id 不能为空"),
            Err(err) => return err_json("BAD_INPUT", err),
        };
        let mut store = match MemoryItemStore::open(db_path) {
            Ok(store) => store,
            Err(err) => return memory_error(err),
        };
        match store.delete(id) {
            Ok(deleted) => ok_json(json!({ "deleted": deleted })),
            Err(err) => memory_error(err),
        }
    })
}

// ---------------------------------------------------------------------------
// 流式对话（W7 P0）：回调式 FFI
// ---------------------------------------------------------------------------

/// 流式回调：`chunk_json` **只在本次回调期间有效**（Rust 分配、Rust 释放，Dart 侧绝不能 free）；
/// 返回 `1` = 调用方要求取消（停止读取上游），`0` = 继续。
pub type ChatChunkCallback =
    unsafe extern "C" fn(chunk_json: *const c_char, user: *mut c_void) -> i32;

static CHAT_CANCEL: OnceLock<crate::chat_ffi::CancelRegistry> = OnceLock::new();

fn chat_cancel_registry() -> &'static crate::chat_ffi::CancelRegistry {
    CHAT_CANCEL.get_or_init(crate::chat_ffi::CancelRegistry::new)
}

/// 原始回调指针 + user data 的 `Send` 包装。
///
/// 安全前提：`user` 由调用方保证在流结束前有效（Dart 侧是 `NativeCallable.listener` 的
/// 端口 id，跨线程投递是安全的）；回调本身必须线程安全。
struct ChatCallback {
    on_chunk: ChatChunkCallback,
    user: *mut c_void,
}

#[allow(unsafe_code)]
unsafe impl Send for ChatCallback {}

impl ChatCallback {
    fn emit(&self, json: &str) -> bool {
        let Ok(text) = CString::new(json.replace('\0', "")) else {
            return true;
        };
        // catch_unwind：Dart 侧回调抛异常不能把 Rust 进程带崩
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            (self.on_chunk)(text.as_ptr(), self.user)
        }));
        // 字符串 `text` 在本次调用期间有效，返回后即释放（Dart 侧不得保存指针）
        drop(text);
        match result {
            Ok(value) => value == 0,
            Err(_) => false,
        }
    }
}

/// 发起一次流式对话，立刻返回 handle（>0）。每个 Chunk 通过 `on_chunk` 回调交付；
/// 流内的错误也以 `Chunk::Error` 回调，**不会**用返回值/异常表达。
///
/// 返回 `0` 表示入参无效（空指针 / 空 JSON）；此时不会启动线程。
///
/// # Safety
///
/// - `request_json` / `db_path`：NUL 结尾 UTF-8 字符串，调用期间有效；
/// - `on_chunk`：必须是非空的、线程安全的函数指针；
/// - `user`：原样回传给回调，生命周期由调用方保证。
#[no_mangle]
pub unsafe extern "C" fn yunong_chat_stream(
    request_json: *const c_char,
    _db_path: *const c_char,
    on_chunk: Option<ChatChunkCallback>,
    user: *mut c_void,
) -> i64 {
    let Some(callback) = on_chunk else {
        return 0;
    };
    let Ok(request) = read_arg(request_json, "request_json") else {
        return 0;
    };
    let request = request.to_owned();
    let handle = chat_cancel_registry().allocate() as i64;
    let callback = ChatCallback {
        on_chunk: callback,
        user,
    };

    std::thread::spawn(move || {
        let registry = chat_cancel_registry();
        let cancelled = || registry.is_cancelled(handle as u64);
        let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let parsed = match crate::chat_ffi::ChatRunConfig::parse(&request) {
                Ok(config) => config,
                Err((code, message)) => {
                    callback.emit(&crate::chat_ffi::error_chunk_json(code, message));
                    return;
                }
            };
            let result = crate::chat_ffi::run_stream(parsed, |json| {
                if cancelled() {
                    return false;
                }
                callback.emit(&json)
            });
            if let Err((code, message)) = result {
                callback.emit(&crate::chat_ffi::error_chunk_json(code, message));
            }
        }));
        if body.is_err() {
            callback.emit(&crate::chat_ffi::error_chunk_json(
                yunong_shared::ErrorCode::Internal,
                "内部错误：流式对话线程发生 panic",
            ));
        }
        registry.release(handle as u64);
    });

    handle
}

/// 取消某条流（handle 由 [`yunong_chat_stream`] 返回）。
///
/// 可重复调用；流已结束时返回 `1`（幂等，不算错）。
///
/// # Safety
///
/// 无指针参数，任何线程调用都安全。
#[no_mangle]
pub extern "C" fn yunong_chat_cancel(handle: i64) -> i32 {
    if handle <= 0 {
        return 1;
    }
    chat_cancel_registry().cancel(handle as u64);
    1
}
// ---------------------------------------------------------------------------
// 凭据安全存储（W7 P0）：明文永不出 Rust
// ---------------------------------------------------------------------------

fn secret_store_for(db_path: &str) -> Box<dyn crate::secret::KeyStore> {
    let base = std::path::Path::new(db_path)
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    crate::secret::default_store(base)
}

fn secret_error_json(err: crate::secret::SecretError) -> Value {
    match err {
        crate::secret::SecretError::InvalidKey(message)
        | crate::secret::SecretError::InvalidValue(message) => err_json("BAD_INPUT", message),
        crate::secret::SecretError::Storage(message) => err_json("STORAGE", message),
    }
}

fn secret_status_json(status: crate::secret::SecretStatus) -> Value {
    ok_json(json!({
        "set": status.set,
        "masked": status.masked,
        "degraded": status.degraded,
        "reason": status.reason,
    }))
}

/// 写入一条凭据（BYOK API Key 等）。返回值**只有掩码**，不含明文。
///
/// 平台安全存储不可用（DPAPI 失败 / 非 Windows）时返回 `degraded:true` +
/// `reason`：本次仅内存生效，**绝不落明文文件**。
///
/// # Safety
///
/// 三个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_set_secret(
    key: *const c_char,
    value: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (key, value, db_path) = match (
            read_arg(key, "key"),
            read_arg(value, "value"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(key), Ok(value), Ok(db_path)) => (key, value, db_path),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                return err_json("BAD_INPUT", err)
            }
        };
        let store = secret_store_for(db_path);
        match store.set(key, value) {
            Ok(status) => secret_status_json(status),
            Err(err) => secret_error_json(err),
        }
    })
}

/// 查询凭据状态：**只返回 `{set, masked, degraded, reason}`，任何情况下都没有明文**。
///
/// # Safety
///
/// 两个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_get_secret(
    key: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (key, db_path) = match (read_arg(key, "key"), read_arg(db_path, "db_path")) {
            (Ok(key), Ok(db_path)) => (key, db_path),
            (Err(err), _) | (_, Err(err)) => return err_json("BAD_INPUT", err),
        };
        let store = secret_store_for(db_path);
        match store.status(key) {
            Ok(status) => secret_status_json(status),
            Err(err) => secret_error_json(err),
        }
    })
}

/// 删除一条凭据；`deleted` 表示之前是否存在（幂等）。
///
/// # Safety
///
/// 两个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_delete_secret(
    key: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (key, db_path) = match (read_arg(key, "key"), read_arg(db_path, "db_path")) {
            (Ok(key), Ok(db_path)) => (key, db_path),
            (Err(err), _) | (_, Err(err)) => return err_json("BAD_INPUT", err),
        };
        let store = secret_store_for(db_path);
        match store.delete(key) {
            Ok(deleted) => ok_json(json!({ "deleted": deleted })),
            Err(err) => secret_error_json(err),
        }
    })
}
/// 释放本模块返回的字符串。传 null 是安全的空操作。
///
/// # Safety
///
/// `ptr` 必须来自本模块的某个函数返回值，且只能释放一次。
#[no_mangle]
pub unsafe extern "C" fn yunong_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    drop(CString::from_raw(ptr));
}

/// 版本号（用于 Dart 侧确认 DLL 与 app 匹配）。返回静态字符串，**不要** free。
#[no_mangle]
pub extern "C" fn yunong_core_version() -> *const c_char {
    // 静态常量，生命周期是 'static；用 CStr 保证 NUL 结尾。
    static VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    VERSION.as_ptr().cast::<c_char>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份结构完整的最小课表页（与 `parse::tests` 的 fixture 同构）。
    fn sample_page() -> String {
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
        for _ in 1..7 {
            page.push_str("<td></td>");
        }
        page.push_str("</tr></table></body></html>");
        page
    }

    fn call_import(html: &str, term: &str, db: &str) -> Value {
        let html = CString::new(html).unwrap();
        let term = CString::new(term).unwrap();
        let db = CString::new(db).unwrap();
        let raw = unsafe { yunong_import_timetable(html.as_ptr(), term.as_ptr(), db.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    fn call_list(term: &str, db: &str) -> Value {
        let term = CString::new(term).unwrap();
        let db = CString::new(db).unwrap();
        let raw = unsafe { yunong_list_courses(term.as_ptr(), db.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    fn temp_db() -> String {
        let dir = std::env::temp_dir().join(format!("yunong-ffi-{}", uuid::Uuid::new_v4()));
        dir.join("timetable.db").to_string_lossy().into_owned()
    }

    #[test]
    fn import_then_query_via_ffi() {
        let db = temp_db();
        let result = call_import(&sample_page(), "2026-2027-1", &db);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["data"]["imported"], 1);
        assert_eq!(result["data"]["term"], "2026-2027-1");
        assert!(result["data"]["warnings"].is_array());

        let listed = call_list("2026-2027-1", &db);
        assert_eq!(listed["ok"], true, "{listed}");
        let courses = listed["data"]["courses"].as_array().unwrap();
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0]["name"], "高等数学A");
        assert_eq!(courses[0]["weekday"], 1);
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn revamped_page_fails_loudly_with_user_message() {
        let db = temp_db();
        let result = call_import("<html><body>登录页</body></html>", "t1", &db);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "PARSE_FAILED");
        // 断言**错误码**（稳定契约），不断言文案：文案会随实现调整，且 CI 与本地时序不同，
        // 锁死文案的测试迟早会红（t2 的教训）。用户可见的固定文案在 Dart 侧 `describeFailure`。
        let message = result["message"].as_str().unwrap();
        assert!(!message.trim().is_empty(), "失败必须带可读的 message");
        // 失败时不能建库/落空表
        let listed = call_list("t1", &db);
        assert_eq!(listed["ok"], true);
        assert_eq!(listed["data"]["courses"].as_array().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn null_and_empty_inputs_are_rejected_without_crashing() {
        let db = CString::new(temp_db()).unwrap();
        // null html
        let raw = unsafe { yunong_import_timetable(std::ptr::null(), db.as_ptr(), db.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");

        // 空白 HTML
        let empty = CString::new("   ").unwrap();
        let term = CString::new("t1").unwrap();
        let raw = unsafe { yunong_import_timetable(empty.as_ptr(), term.as_ptr(), db.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");
    }

    #[test]
    fn version_pointer_is_stable_and_not_freed() {
        let ptr = yunong_core_version();
        assert!(!ptr.is_null());
        let version = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        assert!(!version.is_empty());
    }

    #[test]
    fn empty_parse_is_not_written_to_the_store() {
        let db = temp_db();
        // 有表头、但没有课：解析层返回 Err(Empty)
        let mut page = String::from(
            "<table id=\"timetable\"><tr><th>节次</th>\
             <th>星期一</th><th>星期二</th><th>星期三</th><th>星期四</th>\
             <th>星期五</th><th>星期六</th><th>星期日</th></tr></table>",
        );
        page.push_str("");
        let result = call_import(&page, "t1", &db);
        assert_eq!(result["ok"], false, "{result}");
        assert!(
            result["code"] == "PARSE_FAILED" || result["code"] == "PARSE_EMPTY",
            "{result}"
        );
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn panics_are_caught_and_become_error_envelopes() {
        // 所有导出函数都走 `respond`：这里直接验证"闭包里 panic → 返回失败信封、进程不死"。
        let raw = respond(|| unreachable!("模拟解析器里的不可达分支"));
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).expect("兜底信封也必须是合法 JSON");
        assert_eq!(value["ok"], false, "{value}");
        assert_eq!(value["code"], "INTERNAL", "{value}");
    }

    #[test]
    fn json_payload_has_no_raw_newline_or_nul() {
        // Dart 侧按行/按包处理字符串时，裸换行会出问题；NUL 会截断 C 字符串。
        let raw = respond(|| err_json("BAD_INPUT", "第一行\n第二行\0尾巴"));
        let bytes = unsafe { CStr::from_ptr(raw) }.to_bytes().to_vec();
        unsafe { yunong_free_string(raw) };
        let text = String::from_utf8(bytes).expect("必须是 UTF-8");
        assert!(!text.contains('\n'), "JSON 里不能有裸换行：{text}");
        assert!(!text.contains('\0'), "C 字符串里不能有 NUL：{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["ok"], false);
    }

    #[test]
    fn list_terms_via_ffi() {
        let db = temp_db();
        assert_eq!(call_import(&sample_page(), "2026-2027-1", &db)["ok"], true);
        let db_c = CString::new(db.clone()).unwrap();
        let raw = unsafe { yunong_list_terms(db_c.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["ok"], true, "{value}");
        let terms = value["data"]["terms"].as_array().unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0]["term"], "2026-2027-1");
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn free_string_on_null_is_a_noop() {
        unsafe { yunong_free_string(std::ptr::null_mut()) };
    }
    // ---------------------------------------------------------------------
    // 门户课表 JSON 入库（W6）
    // ---------------------------------------------------------------------

    /// 造一份一周的门户 data：周一第 1-2 节一门课。
    fn weekly_json(week_no: u32) -> String {
        let mut days: Vec<Vec<String>> = vec![vec![String::new(); 10]; 7];
        days[0][0] = "高等数学I,东校区1教305,班级;,张老师".to_owned();
        days[0][1] = "高等数学I,东校区1教305,班级;,张老师".to_owned();
        serde_json::json!({
            "weeks": [{ "week": week_no, "data": { "resultsJsonArr": days, "weekcount": "20" } }],
            "weekcount": 20
        })
        .to_string()
    }

    fn call_schedule(json: &str, term: &str, db: &str) -> Value {
        let json = CString::new(json).unwrap();
        let term = CString::new(term).unwrap();
        let db = CString::new(db).unwrap();
        let raw = unsafe { yunong_import_schedule_json(json.as_ptr(), term.as_ptr(), db.as_ptr()) };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn import_schedule_json_then_query() {
        let db = temp_db();
        let result = call_schedule(&weekly_json(1), "2026-2027-1", &db);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["data"]["imported"], 1);
        assert_eq!(result["data"]["partial"], false);
        assert_eq!(result["data"]["weeks_seen"][0], 1);

        let listed = call_list("2026-2027-1", &db);
        let courses = listed["data"]["courses"].as_array().unwrap();
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0]["name"], "高等数学I");
        assert_eq!(courses[0]["weekday"], 1);
        assert_eq!(courses[0]["start_period"], 1);
        assert_eq!(courses[0]["end_period"], 2);
        assert_eq!(courses[0]["weeks"], "1");
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn partial_weeks_import_with_flag() {
        let db = temp_db();
        let payload: Value = serde_json::from_str(&weekly_json(1)).unwrap();
        let mut payload = payload;
        payload["missing_weeks"] = serde_json::json!([2, 3]);
        let result = call_schedule(&payload.to_string(), "2026-2027-1", &db);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["data"]["partial"], true, "{result}");
        assert_eq!(result["data"]["missing_weeks"], serde_json::json!([2, 3]));
        assert_eq!(result["data"]["imported"], 1);
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn empty_schedule_json_never_wipes_existing_courses() {
        let db = temp_db();
        assert_eq!(
            call_schedule(&weekly_json(1), "2026-2027-1", &db)["ok"],
            true
        );

        let empty = serde_json::json!({
            "weeks": [{ "week": 1, "data": { "resultsJsonArr": vec![vec![String::new(); 10]; 7] } }]
        })
        .to_string();
        let result = call_schedule(&empty, "2026-2027-1", &db);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "PARSE_EMPTY");

        let listed = call_list("2026-2027-1", &db);
        assert_eq!(listed["data"]["courses"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn reimport_schedule_replaces_instead_of_duplicating() {
        let db = temp_db();
        call_schedule(&weekly_json(1), "2026-2027-1", &db);
        call_schedule(&weekly_json(1), "2026-2027-1", &db);
        let listed = call_list("2026-2027-1", &db);
        assert_eq!(
            listed["data"]["courses"].as_array().unwrap().len(),
            1,
            "重抓必须替换，不能翻倍"
        );
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn malformed_schedule_json_is_bad_input_not_a_crash() {
        let db = temp_db();
        let result = call_schedule("这不是 JSON", "2026-2027-1", &db);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "BAD_INPUT");
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }
    // ---------------------------------------------------------------------
    // 学期开始日（W6：不硬编码，用户确认后持久化）
    // ---------------------------------------------------------------------

    fn call_set_term_start(term: &str, starts_on: &str, db: &str) -> Value {
        let term = CString::new(term).unwrap();
        let starts_on = CString::new(starts_on).unwrap();
        let db = CString::new(db).unwrap();
        let raw = unsafe { yunong_set_term_start(term.as_ptr(), starts_on.as_ptr(), db.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn set_term_start_persists_and_is_readable_via_list_terms() {
        let db = temp_db();
        // 先有学期（导入或随便建一条），再设置开学日
        assert_eq!(
            call_schedule(&weekly_json(1), "2026-2027-1", &db)["ok"],
            true
        );
        let result = call_set_term_start("2026-2027-1", "2026-08-31", &db);
        assert_eq!(result["ok"], true, "{result}");

        let db_c = CString::new(db.clone()).unwrap();
        let raw = unsafe { yunong_list_terms(db_c.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        let terms = value["data"]["terms"].as_array().unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0]["starts_on"], "2026-08-31", "{value}");
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn set_term_start_rejects_bad_date_without_touching_the_store() {
        let db = temp_db();
        assert_eq!(
            call_schedule(&weekly_json(1), "2026-2027-1", &db)["ok"],
            true
        );
        for bad in [
            "2026-8-31",
            "2026/08/31",
            "20260831",
            "2026-13-01",
            "2026-08-32",
        ] {
            let result = call_set_term_start("2026-2027-1", bad, &db);
            assert_eq!(result["ok"], false, "{bad} => {result}");
            assert_eq!(result["code"], "BAD_INPUT", "{bad} => {result}");
        }
        let db_c = CString::new(db.clone()).unwrap();
        let raw = unsafe { yunong_list_terms(db_c.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["data"]["terms"][0]["starts_on"], "",
            "坏日期不能写库：{value}"
        );
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    // ---------------------------------------------------------------------
    // ICS 导出（W7-1）
    // ---------------------------------------------------------------------

    fn call_export(term: &str, out_path: &str, db: &str) -> Value {
        let term = CString::new(term).unwrap();
        let out_path = CString::new(out_path).unwrap();
        let db_c = CString::new(db).unwrap();
        let raw = unsafe { yunong_export_ics(term.as_ptr(), out_path.as_ptr(), db_c.as_ptr()) };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    /// 与 `temp_db()` 同一个临时目录下的 `.ics` 路径。
    fn temp_ics(db: &str) -> String {
        std::path::Path::new(db)
            .parent()
            .unwrap()
            .join("term.ics")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn export_ics_via_ffi_writes_an_all_day_event() {
        let db = temp_db();
        assert_eq!(
            call_schedule(&weekly_json(1), "2026-2027-1", &db)["ok"],
            true
        );
        assert_eq!(
            call_set_term_start("2026-2027-1", "2026-08-31", &db)["ok"],
            true
        );
        let out = temp_ics(&db);
        let result = call_export("2026-2027-1", &out, &db);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["data"]["events"], 1, "{result}");
        assert_eq!(result["data"]["path"], out.as_str(), "{result}");

        let text = std::fs::read_to_string(&out).expect("导出文件必须存在");
        assert!(text.starts_with("BEGIN:VCALENDAR\r\n"), "{text}");
        assert!(text.contains("DTSTART;VALUE=DATE:20260831"), "{text}");
        assert!(!text.contains("RRULE"), "{text}");
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn export_ics_without_confirmed_start_reports_code_and_writes_nothing() {
        let db = temp_db();
        // 只导入课表，不确认开学日
        assert_eq!(
            call_schedule(&weekly_json(1), "2026-2027-1", &db)["ok"],
            true
        );
        let out = temp_ics(&db);
        let result = call_export("2026-2027-1", &out, &db);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "TERM_START_UNKNOWN", "{result}");
        assert!(
            !std::path::Path::new(&out).exists(),
            "开学日未知时不能生成文件（更不能用默认日期凑一份）"
        );
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn export_ics_for_unknown_term_reports_term_start_unknown() {
        let db = temp_db();
        assert_eq!(
            call_schedule(&weekly_json(1), "2026-2027-1", &db)["ok"],
            true
        );
        let out = temp_ics(&db);
        let result = call_export("1999-2000-1", &out, &db);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "TERM_START_UNKNOWN", "{result}");
        assert!(!std::path::Path::new(&out).exists());
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn export_ics_rejects_empty_and_null_arguments() {
        let db = temp_db();
        let result = call_export("   ", &temp_ics(&db), &db);
        assert_eq!(result["code"], "BAD_INPUT", "{result}");

        let result = call_export("2026-2027-1", "  ", &db);
        assert_eq!(result["code"], "BAD_INPUT", "{result}");

        // null 指针同样只是失败信封，不崩
        let db_c = CString::new(db.clone()).unwrap();
        let raw = unsafe { yunong_export_ics(std::ptr::null(), std::ptr::null(), db_c.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    // ---------------------------------------------------------------------
    // 本地记忆（W7-3）
    // ---------------------------------------------------------------------

    fn call_memory_upsert(
        kind: &str,
        title: &str,
        content: &str,
        source: &str,
        id: &str,
        db: &str,
    ) -> Value {
        let kind = CString::new(kind).unwrap();
        let title = CString::new(title).unwrap();
        let content = CString::new(content).unwrap();
        let source = CString::new(source).unwrap();
        let id = CString::new(id).unwrap();
        let db_c = CString::new(db).unwrap();
        let raw = unsafe {
            yunong_memory_upsert(
                kind.as_ptr(),
                title.as_ptr(),
                content.as_ptr(),
                source.as_ptr(),
                id.as_ptr(),
                db_c.as_ptr(),
            )
        };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    fn call_memory_list(kind: &str, limit: &str, db: &str) -> Value {
        let kind = CString::new(kind).unwrap();
        let limit = CString::new(limit).unwrap();
        let db_c = CString::new(db).unwrap();
        let raw = unsafe { yunong_memory_list(kind.as_ptr(), limit.as_ptr(), db_c.as_ptr()) };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    fn call_memory_delete(id: &str, db: &str) -> Value {
        let id = CString::new(id).unwrap();
        let db_c = CString::new(db).unwrap();
        let raw = unsafe { yunong_memory_delete(id.as_ptr(), db_c.as_ptr()) };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn memory_roundtrip_via_ffi() {
        let db = temp_db();
        let created = call_memory_upsert("note", "第一节课", "周三记得带雨伞", "user", "", &db);
        assert_eq!(created["ok"], true, "{created}");
        let id = created["data"]["id"].as_i64().expect("必须返回 id");
        assert!(id > 0, "{created}");
        assert_eq!(created["data"]["evicted"], 0, "{created}");

        let all = call_memory_list("", "", &db);
        assert_eq!(all["ok"], true, "{all}");
        let items = all["data"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{all}");
        assert_eq!(items[0]["id"], id);
        assert_eq!(items[0]["kind"], "note");
        assert_eq!(items[0]["title"], "第一节课");
        assert_eq!(items[0]["content"], "周三记得带雨伞");
        assert_eq!(items[0]["source"], "user");
        assert!(items[0]["created_at"].as_i64().unwrap() > 0);
        assert!(items[0]["updated_at"].as_i64().unwrap() > 0);
        assert!(items[0]["expires_at"].is_null());

        // 按种类过滤
        assert_eq!(
            call_memory_list("note", "10", &db)["data"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            call_memory_list("summary", "10", &db)["data"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        // 带 id 再写 = 更新同一条（不新增）
        let id_text = id.to_string();
        let updated =
            call_memory_upsert("note", "第一节课（改）", "改带外套", "user", &id_text, &db);
        assert_eq!(updated["ok"], true, "{updated}");
        assert_eq!(updated["data"]["id"], id, "{updated}");
        let items = call_memory_list("", "", &db)["data"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(items.len(), 1, "更新不能多出一条");
        assert_eq!(items[0]["content"], "改带外套");
        assert_eq!(items[0]["title"], "第一节课（改）");

        // 删除幂等
        assert_eq!(call_memory_delete(&id_text, &db)["data"]["deleted"], 1);
        assert_eq!(call_memory_delete(&id_text, &db)["data"]["deleted"], 0);
        assert_eq!(
            call_memory_list("", "", &db)["data"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn memory_rejects_bad_input_with_bad_input_code() {
        let db = temp_db();
        // 类型不在白名单
        let result = call_memory_upsert("diary", "标题", "正文", "", "", &db);
        assert_eq!(result["code"], "BAD_INPUT", "{result}");
        // 空正文
        let result = call_memory_upsert("note", "标题", "   ", "", "", &db);
        assert_eq!(result["code"], "BAD_INPUT", "{result}");
        // 正文超 64 KiB：报错但不截断、不落库
        let huge = "x".repeat(64 * 1024 + 1);
        let result = call_memory_upsert("note", "标题", &huge, "", "", &db);
        assert_eq!(result["code"], "BAD_INPUT", "{result}");
        assert_eq!(
            call_memory_list("", "", &db)["data"]
                .as_array()
                .unwrap()
                .len(),
            0,
            "超限的那条不能落库"
        );
        // id 非法 / 更新不存在的 id
        let result = call_memory_upsert("note", "标题", "正文", "", "abc", &db);
        assert_eq!(result["code"], "BAD_INPUT", "{result}");
        let result = call_memory_upsert("note", "标题", "正文", "", "999", &db);
        assert_eq!(result["code"], "BAD_INPUT", "{result}");
        assert!(
            result["message"].as_str().unwrap().contains("不存在"),
            "{result}"
        );
        // 删除缺 id / id 非正数
        assert_eq!(call_memory_delete("", &db)["code"], "BAD_INPUT");
        assert_eq!(call_memory_delete("0", &db)["code"], "BAD_INPUT");
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn memory_list_limit_boundaries_via_ffi() {
        let db = temp_db();
        for index in 0..3 {
            let result = call_memory_upsert(
                "fact",
                &format!("事实 {index}"),
                &format!("第 {index} 条"),
                "user",
                "",
                &db,
            );
            assert_eq!(result["ok"], true, "{result}");
        }
        assert_eq!(
            call_memory_list("", "1", &db)["data"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            call_memory_list("", "3", &db)["data"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        // 空 limit = 默认条数
        assert_eq!(
            call_memory_list("", "", &db)["data"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        for bad in ["0", "abc", "-1", "5001"] {
            let result = call_memory_list("", bad, &db);
            assert_eq!(result["code"], "BAD_INPUT", "limit={bad} => {result}");
        }
        // 种类过滤也要校验
        let result = call_memory_list("diary", "10", &db);
        assert_eq!(result["code"], "BAD_INPUT", "{result}");
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn memory_null_arguments_do_not_crash() {
        let raw = unsafe {
            yunong_memory_upsert(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert!(!raw.is_null(), "绝不返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");

        let raw =
            unsafe { yunong_memory_list(std::ptr::null(), std::ptr::null(), std::ptr::null()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");

        let raw = unsafe { yunong_memory_delete(std::ptr::null(), std::ptr::null()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");
    }

    #[test]
    fn memory_and_timetable_share_the_same_database_file() {
        let db = temp_db();
        // 同一个库文件里既放课表也放记忆，互不干扰
        assert_eq!(
            call_schedule(&weekly_json(1), "2026-2027-1", &db)["ok"],
            true
        );
        let created = call_memory_upsert("fact", "班级", "我在 3 班", "user", "", &db);
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(
            call_list("2026-2027-1", &db)["data"]["courses"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "写记忆不能动课表"
        );
        assert_eq!(
            call_memory_list("", "", &db)["data"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }
    // ---------------------------------------------------------------------
    // 凭据 FFI（W7 P0）：明文永不出 Rust
    // ---------------------------------------------------------------------

    unsafe fn call_secret(json: *mut c_char) -> Value {
        let text = CStr::from_ptr(json).to_str().unwrap().to_owned();
        yunong_free_string(json);
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn secret_set_get_delete_never_leaks_plaintext() {
        let base = std::env::temp_dir().join(format!("yunong-ffi-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let db = CString::new(base.join("t.db").to_string_lossy().to_string()).unwrap();
        let key = CString::new("byok_api_key").unwrap();
        let plain = "sk-test-0123456789abcdef0123456789abcdef";
        let value = CString::new(plain).unwrap();

        unsafe {
            // set
            let response =
                call_secret(yunong_set_secret(key.as_ptr(), value.as_ptr(), db.as_ptr()));
            assert_eq!(response["ok"], true);
            let masked = response["data"]["masked"].as_str().unwrap().to_owned();
            assert_eq!(masked, "sk-****cdef");
            let raw = response.to_string();
            assert!(!raw.contains(plain), "set 返回里出现明文：{raw}");

            // get：只有 set + 掩码
            // 注意平台差异：非 Windows 没有系统安全存储，set 走降级（不落盘），
            // 因此 get 必然是 set:false + degraded:true。断言必须按平台分支，
            // 否则 Linux CI 上会红（此前就是这样挂的）。
            let response = call_secret(yunong_get_secret(key.as_ptr(), db.as_ptr()));
            assert_eq!(response["ok"], true);
            if cfg!(windows) {
                assert_eq!(response["data"]["set"], true);
                assert_eq!(response["data"]["masked"], "sk-****cdef");
            } else {
                assert_eq!(response["data"]["set"], false);
                assert_eq!(response["data"]["degraded"], true);
            }
            let raw = response.to_string();
            assert!(!raw.contains(plain), "get 返回里出现明文：{raw}");
            assert!(!raw.contains("0123456789abcdef"), "get 返回里出现明文片段：{raw}");

            // delete → get(set:false)；非 Windows 本来就没存，deleted 为 false
            let response = call_secret(yunong_delete_secret(key.as_ptr(), db.as_ptr()));
            assert_eq!(response["ok"], true);
            assert_eq!(response["data"]["deleted"], cfg!(windows));
            let response = call_secret(yunong_get_secret(key.as_ptr(), db.as_ptr()));
            assert_eq!(response["data"]["set"], false);
            // 再删一次是幂等
            let response = call_secret(yunong_delete_secret(key.as_ptr(), db.as_ptr()));
            assert_eq!(response["data"]["deleted"], false);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn secret_rejects_bad_inputs() {
        let base = std::env::temp_dir().join(format!("yunong-ffi-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let db = CString::new(base.join("t.db").to_string_lossy().to_string()).unwrap();
        let value = CString::new("sk-abc123456789").unwrap();
        unsafe {
            let bad_key = CString::new("BAD KEY").unwrap();
            let response = call_secret(yunong_set_secret(
                bad_key.as_ptr(),
                value.as_ptr(),
                db.as_ptr(),
            ));
            assert_eq!(response["code"], "BAD_INPUT", "{response}");

            let key = CString::new("k").unwrap();
            let empty = CString::new("").unwrap();
            let response =
                call_secret(yunong_set_secret(key.as_ptr(), empty.as_ptr(), db.as_ptr()));
            assert_eq!(response["code"], "BAD_INPUT", "{response}");

            let too_long = CString::new("a".repeat(4097)).unwrap();
            let response = call_secret(yunong_set_secret(
                key.as_ptr(),
                too_long.as_ptr(),
                db.as_ptr(),
            ));
            assert_eq!(response["code"], "BAD_INPUT", "{response}");
            // 非法 key 的 get/delete 同样是 BAD_INPUT
            let response = call_secret(yunong_get_secret(bad_key.as_ptr(), db.as_ptr()));
            assert_eq!(response["code"], "BAD_INPUT");
            let response = call_secret(yunong_delete_secret(bad_key.as_ptr(), db.as_ptr()));
            assert_eq!(response["code"], "BAD_INPUT");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn secret_degraded_store_reports_reason_without_plaintext() {
        // 模拟"平台安全存储不可用"：内存实现（`degraded:true` + reason），绝不落明文文件
        use crate::secret::KeyStore;
        let store = crate::secret::MemoryKeyStore::new("DPAPI 不可用：本次仅内存生效");
        let status = store.set("byok_api_key", "sk-abc123456789xyz").unwrap();
        assert!(status.degraded);
        assert!(status.reason.unwrap().contains("仅内存"));
        let json = serde_json::to_string(&serde_json::json!({
            "set": status.set,
            "masked": status.masked,
            "degraded": status.degraded,
        }))
        .unwrap();
        assert!(!json.contains("sk-abc123456789xyz"));
        assert!(json.contains("sk-****9xyz"));
    }
    // ---------------------------------------------------------------------
    // 流式对话 FFI（W7 P0）
    // ---------------------------------------------------------------------

    extern "C" fn collect_chunk(chunk_json: *const c_char, user: *mut c_void) -> i32 {
        let sink = unsafe { &mut *(user as *mut Vec<String>) };
        let text = unsafe { CStr::from_ptr(chunk_json) }
            .to_str()
            .unwrap()
            .to_owned();
        sink.push(text);
        0
    }

    #[test]
    fn chat_stream_delivers_config_errors_as_chunks() {
        // 缺 API Key：不应崩，也不应走"返回值报错"，而是回一条 Chunk::Error
        let json = CString::new(r#"{"channel":"byok","model":"m","messages":[]}"#).unwrap();
        let db = CString::new(":memory:").unwrap();
        let mut sink: Vec<String> = Vec::new();
        let handle = unsafe {
            yunong_chat_stream(
                json.as_ptr(),
                db.as_ptr(),
                Some(collect_chunk),
                &mut sink as *mut Vec<String> as *mut c_void,
            )
        };
        assert!(handle > 0, "合法调用必须返回 handle");
        for _ in 0..400 {
            if !sink.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!sink.is_empty(), "必须通过回调交付错误");
        let value: Value = serde_json::from_str(&sink[0]).unwrap();
        assert_eq!(value["type"], "error");
        assert!(!value["message"].as_str().unwrap().is_empty());
        // 收尾：取消（幂等）+ 句柄最终会被回收
        assert_eq!(yunong_chat_cancel(handle), 1);
        assert_eq!(yunong_chat_cancel(handle), 1);
    }

    #[test]
    fn chat_stream_rejects_null_arguments_without_spawning() {
        let db = CString::new(":memory:").unwrap();
        let handle = unsafe {
            yunong_chat_stream(
                std::ptr::null(),
                db.as_ptr(),
                Some(collect_chunk),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(handle, 0);
        // 没有回调也返回 0（Dart 侧拿不到流）
        let json = CString::new("{}").unwrap();
        let handle =
            unsafe { yunong_chat_stream(json.as_ptr(), db.as_ptr(), None, std::ptr::null_mut()) };
        assert_eq!(handle, 0);
    }
}
