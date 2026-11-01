//! 端侧 SQLite：课表落库（`docs/02-系统设计文档.md` §5.2 的 `terms` / `courses`）。
//!
//! # 隐私边界（ADR-02 / ADR-10）
//!
//! 这个库**只在本机**，永不上云。库里**不存** Cookie / API Key / 课表 HTML 原文：
//! 凭据只在 `keystore`，本文件只存结构化课程。
//!
//! # 为什么解析失败要在这里再拦一次
//!
//! `parse` 层已经保证"结构性错误返回 `Err`"，但落库层还要守一条：
//! **导入一个空结果 == 删光旧课表**。用户看到"这学期没课"比看到"页面可能改版"危险得多
//! （v0 的教训），所以 [`CourseStore::import_report`] 对空课程直接报错，不动库。
//!
//! # 幂等
//!
//! 同一学期重复抓取必须**替换**而不是追加（否则每次抓取课表翻倍）：
//! [`CourseStore::replace_term_courses`] 在一个事务里先删该 term 的课程再插入。

use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::calendar::Course;
use crate::error::{CoreError, CoreResult};
use crate::parse::ParseReport;

/// 建表语句（与 `docs/02` §5.2 一致；`starts_on` 允许空串表示"学期起止未知"）。
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS terms (
  term TEXT PRIMARY KEY,
  starts_on TEXT NOT NULL,
  ends_on TEXT
);
CREATE TABLE IF NOT EXISTS courses (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  term TEXT NOT NULL,
  name TEXT NOT NULL,
  weekday INTEGER NOT NULL,
  start_period INTEGER NOT NULL,
  end_period INTEGER NOT NULL,
  weeks TEXT NOT NULL,
  room TEXT,
  teacher TEXT
);
CREATE INDEX IF NOT EXISTS idx_courses_term ON courses(term);
"#;

/// 落库后的一门课（带自增 id，便于 UI 定位与删除）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCourse {
    pub id: i64,
    pub term: String,
    pub name: String,
    /// 1 = 周一 … 7 = 周日
    pub weekday: u8,
    pub start_period: u8,
    pub end_period: u8,
    pub weeks: String,
    pub room: Option<String>,
    pub teacher: Option<String>,
}

/// 学期元信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermRecord {
    pub term: String,
    /// 开学日期（`YYYY-MM-DD`）；空串 = 未知（页面只给了学期名时）
    pub starts_on: String,
    pub ends_on: Option<String>,
}

/// 一次导入的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOutcome {
    pub term: String,
    /// 本次写入的课程数（= `courses.len()`）
    pub imported: usize,
    /// 解析告警（可提示"解析不完整，请核对后反馈"，不影响已入库课程）
    pub warnings: Vec<String>,
}

/// 课表库。
pub struct CourseStore {
    conn: Connection,
}

impl CourseStore {
    /// 打开（或创建）一个库文件；父目录不存在会自动创建。
    pub fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|err| CoreError::Storage(format!("建库目录失败：{err}")))?;
            }
        }
        Self::from_connection(
            Connection::open(path)
                .map_err(|err| CoreError::Storage(format!("打开 SQLite 失败：{err}")))?,
        )
    }

    /// 内存库（测试用）。
    pub fn open_in_memory() -> CoreResult<Self> {
        Self::from_connection(
            Connection::open_in_memory()
                .map_err(|err| CoreError::Storage(format!("打开内存 SQLite 失败：{err}")))?,
        )
    }

    fn from_connection(conn: Connection) -> CoreResult<Self> {
        // WAL：读写不互相阻塞（会话进行中也要能查课表）。
        // 内存库不支持 WAL，失败就退回默认 journal，不算错。
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute_batch(SCHEMA)
            .map_err(|err| CoreError::Storage(format!("建表失败：{err}")))?;
        Ok(Self { conn })
    }

    /// 写入/更新学期元信息。
    pub fn upsert_term(
        &mut self,
        term: &str,
        starts_on: Option<&str>,
        ends_on: Option<&str>,
    ) -> CoreResult<()> {
        if term.trim().is_empty() {
            return Err(CoreError::Invalid("学期名不能为空".into()));
        }
        self.conn
            .execute(
                "INSERT INTO terms (term, starts_on, ends_on) VALUES (?1, ?2, ?3)
                 ON CONFLICT(term) DO UPDATE SET
                   starts_on = CASE WHEN excluded.starts_on != '' THEN excluded.starts_on
                                    ELSE terms.starts_on END,
                   ends_on   = COALESCE(excluded.ends_on, terms.ends_on)",
                params![term, starts_on.unwrap_or(""), ends_on],
            )
            .map_err(|err| CoreError::Storage(format!("写学期失败：{err}")))?;
        Ok(())
    }

    /// **替换**某学期的全部课程（幂等：重复抓取不会翻倍）。
    pub fn replace_term_courses(&mut self, term: &str, courses: &[Course]) -> CoreResult<usize> {
        if term.trim().is_empty() {
            return Err(CoreError::Invalid("学期名不能为空".into()));
        }
        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Storage(format!("开事务失败：{err}")))?;
        tx.execute("DELETE FROM courses WHERE term = ?1", params![term])
            .map_err(|err| CoreError::Storage(format!("清空旧课表失败：{err}")))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO courses
                     (term, name, weekday, start_period, end_period, weeks, room, teacher)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(|err| CoreError::Storage(format!("准备插入失败：{err}")))?;
            for course in courses {
                stmt.execute(params![
                    term,
                    course.name,
                    course.weekday,
                    course.start_period,
                    course.end_period,
                    course.weeks,
                    course.room,
                    course.teacher,
                ])
                .map_err(|err| CoreError::Storage(format!("写课程失败：{err}")))?;
            }
        }
        tx.commit()
            .map_err(|err| CoreError::Storage(format!("提交事务失败：{err}")))?;
        Ok(courses.len())
    }

    /// 把一次解析结果落库。
    ///
    /// 空结果**直接报错**，既不写库也不清库——"静默空表"是明令禁止的失败姿势。
    pub fn import_report(&mut self, term: &str, report: &ParseReport) -> CoreResult<ImportOutcome> {
        if report.courses.is_empty() {
            return Err(CoreError::Invalid(
                "解析结果为空：请确认本学期是否已排课，或学校页面是否改版".into(),
            ));
        }
        self.upsert_term(term, None, None)?;
        let imported = self.replace_term_courses(term, &report.courses)?;
        Ok(ImportOutcome {
            term: term.to_owned(),
            imported,
            warnings: report.warnings.clone(),
        })
    }

    /// 列出已有学期（按名称倒序，最近的在前）。
    pub fn list_terms(&self) -> CoreResult<Vec<TermRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT term, starts_on, ends_on FROM terms ORDER BY term DESC")
            .map_err(|err| CoreError::Storage(format!("查询学期失败：{err}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TermRecord {
                    term: row.get(0)?,
                    starts_on: row.get(1)?,
                    ends_on: row.get(2)?,
                })
            })
            .map_err(|err| CoreError::Storage(format!("查询学期失败：{err}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Storage(format!("读学期失败：{err}")))
    }

    /// 查某学期的课程（按星期、节次排序，UI 直接渲染）。
    pub fn list_courses(&self, term: &str) -> CoreResult<Vec<StoredCourse>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, term, name, weekday, start_period, end_period, weeks, room, teacher
                 FROM courses WHERE term = ?1
                 ORDER BY weekday, start_period, name",
            )
            .map_err(|err| CoreError::Storage(format!("查询课表失败：{err}")))?;
        let rows = stmt
            .query_map(params![term], |row| {
                Ok(StoredCourse {
                    id: row.get(0)?,
                    term: row.get(1)?,
                    name: row.get(2)?,
                    weekday: row.get::<_, u8>(3)?,
                    start_period: row.get::<_, u8>(4)?,
                    end_period: row.get::<_, u8>(5)?,
                    weeks: row.get(6)?,
                    room: row.get(7)?,
                    teacher: row.get(8)?,
                })
            })
            .map_err(|err| CoreError::Storage(format!("查询课表失败：{err}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Storage(format!("读课表失败：{err}")))
    }

    /// 删除某学期（课程一并删除）。
    pub fn delete_term(&mut self, term: &str) -> CoreResult<()> {
        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Storage(format!("开事务失败：{err}")))?;
        tx.execute("DELETE FROM courses WHERE term = ?1", params![term])
            .map_err(|err| CoreError::Storage(format!("删课程失败：{err}")))?;
        tx.execute("DELETE FROM terms WHERE term = ?1", params![term])
            .map_err(|err| CoreError::Storage(format!("删学期失败：{err}")))?;
        tx.commit()
            .map_err(|err| CoreError::Storage(format!("提交事务失败：{err}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn course(name: &str, weekday: u8, start: u8, end: u8) -> Course {
        Course {
            name: name.into(),
            weekday,
            start_period: start,
            end_period: end,
            weeks: "1-16".into(),
            room: Some("东305".into()),
            teacher: Some("张老师".into()),
        }
    }

    fn report(courses: Vec<Course>) -> ParseReport {
        ParseReport {
            courses,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn import_then_list_roundtrip() {
        let mut store = CourseStore::open_in_memory().unwrap();
        let outcome = store
            .import_report("2026-2027-1", &report(vec![course("高等数学A", 1, 1, 2)]))
            .unwrap();
        assert_eq!(outcome.imported, 1);
        assert_eq!(outcome.warnings.len(), 0);

        let courses = store.list_courses("2026-2027-1").unwrap();
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].name, "高等数学A");
        assert_eq!(courses[0].weekday, 1);
        assert_eq!(courses[0].room.as_deref(), Some("东305"));

        let terms = store.list_terms().unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].term, "2026-2027-1");
    }

    #[test]
    fn reimport_replaces_instead_of_duplicating() {
        let mut store = CourseStore::open_in_memory().unwrap();
        store
            .import_report(
                "t1",
                &report(vec![course("A", 1, 1, 2), course("B", 2, 3, 4)]),
            )
            .unwrap();
        // 第二次抓取：B 变成 C，数量不能翻倍
        store
            .import_report(
                "t1",
                &report(vec![course("A", 1, 1, 2), course("C", 2, 3, 4)]),
            )
            .unwrap();
        let courses = store.list_courses("t1").unwrap();
        assert_eq!(courses.len(), 2, "重抓必须是替换，不是追加");
        assert!(courses.iter().any(|c| c.name == "C"));
        assert!(!courses.iter().any(|c| c.name == "B"));
    }

    #[test]
    fn empty_parse_result_never_wipes_the_store() {
        let mut store = CourseStore::open_in_memory().unwrap();
        store
            .import_report("t1", &report(vec![course("A", 1, 1, 2)]))
            .unwrap();

        // 改版页面解析出 0 门课 → 必须报错，且旧数据还在
        let err = store.import_report("t1", &report(Vec::new())).unwrap_err();
        assert!(err.to_string().contains("页面是否改版"), "{err}");
        assert_eq!(store.list_courses("t1").unwrap().len(), 1, "不许清空旧课表");
    }

    #[test]
    fn terms_are_sorted_and_deletable() {
        let mut store = CourseStore::open_in_memory().unwrap();
        for term in ["2025-2026-1", "2026-2027-1", "2026-2027-2"] {
            store
                .import_report(term, &report(vec![course("A", 1, 1, 2)]))
                .unwrap();
        }
        let terms: Vec<String> = store
            .list_terms()
            .unwrap()
            .into_iter()
            .map(|t| t.term)
            .collect();
        assert_eq!(terms, vec!["2026-2027-2", "2026-2027-1", "2025-2026-1"]);

        store.delete_term("2026-2027-1").unwrap();
        assert_eq!(store.list_terms().unwrap().len(), 2);
        assert!(store.list_courses("2026-2027-1").unwrap().is_empty());
    }

    #[test]
    fn warnings_are_passed_through() {
        let mut store = CourseStore::open_in_memory().unwrap();
        let mut parsed = report(vec![course("A", 1, 1, 2)]);
        parsed.warnings.push("第 2 行·星期一·第 3 块缺课名".into());
        let outcome = store.import_report("t1", &parsed).unwrap();
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("第 2 行"));
    }

    #[test]
    fn empty_term_name_is_rejected() {
        let mut store = CourseStore::open_in_memory().unwrap();
        let err = store
            .import_report("  ", &report(vec![course("A", 1, 1, 2)]))
            .unwrap_err();
        assert!(matches!(err, CoreError::Invalid(_)));
    }

    #[test]
    fn file_store_creates_parent_directory() {
        let dir = std::env::temp_dir().join(format!("yunong-store-{}", uuid::Uuid::new_v4()));
        let path = dir.join("nested").join("timetable.db");
        {
            let mut store = CourseStore::open(&path).unwrap();
            store
                .import_report("t1", &report(vec![course("A", 1, 1, 2)]))
                .unwrap();
        }
        assert!(path.exists());
        let store = CourseStore::open(&path).unwrap();
        assert_eq!(store.list_courses("t1").unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
