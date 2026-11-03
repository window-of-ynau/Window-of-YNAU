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
use crate::local_search::{SearchDoc, SearchStore};
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
// 本地记忆（W7-3）与本地检索（W9）：`memory_items` / `search_*` 的增删查。
// 正文只在本机，不上云（ADR-02 / ADR-19）。
// -------------------------------------------------------------------------

/// 校验类错误统一 `BAD_INPUT` 的映射（记忆与检索共用）。
///
/// 共享的 [`map_core_error`] 把 `Invalid` 归到 `STORAGE`（课表那几个函数的历史行为，
/// 改动它等于改既有契约）。但"记忆类型写错""limit 越界"不是存储故障：界面若显示
/// "本地数据保存失败"，用户会去查磁盘而不是改输入。所以这两个模块单独映射。
fn input_error(err: CoreError) -> Value {
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
            Err(err) => return input_error(err),
        };
        let id = match parse_optional_id(id_or_empty) {
            Ok(id) => id,
            Err(err) => return err_json("BAD_INPUT", err),
        };
        let mut store = match MemoryItemStore::open(db_path) {
            Ok(store) => store,
            Err(err) => return input_error(err),
        };
        let draft = MemoryDraft {
            kind,
            title: Some(title),
            content,
            source: Some(source),
            expires_at: None,
        };
        match store.upsert(&draft, id) {
            Ok(write) => {
                // P0-2：记忆写完**必须**同步进本地检索索引，否则"存了却搜不到"。
                // 索引失败不回滚记忆（内容已经安全落库），但要如实告诉调用方。
                let title = if title.trim().is_empty() {
                    None
                } else {
                    Some(title)
                };
                let (indexed, index_error) =
                    index_memory_for_search(db_path, write.id, kind.as_str(), title, content);
                ok_json(json!({
                    "id": write.id,
                    "evicted": write.evicted,
                    "indexed": indexed,
                    "index_error": index_error,
                }))
            }
            Err(err) => input_error(err),
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
                Err(err) => return input_error(err),
            },
        };
        let limit = match parse_list_limit(limit) {
            Ok(limit) => limit,
            Err(err) => return err_json("BAD_INPUT", err),
        };
        let store = match MemoryItemStore::open(db_path) {
            Ok(store) => store,
            Err(err) => return input_error(err),
        };
        match store.list(kind, limit) {
            Ok(items) => ok_json(json!(items)),
            Err(err) => input_error(err),
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
            Err(err) => return input_error(err),
        };
        match store.delete(id) {
            Ok(deleted) => {
                // P0-2：删记忆必须同步删索引，否则"删了还能搜到"（孤儿倒排）
                let unindexed = unindex_memory_for_search(db_path, id);
                ok_json(json!({ "deleted": deleted, "unindexed": unindexed }))
            }
            Err(err) => input_error(err),
        }
    })
}

/// 把一条记忆写进本地检索索引；返回 `(是否成功, 失败原因)`。
///
/// 失败**不阻断**记忆写入：正文已经安全落库，索引可以重建（`yunong_search_upsert` 也能补）。
/// 但要如实回给调用方，否则又会变成"存了搜不到"且无人知情。
fn index_memory_for_search(
    db_path: &str,
    id: i64,
    kind: &str,
    title: Option<&str>,
    content: &str,
) -> (bool, Option<String>) {
    match SearchStore::open(db_path) {
        Ok(mut search) => {
            let doc = SearchDoc {
                id,
                kind,
                title,
                content,
            };
            match search.upsert(&doc) {
                Ok(_) => (true, None),
                Err(err) => (false, Some(err.to_string())),
            }
        }
        Err(err) => (false, Some(err.to_string())),
    }
}

/// 从本地检索索引里删掉一条记忆；返回是否删成功（失败只影响搜索，不影响记忆本身）。
fn unindex_memory_for_search(db_path: &str, id: i64) -> bool {
    match SearchStore::open(db_path) {
        Ok(mut search) => search.delete(id).is_ok(),
        Err(_) => false,
    }
}

// -------------------------------------------------------------------------
// 本地检索（ADR-19 决策 1）：SQLite 上的 BM25，中文 bigram，不做 embedding。
// -------------------------------------------------------------------------

/// 解析检索的 `limit`：空串 = 默认 10；**超过 100 按 100 处理（不报错）**；
/// 0 / 非数字才报 `BAD_INPUT`（"要 0 条"是调用方的 bug，静默返回空数组会掩盖它）。
fn parse_search_limit(text: &str) -> Result<usize, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(crate::local_search::DEFAULT_SEARCH_LIMIT);
    }
    match text.parse::<usize>() {
        Ok(0) => Err("limit 必须是正整数".to_owned()),
        Ok(limit) => Ok(limit.min(crate::local_search::MAX_SEARCH_LIMIT)),
        Err(_) => Err(format!("limit 必须是整数：{text}")),
    }
}

/// 请求 id（响应里带一个，便于 Dart 侧把"哪次点搜索"与日志对上）。
fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 本地 BM25 检索（**契约已锁**，不要再改形状）。
///
/// ```json
/// {"ok":true,"data":{"hits":[{"id":12,"kind":"note","title":"…","snippet":"…","score":3.2741}]},"request_id":"…"}
/// ```
///
/// - 空查询 / 只含标点 / 无命中 → `{"ok":true,"data":{"hits":[]}}`（"没搜到"不是错误）；
/// - `limit` 缺省 10，上限 100（超出按 100 处理，不报错）；
/// - `id` 是数字（记忆页 = `memory_items.id`），`kind` 与记忆一致（note/summary/fact），
///   `score` 保留 4 位小数。
///
/// # Safety
///
/// 三个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_search_local(
    query: *const c_char,
    limit: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (query, limit, db_path) = match (
            read_arg(query, "query"),
            read_arg(limit, "limit"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(query), Ok(limit), Ok(db_path)) => (query, limit, db_path),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                return err_json("BAD_INPUT", err)
            }
        };
        let limit = match parse_search_limit(limit) {
            Ok(limit) => limit,
            Err(err) => return err_json("BAD_INPUT", err),
        };
        let store = match SearchStore::open(db_path) {
            Ok(store) => store,
            Err(err) => return input_error(err),
        };
        match store.search(query, limit) {
            // 契约：**必须**包 `data.hits`（不是 data 直接是数组）
            Ok(results) => json!({
                "ok": true,
                "data": { "hits": results },
                "request_id": new_request_id(),
            }),
            Err(err) => input_error(err),
        }
    })
}

/// 写入/更新一篇可检索文档（索引**增量**更新，不重建）。
///
/// 返回 `{"ok":true,"data":{"id":12,"terms":37}}`，`terms` = 该文档的倒排行数。
/// 同一数字 `doc_id` 反复调用 = 覆盖（旧倒排会被清掉）。
/// `kind` 与 `memory_items` 一致：`note` / `summary` / `fact`。
///
/// # Safety
///
/// 五个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_search_upsert(
    doc_id: *const c_char,
    kind: *const c_char,
    title: *const c_char,
    content: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (doc_id, kind, title, content, db_path) = match (
            read_arg(doc_id, "doc_id"),
            read_arg(kind, "kind"),
            read_arg(title, "title"),
            read_arg(content, "content"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(id), Ok(kind), Ok(title), Ok(content), Ok(db_path)) => {
                (id, kind, title, content, db_path)
            }
            _ => return err_json("BAD_INPUT", "参数不完整或不是 UTF-8"),
        };
        // id 是数字：契约（`data.hits[].id`）是 number，不是 "memory:12" 这种字符串
        let id = match doc_id.trim().parse::<i64>() {
            Ok(id) if id > 0 => id,
            _ => {
                return err_json(
                    "BAD_INPUT",
                    format!("doc_id 必须是正整数：{}", doc_id.trim()),
                )
            }
        };
        let kind = match MemoryKind::parse(kind) {
            Ok(kind) => kind,
            Err(err) => return input_error(err),
        };
        let mut store = match SearchStore::open(db_path) {
            Ok(store) => store,
            Err(err) => return input_error(err),
        };
        let title = if title.trim().is_empty() {
            None
        } else {
            Some(title)
        };
        let doc = SearchDoc {
            id,
            kind: kind.as_str(),
            title,
            content,
        };
        match store.upsert(&doc) {
            // `evicted` 只在索引到容量上限、按 updated_at 淘汰了最旧文档时才非 0（R5-F3）
            Ok(outcome) => ok_json(json!({
                "id": id,
                "terms": outcome.terms,
                "evicted": outcome.evicted,
            })),
            Err(err) => input_error(err),
        }
    })
}

/// 从索引里删除一篇文档（倒排 + 文档行），返回 `{"ok":true,"data":{"deleted":0|1}}`。
///
/// # Safety
///
/// 两个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_search_delete(
    doc_id: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (doc_id, db_path) = match (read_arg(doc_id, "doc_id"), read_arg(db_path, "db_path")) {
            (Ok(doc_id), Ok(db_path)) => (doc_id, db_path),
            (Err(err), _) | (_, Err(err)) => return err_json("BAD_INPUT", err),
        };
        let id = match doc_id.trim().parse::<i64>() {
            Ok(id) if id > 0 => id,
            _ => {
                return err_json(
                    "BAD_INPUT",
                    format!("doc_id 必须是正整数：{}", doc_id.trim()),
                )
            }
        };
        let mut store = match SearchStore::open(db_path) {
            Ok(store) => store,
            Err(err) => return input_error(err),
        };
        match store.delete(id) {
            Ok(deleted) => ok_json(json!({ "deleted": deleted })),
            Err(err) => input_error(err),
        }
    })
}

// ---------------------------------------------------------------------------
// 流式对话（W7 P0）：回调式 FFI
// ---------------------------------------------------------------------------

/// 流式回调：`chunk_json` **只在本次回调期间有效**（Rust 分配、Rust 释放，Dart 侧绝不能 free）。
///
/// **返回类型是 `void`**（R5-F6：早期版本让回调返回 `1/0` 表示取消，现在不是了）：
/// 回调里**不能**通过返回值取消流。要取消请拿 `yunong_chat_stream` 返回的 handle
/// 调 [`yunong_chat_cancel`]（可以在回调里调，也可以从别的线程调）。
///
/// 取消之后仍会再收到**一帧终态** `{"type":"done","finish":"stop"}`（R5-F7）：
/// 它表示"本条流已结束"，不代表模型正常说完——客户端据此收尾即可。
pub type ChatChunkCallback = unsafe extern "C" fn(chunk_json: *const c_char, user: *mut c_void);

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
        // 回调是 void：Dart 的 `NativeCallable.listener` 只能返回 void，
        // 返回寄存器内容未定义，若当成"非 0 即取消"会把每个 chunk 都判成取消，
        // 流立刻结束、done 永远不来。取消只由 `yunong_chat_cancel` 的 registry 判定。
        match CString::new(json) {
            Ok(c) => {
                unsafe { (self.on_chunk)(c.as_ptr(), self.user) };
                true
            }
            // 含 NUL 的非法 JSON：无法安全投递，按终止处理
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
        // R4-F3：`set:true, broken:true` = 本地有密文但解不开，UI 该提示"请重新填写"
        "broken": status.broken,
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

/// 用**存储里的 refresh** 换一个新的 access（"重启免登录"的最后一环）。
///
/// refresh 明文**永不跨过这道边界**：Rust 自己从平台安全存储读出来发请求，
/// 只把新的 access 交给 Dart；服务端若轮换了 refresh，也由 Rust 直接写回存储。
///
/// ```json
/// {"ok":true,"data":{"access_token":"…","expires_in":3600}}
/// {"ok":false,"code":"UNAUTHORIZED","message":"登录已失效，请重新登录"}
/// {"ok":false,"code":"NETWORK","message":"网络不可用：…"}
/// ```
///
/// **失败分级**（这是本函数最要紧的语义）：
/// - 401 / 403 → 删掉本地 access + refresh，返回 `UNAUTHORIZED`（UI 回未登录态）；
/// - 网络不可达 / 超时 / 5xx / 429 → **保留令牌**，返回 `NETWORK`（让用户重试，而不是重新登录）。
///
/// # 阻塞说明
///
/// 这是一次同步 HTTP 调用（最长 `connect 5s + total 10s`）。**Dart 侧应放在后台 isolate 里调**，
/// 别在 UI isolate 上直接调用。
///
/// # Safety
///
/// 两个参数都必须是有效的 NUL 结尾 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn yunong_auth_refresh_from_store(
    cloud_base_url: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (base_url, db_path) = match (
            read_arg(cloud_base_url, "cloud_base_url"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(base_url), Ok(db_path)) => (base_url, db_path),
            (Err(err), _) | (_, Err(err)) => return err_json("BAD_INPUT", err),
        };
        let store = secret_store_for(db_path);
        auth_refresh_json(store.as_ref(), base_url)
    })
}

/// `yunong_auth_refresh_from_store` 的本体（把 store 作为参数，方便测试注入内存实现）。
fn auth_refresh_json(store: &dyn crate::secret::KeyStore, base_url: &str) -> Value {
    match crate::auth::refresh_blocking(store, base_url, Default::default()) {
        Ok(refreshed) => ok_json(json!({
            "access_token": refreshed.access_token,
            "expires_in": refreshed.expires_in,
            "rotated": refreshed.rotated,
        })),
        Err(err) => err_json(err.code(), err.message()),
    }
}
// ---------------------------------------------------------------------------
// 联网搜索状态（ADR-19 决策 5）：UI 必须诚实
// ---------------------------------------------------------------------------

/// 查询当前通道/协议下可用的搜索来源。
///
/// 返回 `{source, label, available, hint}`：`available:false` 时 UI **必须**常驻提示
/// “未配置联网搜索”，不允许静默降级。
///
/// # Safety
///
/// 三个参数都必须是有效的 NUL 结尾 UTF-8 字符串（db_path 用于判断 BYOK Key 是否已保存）。
#[no_mangle]
pub unsafe extern "C" fn yunong_search_status(
    channel: *const c_char,
    protocol: *const c_char,
    db_path: *const c_char,
) -> *mut c_char {
    respond(|| {
        let (channel, protocol, db_path) = match (
            read_arg(channel, "channel"),
            read_arg(protocol, "protocol"),
            read_arg(db_path, "db_path"),
        ) {
            (Ok(channel), Ok(protocol), Ok(db_path)) => (channel, protocol, db_path),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                return err_json("BAD_INPUT", err)
            }
        };
        // BYOK 是否已保存"自带搜索 Key"（复用同一套 secret store，明文不出 Rust）
        let has_byok_key = {
            let store = secret_store_for(db_path);
            matches!(store.load_raw(crate::secret::BYOK_SEARCH_KEY), Ok(Some(_)))
        };
        let source = crate::search::resolve_source(channel, protocol, has_byok_key);
        let status = crate::search::SearchStatus::for_source(source);
        ok_json(json!({
            "source": status.source,
            "label": status.label,
            "available": status.available,
            "hint": status.hint,
        }))
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

/// 让 Windows 原生标题栏跟随 App 主题（深色/浅色）。
///
/// ```json
/// {"ok":true,"data":{"dark":true,"attribute":20,"hwnd":1234567}}
/// {"ok":false,"code":"UNSUPPORTED","message":"当前平台不支持设置标题栏颜色（仅 Windows）"}
/// {"ok":false,"code":"NOT_FOUND","message":"没找到云农之窗的窗口（标题栏设置需在窗口创建之后调用）"}
/// {"ok":false,"code":"DWM_FAILED","message":"设置标题栏颜色失败：…"}
/// ```
///
/// - 属性号先试 **20**（Win10 20H1+ / Win11）再试 **19**（1809~2004），两个都失败才报错；
/// - **必须在窗口创建之后调用**：启动时调一次 + 用户切主题时再调；
/// - 非 Windows 返回 `UNSUPPORTED`（不 panic）；找不到窗口返回 `NOT_FOUND`（可重试）。
///
/// # Safety
///
/// 无指针参数，任何线程调用都安全（内部只做 Win32 调用）。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn yunong_set_titlebar_dark(dark: i32) -> *mut c_char {
    respond(|| match crate::titlebar::set_dark(dark != 0) {
        Ok(report) => ok_json(json!({
            "dark": report.dark,
            "attribute": report.attribute,
            "hwnd": report.hwnd,
        })),
        Err(err) => err_json(err.code(), err.message()),
    })
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
    // P0-2：记忆写入/更新/删除必须同步本地检索索引
    // ---------------------------------------------------------------------

    fn search_hits(query: &str, db: &str) -> Vec<serde_json::Value> {
        let value = call_search_local(query, "", db);
        assert_eq!(value["ok"], true, "{value}");
        value["data"]["hits"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// owner 实测场景：**不填标题**、正文「雨伞是黑色的」，搜「雨伞」「黑色」都要能命中。
    #[test]
    fn memory_upsert_is_searchable_even_without_title() {
        let db = temp_db();
        let created = call_memory_upsert("note", "", "雨伞是黑色的", "user", "", &db);
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(created["data"]["indexed"], true, "{created}");
        let id = created["data"]["id"].as_i64().unwrap();

        for query in ["雨伞", "黑色", "雨伞是黑色的"] {
            let hits = search_hits(query, &db);
            assert!(
                hits.iter().any(|hit| hit["id"].as_i64() == Some(id)),
                "搜 {query:?} 应当命中记忆 {id}，实际：{hits:?}"
            );
        }
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    /// 带标题的记忆：标题与正文都能搜到，kind 跟着记忆走。
    #[test]
    fn memory_upsert_indexes_title_and_kind() {
        let db = temp_db();
        let created = call_memory_upsert("fact", "宿舍", "水壶在柜子里", "user", "", &db);
        assert_eq!(created["data"]["indexed"], true, "{created}");
        let id = created["data"]["id"].as_i64().unwrap();
        let hits = search_hits("水壶", &db);
        let hit = hits
            .iter()
            .find(|hit| hit["id"].as_i64() == Some(id))
            .unwrap_or_else(|| panic!("没命中：{hits:?}"));
        assert_eq!(hit["kind"], "fact");
        assert_eq!(hit["title"], "宿舍");
        assert!(search_hits("宿舍", &db)
            .iter()
            .any(|h| h["id"].as_i64() == Some(id)));
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    /// 更新记忆：索引换成新内容，**不是**多出一条。
    #[test]
    fn memory_update_replaces_index_entry() {
        let db = temp_db();
        let created = call_memory_upsert("note", "", "雨伞是黑色的", "user", "", &db);
        let id = created["data"]["id"].as_i64().unwrap().to_string();

        let updated = call_memory_upsert("note", "", "雨伞是蓝色的", "user", &id, &db);
        assert_eq!(updated["ok"], true, "{updated}");
        assert_eq!(updated["data"]["id"].as_i64(), id.parse::<i64>().ok());

        assert!(
            search_hits("黑色", &db).is_empty(),
            "旧内容不该还能搜到（否则是没更新索引）"
        );
        let hits = search_hits("蓝色", &db);
        assert_eq!(hits.len(), 1, "更新后只该有一条：{hits:?}");
        assert_eq!(hits[0]["id"].as_i64(), id.parse::<i64>().ok());
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    /// 删除记忆：索引里也要消失。
    #[test]
    fn memory_delete_removes_index_entry() {
        let db = temp_db();
        let created = call_memory_upsert("note", "", "雨伞是黑色的", "user", "", &db);
        let id = created["data"]["id"].as_i64().unwrap();
        assert!(!search_hits("雨伞", &db).is_empty());

        let deleted = call_memory_delete(&id.to_string(), &db);
        assert_eq!(deleted["data"]["deleted"], 1, "{deleted}");
        assert_eq!(deleted["data"]["unindexed"], true, "{deleted}");
        assert!(
            search_hits("雨伞", &db).is_empty(),
            "删了记忆还搜得到 = 索引没清"
        );
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    // ---------------------------------------------------------------------
    // 本地检索 FFI（ADR-19 决策 1）
    // ---------------------------------------------------------------------

    fn call_search_upsert(doc_id: &str, kind: &str, title: &str, content: &str, db: &str) -> Value {
        let (doc_id, kind, title, content, db) = (
            CString::new(doc_id).unwrap(),
            CString::new(kind).unwrap(),
            CString::new(title).unwrap(),
            CString::new(content).unwrap(),
            CString::new(db).unwrap(),
        );
        let raw = unsafe {
            yunong_search_upsert(
                doc_id.as_ptr(),
                kind.as_ptr(),
                title.as_ptr(),
                content.as_ptr(),
                db.as_ptr(),
            )
        };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    fn call_search_local(query: &str, limit: &str, db: &str) -> Value {
        let (query, limit, db) = (
            CString::new(query).unwrap(),
            CString::new(limit).unwrap(),
            CString::new(db).unwrap(),
        );
        let raw = unsafe { yunong_search_local(query.as_ptr(), limit.as_ptr(), db.as_ptr()) };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    fn call_search_delete(doc_id: &str, db: &str) -> Value {
        let (doc_id, db) = (CString::new(doc_id).unwrap(), CString::new(db).unwrap());
        let raw = unsafe { yunong_search_delete(doc_id.as_ptr(), db.as_ptr()) };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn search_roundtrip_via_ffi_matches_the_locked_contract() {
        let db = temp_db();
        let written = call_search_upsert(
            "1",
            "note",
            "课表提醒",
            "周三下午三门课，最后一门在尚农楼1-18",
            &db,
        );
        assert_eq!(written["ok"], true, "{written}");
        // 契约：data.id 是**数字**
        assert_eq!(written["data"]["id"], 1, "{written}");
        assert!(written["data"]["terms"].as_u64().unwrap() > 0, "{written}");
        call_search_upsert("2", "fact", "图书馆", "图书馆开放到晚十点", &db);

        let found = call_search_local("尚农楼", "", &db);
        assert_eq!(found["ok"], true, "{found}");
        // 契约：必须包 `data.hits`，且带 request_id
        assert!(found["data"].is_object(), "{found}");
        let hits = found["data"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "{found}");
        assert_eq!(hits[0]["id"], 1, "id 必须是数字");
        assert_eq!(hits[0]["kind"], "note");
        assert_eq!(hits[0]["title"], "课表提醒");
        assert!(hits[0]["snippet"].as_str().unwrap().contains("尚农楼"));
        assert!(hits[0]["score"].as_f64().unwrap() > 0.0);
        assert!(
            found["request_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "响应必须带 request_id：{found}"
        );

        // limit 生效
        let one = call_search_local("三门", "1", &db);
        assert_eq!(one["data"]["hits"].as_array().unwrap().len(), 1, "{one}");

        // 删除后不再命中
        assert_eq!(call_search_delete("1", &db)["data"]["deleted"], 1);
        assert!(call_search_local("尚农楼", "", &db)["data"]["hits"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(call_search_delete("1", &db)["data"]["deleted"], 0);
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn search_empty_query_bad_limits_and_kind_via_ffi() {
        let db = temp_db();
        call_search_upsert("1", "note", "笔记", "高等数学笔记", &db);
        // 空查询 = 空数组，不是错误
        let empty = call_search_local("   ", "", &db);
        assert_eq!(empty["ok"], true, "{empty}");
        assert!(
            empty["data"]["hits"].as_array().unwrap().is_empty(),
            "{empty}"
        );
        // 无命中也不是错误
        let miss = call_search_local("量子力学", "", &db);
        assert_eq!(miss["ok"], true, "{miss}");
        assert!(
            miss["data"]["hits"].as_array().unwrap().is_empty(),
            "{miss}"
        );
        // limit：0 / 非数字是 BAD_INPUT；超过 100 **夹到 100 不报错**
        for bad in ["0", "abc", "-1"] {
            let result = call_search_local("数学", bad, &db);
            assert_eq!(result["code"], "BAD_INPUT", "limit={bad} => {result}");
        }
        let clamped = call_search_local("数学", "999", &db);
        assert_eq!(clamped["ok"], true, "{clamped}");
        assert_eq!(
            clamped["data"]["hits"].as_array().unwrap().len(),
            1,
            "{clamped}"
        );

        // R5-F2：超长 query 走 BAD_INPUT（上限 512 字符）
        let too_long = "数".repeat(crate::local_search::MAX_QUERY_CHARS + 1);
        let over = call_search_local(&too_long, "", &db);
        assert_eq!(over["code"], "BAD_INPUT", "{over}");

        // doc_id 必须是正整数；kind 只接受 note/summary/fact
        assert_eq!(
            call_search_upsert("memory:1", "note", "标题", "正文", &db)["code"],
            "BAD_INPUT"
        );
        assert_eq!(
            call_search_upsert("0", "note", "标题", "正文", &db)["code"],
            "BAD_INPUT"
        );
        assert_eq!(
            call_search_upsert("2", "doc", "标题", "正文", &db)["code"],
            "BAD_INPUT"
        );
        assert_eq!(
            call_search_upsert("2", "", "标题", "正文", &db)["code"],
            "BAD_INPUT"
        );
        let _ = std::fs::remove_dir_all(std::path::Path::new(&db).parent().unwrap());
    }

    #[test]
    fn search_null_arguments_do_not_crash() {
        let raw =
            unsafe { yunong_search_local(std::ptr::null(), std::ptr::null(), std::ptr::null()) };
        assert!(!raw.is_null(), "绝不返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");

        let raw = unsafe {
            yunong_search_upsert(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");

        let raw = unsafe { yunong_search_delete(std::ptr::null(), std::ptr::null()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");
    }

    // ---------------------------------------------------------------------
    // 套餐令牌刷新 FFI（T61）：refresh 明文不出 Rust，失败分级清/不清存储
    // ---------------------------------------------------------------------

    const AUTH_OK_BODY: &str = r#"{"ok":true,"data":{"user_id":"u1","access_token":"new-access-9","expires_in":3600,"refresh_token":"rotated-9"}}"#;

    fn memory_store_with(
        refresh: Option<&str>,
        access: Option<&str>,
    ) -> crate::secret::MemoryKeyStore {
        use crate::secret::KeyStore;
        let store = crate::secret::MemoryKeyStore::new("测试内存存储");
        if let Some(value) = refresh {
            store.set(crate::auth::REFRESH_KEY, value).unwrap();
        }
        if let Some(value) = access {
            store.set(crate::auth::ACCESS_KEY, value).unwrap();
        }
        store
    }

    fn raw_of(store: &crate::secret::MemoryKeyStore, key: &str) -> Option<String> {
        use crate::secret::KeyStore;
        store.load_raw(key).unwrap()
    }

    /// 成功：返回新 access、把轮换后的 refresh 写回存储。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_refresh_success_returns_access_and_rotates_refresh() {
        let server =
            crate::gateway::fake_server::FakeServer::reply(200, "application/json", AUTH_OK_BODY)
                .await;
        let store = std::sync::Arc::new(memory_store_with(Some("old-refresh"), Some("old-access")));
        let base = server.base_url();
        let store_for_call = std::sync::Arc::clone(&store);
        // `auth_refresh_json` 内部会自己 block_on 一个 runtime，所以放到阻塞线程里跑，
        // 否则会把测试自己的多线程运行时堵住（假上游也就没线程驱动了）。
        let result =
            tokio::task::spawn_blocking(move || auth_refresh_json(store_for_call.as_ref(), &base))
                .await
                .unwrap();

        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["data"]["access_token"], "new-access-9");
        assert_eq!(result["data"]["expires_in"], 3600);
        assert_eq!(result["data"]["rotated"], true);
        assert_eq!(server.captured().path(), crate::auth::REFRESH_PATH);
        assert_eq!(server.captured().json()["refresh_token"], "old-refresh");
        assert_eq!(
            raw_of(&store, crate::auth::REFRESH_KEY).as_deref(),
            Some("rotated-9"),
            "轮换后的 refresh 必须写回，否则下次刷新就 401"
        );
        assert_eq!(
            raw_of(&store, crate::auth::ACCESS_KEY).as_deref(),
            Some("new-access-9")
        );
    }

    /// 401：返回 UNAUTHORIZED，且本地 access + refresh 都被清掉。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_refresh_unauthorized_clears_store() {
        let server = crate::gateway::fake_server::FakeServer::reply(
            401,
            "application/json",
            r#"{"ok":false,"code":"UNAUTHORIZED","message":"刷新令牌无效"}"#,
        )
        .await;
        let store =
            std::sync::Arc::new(memory_store_with(Some("dead-refresh"), Some("dead-access")));
        let base = server.base_url();
        let store_for_call = std::sync::Arc::clone(&store);
        let result =
            tokio::task::spawn_blocking(move || auth_refresh_json(store_for_call.as_ref(), &base))
                .await
                .unwrap();

        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "UNAUTHORIZED");
        assert_eq!(result["message"], "登录已失效，请重新登录");
        assert_eq!(raw_of(&store, crate::auth::REFRESH_KEY), None);
        assert_eq!(raw_of(&store, crate::auth::ACCESS_KEY), None);
    }

    /// 网络不可达：返回 NETWORK，且**存储内容一个字节都不动**。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_refresh_network_failure_keeps_store() {
        let store =
            std::sync::Arc::new(memory_store_with(Some("good-refresh"), Some("good-access")));
        let store_for_call = std::sync::Arc::clone(&store);
        let result = tokio::task::spawn_blocking(move || {
            auth_refresh_json(store_for_call.as_ref(), "http://127.0.0.1:1")
        })
        .await
        .unwrap();

        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "NETWORK", "{result}");
        assert_eq!(
            raw_of(&store, crate::auth::REFRESH_KEY).as_deref(),
            Some("good-refresh"),
            "断网不能把用户登出"
        );
        assert_eq!(
            raw_of(&store, crate::auth::ACCESS_KEY).as_deref(),
            Some("good-access")
        );
    }

    #[test]
    fn auth_refresh_ffi_rejects_null_and_bad_base_url() {
        let raw = unsafe { yunong_auth_refresh_from_store(std::ptr::null(), std::ptr::null()) };
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");

        let base = CString::new("").unwrap();
        let db = CString::new(temp_db()).unwrap();
        let raw = unsafe { yunong_auth_refresh_from_store(base.as_ptr(), db.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "BAD_INPUT", "{value}");
        assert!(value["message"]
            .as_str()
            .unwrap()
            .contains("cloud_base_url"));
    }

    /// R4-F3：密文损坏时 `get_secret` 的 JSON 必须带 `broken:true`（与 `set:true` 并存），
    /// 这样 UI 能提示"本地凭据已损坏，请重新填写"，而不是显示矛盾的"未设置"。
    #[cfg(windows)]
    #[test]
    fn get_secret_reports_broken_ciphertext_via_ffi() {
        let dir = std::env::temp_dir().join(format!("yunong-ffi-broken-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("timetable.db").to_string_lossy().into_owned();
        // 先正常存一条
        let store = secret_store_for(&db_path);
        store
            .set("byok_api_key", "sk-test-0123456789abcdef")
            .unwrap();
        // 再把它写坏
        let bin = dir.join("secrets").join("byok_api_key.bin");
        std::fs::write(&bin, b"not-a-dpapi-blob").unwrap();

        let key = CString::new("byok_api_key").unwrap();
        let db = CString::new(db_path.clone()).unwrap();
        let raw = unsafe { yunong_get_secret(key.as_ptr(), db.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(value["data"]["set"], true, "{value}");
        assert_eq!(value["data"]["broken"], true, "{value}");
        assert!(!value["data"]["masked"].as_str().unwrap().is_empty());
        // 明文绝不出现在返回里
        assert!(
            !text.contains("sk-test-0123456789abcdef"),
            "泄漏明文：{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 平台差异（与 secret 用例同一条规则）：非 Windows 没有系统安全存储，
    /// `default_store` 每次调用都是**新的内存实例**，所以 FFI 这条路上读不到 refresh →
    /// 如实返回 UNAUTHORIZED。Windows 上则真的能读出来并刷新（下一个用例）。
    #[cfg(not(windows))]
    #[test]
    fn auth_refresh_ffi_reports_unauthorized_on_degraded_storage() {
        let base = CString::new("http://127.0.0.1:1").unwrap();
        let db = CString::new(temp_db()).unwrap();
        let raw = unsafe { yunong_auth_refresh_from_store(base.as_ptr(), db.as_ptr()) };
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["code"], "UNAUTHORIZED", "{value}");
    }

    /// Windows 端到端：走真实 FFI + DPAPI 文件存储，验证"存进去 → 刷新 → 轮换写回"。
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_refresh_ffi_end_to_end_on_windows() {
        let server =
            crate::gateway::fake_server::FakeServer::reply(200, "application/json", AUTH_OK_BODY)
                .await;
        let dir = std::env::temp_dir().join(format!("yunong-ffi-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("timetable.db").to_string_lossy().into_owned();

        // 先用 FFI 把一对令牌存进平台安全存储（模拟登录那次）
        let db = CString::new(db_path.clone()).unwrap();
        let refresh_key = CString::new(crate::auth::REFRESH_KEY).unwrap();
        let refresh_value = CString::new("old-refresh").unwrap();
        unsafe {
            let response = call_secret(yunong_set_secret(
                refresh_key.as_ptr(),
                refresh_value.as_ptr(),
                db.as_ptr(),
            ));
            assert_eq!(response["ok"], true, "{response}");
            let access_key = CString::new(crate::auth::ACCESS_KEY).unwrap();
            let access_value = CString::new("old-access").unwrap();
            let response = call_secret(yunong_set_secret(
                access_key.as_ptr(),
                access_value.as_ptr(),
                db.as_ptr(),
            ));
            assert_eq!(response["ok"], true, "{response}");
        }

        // 再走刷新 FFI。注意：`refresh_blocking` 会自建 runtime，
        // 所以必须在**阻塞线程**上调用（tokio 不允许在 runtime 里再 block_on 一个 runtime）。
        let base = CString::new(server.base_url()).unwrap();
        let db_for_call = db.clone();
        let text = tokio::task::spawn_blocking(move || unsafe {
            let raw = yunong_auth_refresh_from_store(base.as_ptr(), db_for_call.as_ptr());
            assert!(!raw.is_null(), "绝不能返回 NULL");
            let text = CStr::from_ptr(raw).to_str().unwrap().to_owned();
            yunong_free_string(raw);
            text
        })
        .await
        .unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(value["data"]["access_token"], "new-access-9");
        assert_eq!(value["data"]["rotated"], true);

        // 轮换后的 refresh 真的写回了存储
        let store = secret_store_for(&db_path);
        assert_eq!(
            store.load_raw(crate::auth::REFRESH_KEY).unwrap().as_deref(),
            Some("rotated-9")
        );
        let _ = std::fs::remove_dir_all(&dir);
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
    fn search_status_covers_four_channels_honestly() {
        let base = std::env::temp_dir().join(format!("yunong-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let db = CString::new(base.join("t.db").to_string_lossy().to_string()).unwrap();
        let cloud = CString::new("cloud").unwrap();
        let byok = CString::new("byok").unwrap();
        let responses = CString::new("openai_responses").unwrap();
        let completions = CString::new("openai_completions").unwrap();

        unsafe {
            // ① 套餐 + Responses → 内置
            let r = call_secret(yunong_search_status(
                cloud.as_ptr(),
                responses.as_ptr(),
                db.as_ptr(),
            ));
            assert_eq!(r["data"]["source"], "built_in");
            assert_eq!(r["data"]["label"], "内置");
            assert_eq!(r["data"]["available"], true);

            // ② 套餐 + Completions → 我方代理
            let r = call_secret(yunong_search_status(
                cloud.as_ptr(),
                completions.as_ptr(),
                db.as_ptr(),
            ));
            assert_eq!(r["data"]["source"], "proxy");
            assert_eq!(r["data"]["label"], "我方代理");

            // ③ BYOK 未填 Key → 未配置 + 诚实提示
            let r = call_secret(yunong_search_status(
                byok.as_ptr(),
                completions.as_ptr(),
                db.as_ptr(),
            ));
            assert_eq!(r["data"]["source"], "none");
            assert_eq!(r["data"]["available"], false);
            assert!(r["data"]["hint"]
                .as_str()
                .unwrap()
                .contains("未配置联网搜索"));

            // ④ BYOK 已填自带 Key → 自带 Key
            // 平台差异（与下面的 secret 用例同一条规则）：非 Windows 没有系统安全存储，
            // set 走降级不落盘，因此读回来仍是「未配置」。断言必须按平台分支，
            // 否则 Linux CI 上会红（此前就是这样挂的）。
            let store = secret_store_for(&base.join("t.db").to_string_lossy());
            let _ = store.set(crate::secret::BYOK_SEARCH_KEY, "sk-test-0123456789abcdef");
            let r = call_secret(yunong_search_status(
                byok.as_ptr(),
                completions.as_ptr(),
                db.as_ptr(),
            ));
            if cfg!(windows) {
                assert_eq!(r["data"]["source"], "byok");
                assert_eq!(r["data"]["label"], "自带 Key");
                assert_eq!(r["data"]["available"], true);
                assert_eq!(r["data"]["hint"], "");
            } else {
                // 降级：Key 未能持久化，必须诚实报「未配置」，不得假装可用
                assert_eq!(r["data"]["source"], "none");
                assert_eq!(r["data"]["available"], false);
            }
        }
        let _ = std::fs::remove_dir_all(&base);
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
            assert!(
                !raw.contains("0123456789abcdef"),
                "get 返回里出现明文片段：{raw}"
            );

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

    extern "C" fn collect_chunk(chunk_json: *const c_char, user: *mut c_void) {
        let sink = unsafe { &mut *(user as *mut Vec<String>) };
        let text = unsafe { CStr::from_ptr(chunk_json) }
            .to_str()
            .unwrap()
            .to_owned();
        sink.push(text);
    }

    /// T75：标题栏深色开关——信封形态正确、任何平台都不 panic。
    #[test]
    fn set_titlebar_dark_returns_envelope_without_panicking() {
        let raw = yunong_set_titlebar_dark(1);
        assert!(!raw.is_null(), "绝不能返回 NULL");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
        unsafe { yunong_free_string(raw) };
        let value: Value = serde_json::from_str(&text).unwrap();
        if cfg!(windows) {
            if value["ok"] == true {
                let attribute = value["data"]["attribute"].as_u64().unwrap();
                assert!(attribute == 20 || attribute == 19, "{value}");
                assert!(value["data"]["hwnd"].as_u64().unwrap_or(0) > 0, "{value}");
            } else {
                let code = value["code"].as_str().unwrap_or_default();
                assert!(
                    code == "NOT_FOUND" || code == "DWM_FAILED",
                    "Windows 上只该是 NOT_FOUND/DWM_FAILED：{value}"
                );
            }
        } else {
            assert_eq!(value["ok"], false, "{value}");
            assert_eq!(value["code"], "UNSUPPORTED", "{value}");
        }
        // 浅色也调一次（同样不许 panic）
        let raw = yunong_set_titlebar_dark(0);
        assert!(!raw.is_null());
        unsafe { yunong_free_string(raw) };
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
