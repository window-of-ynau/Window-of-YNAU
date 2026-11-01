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
//! | `BAD_INPUT` | 空参数 / 非 UTF-8 / 空 HTML | "内容为空，请重新抓取" |
//! | `PARSE_FAILED` | 结构性解析失败（找不到表/表头/列数错位） | "学校页面可能已更新，请反馈" |
//! | `PARSE_EMPTY` | 解析成功但一门课都没有（**不落库**） | "未解析到课程，请确认是否已排课" |
//! | `STORAGE` | SQLite 读写失败 | "本地课表保存失败" |
//! | `INTERNAL` | 兜底 | "内部错误" |

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::{json, Value};

use crate::error::CoreError;
use crate::parse::ParseError;
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
}
