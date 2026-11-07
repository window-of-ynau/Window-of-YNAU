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
//! # 两套版本号，**互不相干**（别混用！）
//!
//! | 机制 | 归谁 | 记什么 |
//! |---|---|---|
//! | `PRAGMA user_version` | `backfill.rs` | **派生数据回填**到第几版（索引/摘要等） |
//! | `schema_meta.schema_version` | 本文件 | **表结构**迁移到第几版（`MIGRATIONS`） |
//!
//! 为什么不用同一个：`user_version` 已经被回填占用。如果表结构迁移也拿它当版本号，
//! 迁移把 `user_version` 抬到 3，回填逻辑就会以为"派生数据已回填到 v3"而**跳过回填**——
//! 老库升级后索引永远是空的，而且**不报错**。所以表结构版本单独存一张 `schema_meta`。
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

/// 表结构的目标版本（`schema_meta.schema_version` 最终要等于它）。
///
/// **加表/加列时必须 +1**，并在 [`MIGRATIONS`] 追加一项。
/// 注意：这**不是** `PRAGMA user_version`（那个归 `backfill.rs`，见模块文档）。
pub const SCHEMA_VERSION: i64 = 1;

/// 逐级迁移：`(目标版本, SQL)`，按版本号升序执行，**每级一个独立事务**。
///
/// 新增表/列时：追加 `(SCHEMA_VERSION, "CREATE TABLE …")`，并同步抬 `SCHEMA_VERSION`；
/// **不要**改历史项（老库已经按老项迁过了）。
///
/// v1 是版本机制的**基线**：老库第一次升级到这里时，表已经由上面幂等的 `SCHEMA` 建好，
/// 这一步只做"打标"，为 v2 起的真实迁移留位置（例如将来 W9 的 `schedule_items` 就从 v2 开始）。
const MIGRATIONS: &[(i64, &str)] = &[(1, "")];

/// `schema_meta` 表：只放版本号这类元信息，**不放用户数据**。
const SCHEMA_META_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
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

// ---------------------------------------------------------------------------
// 表结构迁移（`schema_meta.schema_version`）与迁移前备份
// ---------------------------------------------------------------------------
//
// **不要**把这里的版本号写进 `PRAGMA user_version`：那个是 `backfill.rs` 的
// "派生数据回填进度"，两者混用会让回填被静默跳过（见模块文档）。

/// 读表结构版本；**没有 `schema_meta` 或没有这一行 → 0（全新库/老库）**。
fn read_schema_version(conn: &Connection) -> CoreResult<i64> {
    let has_table: bool = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .map_err(|err| CoreError::Storage(format!("读库结构信息失败：{err}")))?;
    if !has_table {
        return Ok(0);
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(|err| CoreError::Storage(format!("读 schema_version 失败：{err}")))?;
    match raw {
        None => Ok(0),
        Some(text) => text.trim().parse::<i64>().map_err(|err| {
            CoreError::Storage(format!("schema_version 不是整数（值={text}）：{err}"))
        }),
    }
}

/// 写表结构版本（与迁移**同一个事务**里调用，见 [`apply_schema_migrations`]）。
fn write_schema_version(conn: &Connection, version: i64) -> CoreResult<()> {
    conn.execute(
        "INSERT INTO schema_meta (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version.to_string()],
    )
    .map_err(|err| CoreError::Storage(format!("写 schema_version 失败：{err}")))?;
    Ok(())
}

/// 库里是否已经有**用户表**（用来区分"全新库"与"老库"）。
///
/// 全新库不需要备份（没数据可丢）；老库必须先备份再迁移。
fn has_user_tables(conn: &Connection) -> CoreResult<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE type = 'table'
               AND name NOT IN ('schema_meta', 'sqlite_sequence')",
            [],
            |row| row.get(0),
        )
        .map_err(|err| CoreError::Storage(format!("读表清单失败：{err}")))?;
    Ok(count > 0)
}

/// 迁移前备份：`<db 父目录>\backups\yunong.db.bak-v{N}-{yyyyMMddHHmmss}`。
///
/// **失败必须中止迁移**（返回 `Err`，调用方不会再往下走）——用户的课表/记忆全在这个文件里，
/// "备份失败但继续迁移"是数据丢失的直通车。
///
/// 保留最近 [`BACKUP_KEEP`] 份，更早的按**文件名里的时间戳**清理（不依赖 mtime，
/// 免得复制/同步工具改 mtime 后清错）。
fn backup_before_migration(
    conn: &Connection,
    db_path: &Path,
    from_version: i64,
) -> CoreResult<std::path::PathBuf> {
    let parent = db_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let dir = parent.join("backups");
    std::fs::create_dir_all(&dir).map_err(|err| {
        CoreError::Storage(format!(
            "创建备份目录失败，已中止迁移（{}）：{err}",
            dir.display()
        ))
    })?;

    // WAL 模式下最近的写入可能还在 `-wal` 文件里：先 checkpoint 再拷主文件，
    // 否则备份会"少最后几条数据"（正是最需要它的时刻）。失败不致命：退回直接拷主文件。
    // `PRAGMA wal_checkpoint(TRUNCATE)` 是**带参数括号**的形式，不能用 `pragma_update`
    // （那会生成 `PRAGMA wal_checkpoint = TRUNCATE`，语法非法）。返回一行三列，结果忽略。
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()));

    // 本地时间戳由 SQLite 生成：不引入额外的日期库，也避免手写时区逻辑
    let stamp: String = conn
        .query_row(
            "SELECT strftime('%Y%m%d%H%M%S', 'now', 'localtime')",
            [],
            |row| row.get(0),
        )
        .map_err(|err| CoreError::Storage(format!("生成备份时间戳失败，已中止迁移：{err}")))?;

    let target = dir.join(format!("yunong.db.bak-v{from_version}-{stamp}"));
    std::fs::copy(db_path, &target).map_err(|err| {
        CoreError::Storage(format!(
            "备份数据库失败，已中止迁移（{}）：{err}",
            target.display()
        ))
    })?;
    cleanup_old_backups(&dir);
    Ok(target)
}

/// 备份保留份数（含本次新建的）。
const BACKUP_KEEP: usize = 3;

/// 只留最近 `BACKUP_KEEP` 份；清理失败**不影响**迁移（备份已经成功了）。
fn cleanup_old_backups(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<(String, std::path::PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stamp = name.rsplit_once('-').map(|(_, stamp)| stamp.to_owned())?;
            name.starts_with("yunong.db.bak-v")
                .then(|| (stamp, entry.path()))
        })
        .collect();
    // 时间戳是固定长度的数字串，字典序 == 时间序
    backups.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, path) in backups.into_iter().skip(BACKUP_KEEP) {
        let _ = std::fs::remove_file(path);
    }
}

/// 建表 + 逐级迁移；**成功后才写版本号**。
///
/// 顺序（故意如此）：
/// 1. 读现有版本（0 = 全新库或"没有版本机制的老库"）；
/// 2. 比程序新 → 直接报错（老程序不能打开新库，否则会写坏数据）；
/// 3. 需要升级时：老库先备份（失败即中止）；
/// 4. 跑幂等的 `SCHEMA`（全新库建表；老库是无操作）；
/// 5. 逐级跑 `MIGRATIONS`（每级一个事务）；
/// 6. 写 `schema_version`。
fn apply_schema_migrations(conn: &mut Connection, db_path: Option<&Path>) -> CoreResult<()> {
    conn.execute_batch(SCHEMA_META_DDL)
        .map_err(|err| CoreError::Storage(format!("建 schema_meta 失败：{err}")))?;
    let current = read_schema_version(conn)?;
    if current > SCHEMA_VERSION {
        return Err(CoreError::Storage(format!(
            "库的表结构版本是 {current}，比本程序支持的 {SCHEMA_VERSION} 更新：\
             请升级云农之窗后再打开（不降级，避免写坏数据）"
        )));
    }
    if current == SCHEMA_VERSION {
        return Ok(());
    }

    // 老库（已有用户表但没有版本号）才需要备份；全新库没东西可丢
    if current == 0 && has_user_tables(conn)? {
        let Some(path) = db_path else {
            // 内存库：没有文件可备份，也没有"老库升级"的语义
            return migrate_to_latest(conn, current);
        };
        backup_before_migration(conn, path, current)?;
    }
    migrate_to_latest(conn, current)
}

fn migrate_to_latest(conn: &mut Connection, from: i64) -> CoreResult<()> {
    conn.execute_batch(SCHEMA)
        .map_err(|err| CoreError::Storage(format!("建表失败：{err}")))?;
    for (version, sql) in MIGRATIONS {
        if *version <= from {
            continue;
        }
        let tx = conn
            .transaction()
            .map_err(|err| CoreError::Storage(format!("开迁移事务失败：{err}")))?;
        if !sql.trim().is_empty() {
            tx.execute_batch(sql)
                .map_err(|err| CoreError::Storage(format!("迁移到 v{version} 失败：{err}")))?;
        }
        write_schema_version(&tx, *version)?;
        tx.commit()
            .map_err(|err| CoreError::Storage(format!("提交迁移 v{version} 失败：{err}")))?;
    }
    // 迁移列表可能比 SCHEMA_VERSION 短（只抬版本不加 DDL 的情况）
    let latest = MIGRATIONS.last().map(|(version, _)| *version).unwrap_or(0);
    if latest < SCHEMA_VERSION {
        let tx = conn
            .transaction()
            .map_err(|err| CoreError::Storage(format!("开迁移事务失败：{err}")))?;
        write_schema_version(&tx, SCHEMA_VERSION)?;
        tx.commit()
            .map_err(|err| CoreError::Storage(format!("提交版本号失败：{err}")))?;
    }
    Ok(())
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
            Some(path),
        )
    }

    /// 内存库（测试用）。
    pub fn open_in_memory() -> CoreResult<Self> {
        Self::from_connection(
            Connection::open_in_memory()
                .map_err(|err| CoreError::Storage(format!("打开内存 SQLite 失败：{err}")))?,
            // 内存库没有文件可备份，也谈不上"老库升级"
            None,
        )
    }

    fn from_connection(mut conn: Connection, db_path: Option<&Path>) -> CoreResult<Self> {
        // WAL：读写不互相阻塞（会话进行中也要能查课表）。
        // 内存库不支持 WAL，失败就退回默认 journal，不算错。
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        apply_schema_migrations(&mut conn, db_path)?;
        Ok(Self { conn })
    }

    /// 当前库的**表结构版本**（`schema_meta.schema_version`）。
    ///
    /// 诊断/测试用；它与 `PRAGMA user_version`（派生数据回填）是两套东西。
    pub fn schema_version(&self) -> CoreResult<i64> {
        read_schema_version(&self.conn)
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

    // -----------------------------------------------------------------------
    // t163 后续：schema_meta 版本机制 + 迁移前备份
    // -----------------------------------------------------------------------

    /// 每个测试一个独立临时目录（避免并行测试互相覆盖备份目录）。
    struct TempDb {
        dir: std::path::PathBuf,
    }

    impl TempDb {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let dir = std::env::temp_dir()
                .join(format!("yn-store-{}-{tag}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn path(&self) -> std::path::PathBuf {
            self.dir.join("yunong.db")
        }

        fn backups(&self) -> std::path::PathBuf {
            self.dir.join("backups")
        }

        fn backup_names(&self) -> Vec<String> {
            let Ok(entries) = std::fs::read_dir(self.backups()) else {
                return Vec::new();
            };
            let mut names: Vec<String> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 把库改成"老库"：已有用户表 + 数据，但**没有** `schema_meta` 版本号
    /// （模拟引入版本机制之前装过云农之窗的机器）。
    fn make_legacy_db(path: &std::path::Path) {
        let store = CourseStore::open(path).unwrap();
        drop(store);
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "INSERT INTO terms (term, starts_on, ends_on) VALUES ('2026 秋', '2026-09-01', NULL);
             DELETE FROM schema_meta;",
        )
        .unwrap();
    }

    #[test]
    fn fresh_database_is_version_zero_and_migrates_to_latest() {
        let temp = TempDb::new("fresh");
        let path = temp.path();
        // 全新库：还没有 schema_meta → 视为 0
        {
            let conn = Connection::open(&path).unwrap();
            assert_eq!(read_schema_version(&conn).unwrap(), 0);
        }
        let store = CourseStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        // 全新库没有旧数据可丢 → 不该产生备份
        assert!(temp.backup_names().is_empty(), "{:?}", temp.backup_names());
    }

    /// 迁移只写 `schema_meta`，**绝不**碰 `PRAGMA user_version`（那是回填的进度号）。
    #[test]
    fn schema_migration_leaves_user_version_alone() {
        let temp = TempDb::new("user-version");
        let path = temp.path();
        make_legacy_db(&path);
        // 回填逻辑已经把这个库标成"派生数据回填到 v7"
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA user_version = 7;").unwrap();
        }
        let store = CourseStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let conn = Connection::open(&path).unwrap();
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            user_version, 7,
            "schema 迁移不许改 user_version（那是 backfill.rs 的回填进度）"
        );
    }

    #[test]
    fn legacy_database_is_backed_up_before_migration() {
        let temp = TempDb::new("backup");
        let path = temp.path();
        make_legacy_db(&path);
        let store = CourseStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let names = temp.backup_names();
        println!("[backup] {} / {:?}", temp.backups().display(), names);
        assert_eq!(names.len(), 1, "老库升级前必须留一份备份：{names:?}");
        assert!(names[0].starts_with("yunong.db.bak-v0-"), "{names:?}");
        assert!(temp.backups().join(&names[0]).is_file());
    }

    /// 备份失败 → **中止迁移**：open 报错，且库的版本号仍然停在 0（迁移没发生）。
    #[test]
    fn backup_failure_aborts_migration() {
        let temp = TempDb::new("backup-fail");
        let path = temp.path();
        make_legacy_db(&path);
        // 把 `backups` 占成一个**文件**：create_dir_all 必然失败
        std::fs::write(temp.backups(), b"not a directory").unwrap();

        let error = match CourseStore::open(&path) {
            Ok(_) => panic!("备份失败时必须中止迁移"),
            Err(error) => error,
        };
        let message = format!("{error}");
        assert!(message.contains("已中止迁移"), "{message}");

        // 迁移确实没发生：版本仍是 0（没有 schema_version 行）
        let conn = Connection::open(&path).unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);
    }

    /// 备份只保留最近 3 份（按文件名里的时间戳），更早的自动清理。
    #[test]
    fn backups_are_pruned_to_the_latest_three() {
        let temp = TempDb::new("prune");
        std::fs::create_dir_all(temp.backups()).unwrap();
        // 预先放 5 份"旧备份"，时间戳递增
        for stamp in [
            "20260101000000",
            "20260102000000",
            "20260103000000",
            "20260104000000",
            "20260105000000",
        ] {
            std::fs::write(
                temp.backups().join(format!("yunong.db.bak-v0-{stamp}")),
                b"old",
            )
            .unwrap();
        }
        let path = temp.path();
        make_legacy_db(&path);
        let store = CourseStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let names = temp.backup_names();
        assert_eq!(names.len(), BACKUP_KEEP, "只该留 3 份：{names:?}");
        // 最新的三份保留（新建的那份时间戳最大，一定在里面）
        assert!(
            names
                .iter()
                .any(|name| name.starts_with("yunong.db.bak-v0-2")),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|name| name.ends_with("20260101000000")),
            "最旧的应被清掉：{names:?}"
        );
    }

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
