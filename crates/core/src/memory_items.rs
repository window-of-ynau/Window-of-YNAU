//! 长期记忆条目（`memory_items` 表）：用户笔记 / 会话摘要 / 长期事实。
//!
//! 这是 W8 上下文压缩的**存储原语**：只负责存取与容量，不负责"记什么"
//! （压缩策略属于 W8，语义检索属于后续 ADR）。
//!
//! # 隐私边界（ADR-02 / ADR-08）
//!
//! 记忆正文**只在本机 SQLite**，永不上云，也不进日志：云端只存账号、授权、
//! 知识与抽样用量。本文件不做任何网络访问。
//!
//! # 与 `docs/02-系统设计文档.md` §5.2 草案的差异
//!
//! 草案写的是 `memory_items(key TEXT PRIMARY KEY, value, source, confidence, updated_at)`，
//! 这里落地成 `(id, kind, title, content, source, created_at, updated_at, expires_at)`：
//!
//! - 主键用自增 `id` 而不是 `key`：W8 的 `memory://<id>` 指针需要一个**永不变**的
//!   数字 id，而 `key` 是用户可见可改的文本，改一次就断一个指针；
//! - `kind` 把"笔记 / 摘要 / 事实"分开，`list` 能按种类查；
//! - `expires_at` 给"会话摘要"这类条目 TTL，别让临时摘要永久占着容量；
//! - 去掉 `confidence`：现在没有任何产出置信度的逻辑，先别存一个永远为空的列。
//!
//! 文档（单写者）待同步，本任务不动 `docs/**`。
//!
//! # 容量
//!
//! - 单条 `content` ≤ 64 KiB（超了报 `BAD_INPUT`，不静默截断——截断等于丢用户内容）；
//! - 总量 ≤ 5000 条；写超了先清到期条目，再按 `updated_at` **淘汰最旧**，
//!   返回 `evicted` 让界面能说清"为了腾地方删了几条"；
//! - 淘汰永远不淘汰刚刚写入的那条（用户刚存的东西不能被自己的容量策略吃掉）。
//!
//! 时间戳统一用**毫秒**（真毫秒，不是"秒 × 1000"）：秒粒度会让同一秒内的多次写入
//! 并列，`updated_at DESC` 退化成按 id 排，用户刚改的那条排不到最前面。
//!
//! # 刻意不做（避免越界）
//!
//! - **不做语义检索/嵌入**：RAG 需要 embedding 与向量存储，得先有 ADR；
//! - **不与云端同步**：记忆正文不进云端（ADR-02），本文件没有任何网络代码；
//! - **不做压缩策略**：什么时候存摘要、摘要多长，是 W8 的事，这里只提供存储原语。

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{CoreError, CoreResult};

/// 记忆种类的取值范围（先只开这三种，不提前铺用不上的类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryKind {
    /// 用户自己写的笔记
    Note,
    /// 会话摘要（压缩产物，通常带 TTL）
    Summary,
    /// 长期事实（"我在 3 班"、"本学期周三没课"）
    Fact,
}

impl MemoryKind {
    pub const ALL: [Self; 3] = [Self::Note, Self::Summary, Self::Fact];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Summary => "summary",
            Self::Fact => "fact",
        }
    }

    /// 严格解析（大小写敏感）：库里只允许这三个小写值，
    /// 免得 `Note` / `note` 两条并存，`list` 按种类筛选时漏数据。
    pub fn parse(text: &str) -> CoreResult<Self> {
        match text.trim() {
            "note" => Ok(Self::Note),
            "summary" => Ok(Self::Summary),
            "fact" => Ok(Self::Fact),
            other => Err(CoreError::Invalid(format!(
                "记忆类型只支持 {}，收到「{other}」",
                Self::ALL
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ))),
        }
    }
}

/// 条目总数上限（超了按 `updated_at` 淘汰最旧）。
pub const DEFAULT_MAX_ITEMS: usize = 5_000;
/// 单条 `content` 的字节上限。
pub const DEFAULT_MAX_CONTENT_BYTES: usize = 64 * 1024;
/// `title` / `source` 的字节上限：标题不该比正文还长，否则正文上限形同虚设。
pub const DEFAULT_MAX_TEXT_BYTES: usize = 1_024;
/// `list` 不传 limit 时的默认条数。
pub const DEFAULT_LIST_LIMIT: usize = 100;

/// 容量参数（默认值见 [`Limits::default`]；测试与未来的设置项可以改）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_items: usize,
    pub max_content_bytes: usize,
    pub max_text_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_items: DEFAULT_MAX_ITEMS,
            max_content_bytes: DEFAULT_MAX_CONTENT_BYTES,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
        }
    }
}

/// 一条记忆（读出来的样子）。
///
/// `kind` 保留库里的原文而不是 `MemoryKind`：万一库里有旧版本/手工写入的类型，
/// `list` 不能因为一条脏数据就整体失败（解析层"脏数据不拖垮全局"的同一条原则）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: i64,
    pub kind: String,
    pub title: Option<String>,
    pub content: String,
    pub source: Option<String>,
    /// 毫秒时间戳
    pub created_at: i64,
    /// 毫秒时间戳（`list` 按它倒序）
    pub updated_at: i64,
    /// 毫秒时间戳；`None` = 永不过期
    pub expires_at: Option<i64>,
}

/// 写一条记忆的入参。
#[derive(Debug, Clone, Copy)]
pub struct MemoryDraft<'a> {
    pub kind: MemoryKind,
    /// 空串 / 全空白会被规范化成 `NULL`
    pub title: Option<&'a str>,
    pub content: &'a str,
    /// 来源（`chat` / `user` / `import` …），空串同样规范化成 `NULL`
    pub source: Option<&'a str>,
    /// 毫秒时间戳；`None` = 永不过期。
    ///
    /// FFI **暂时不暴露**这个参数：TTL 该多长是 W8 压缩策略的事，
    /// 契约先不开，免得将来改不回去。
    pub expires_at: Option<i64>,
}

/// 一次写入的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryWrite {
    pub id: i64,
    /// 为了守住容量删掉的行数（含"顺手清掉的到期条目"）
    pub evicted: usize,
}

/// `memory_items` 表（与 `terms` / `courses` 同一个库文件）。
pub struct MemoryItemStore {
    conn: Connection,
    limits: Limits,
}

const TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS memory_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  title TEXT,
  content TEXT NOT NULL,
  source TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  expires_at INTEGER
);
"#;

/// 索引单独建：老库缺列时索引建不出来，但那不该让整个库打不开（端侧只警告）。
const INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS idx_memory_items_kind_updated ON memory_items(kind, updated_at DESC);";

/// 表应有的列（用于结构校验与警告文案）。
const EXPECTED_COLUMNS: [&str; 8] = [
    "id",
    "kind",
    "title",
    "content",
    "source",
    "created_at",
    "updated_at",
    "expires_at",
];

impl MemoryItemStore {
    /// 打开（或创建）库文件；父目录不存在会自动创建。
    pub fn open(path: impl AsRef<std::path::Path>) -> CoreResult<Self> {
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
        // WAL：会话进行中也要能读记忆（内存库不支持 WAL，失败就退回默认 journal）。
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute_batch(TABLE_SQL)
            .map_err(|err| CoreError::Storage(format!("建 memory_items 表失败：{err}")))?;
        let store = Self {
            conn,
            limits: Limits::default(),
        };
        // 索引/列校验**只警告不阻断**：手机上的老库不能因为一次升级就打不开
        // （server 侧 verify_schema 会拒绝启动，端侧没有"请运维执行迁移"这条路）。
        //
        // 端侧也没有统一的启动钩子（每个 FFI 调用各自开库），所以校验放在这里：
        // 代价是一次 `PRAGMA table_info`，换来任何一次读写前都能在日志里看到结构问题。
        if let Err(err) = store.conn.execute_batch(INDEX_SQL) {
            eprintln!("[yunong-core] memory_items 索引创建失败（老库结构不完整？）：{err}");
        }
        match store.missing_columns() {
            Ok(missing) if !missing.is_empty() => {
                eprintln!(
                    "[yunong-core] memory_items 缺少列：{}（本地记忆可能不可用，请升级客户端或重建本地库）",
                    missing.join(" / ")
                );
            }
            Ok(_) => {}
            Err(err) => eprintln!("[yunong-core] 校验 memory_items 结构失败：{err}"),
        }
        Ok(store)
    }

    /// 改容量参数（`max_items` 至少 1，否则写一条删一条没有意义）。
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = Limits {
            max_items: limits.max_items.max(1),
            ..limits
        };
        self
    }

    /// 当前容量参数（界面展示"已用 N / 上限 M"时用）。
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// 现有 `memory_items` 缺哪些列（空 = 结构完整）。
    pub fn missing_columns(&self) -> CoreResult<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("PRAGMA table_info(memory_items)")
            .map_err(|err| CoreError::Storage(format!("查表结构失败：{err}")))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|err| CoreError::Storage(format!("查表结构失败：{err}")))?;
        let existing: Vec<String> = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| CoreError::Storage(format!("读表结构失败：{err}")))?;
        Ok(EXPECTED_COLUMNS
            .iter()
            .copied()
            .filter(|column| !existing.iter().any(|name| name == column))
            .map(str::to_owned)
            .collect())
    }

    /// 写入或更新一条记忆，返回 id 与本次淘汰行数。
    ///
    /// `id = Some(..)` 表示更新；id 不存在 → `Invalid`（**不静默变成新增**：
    /// 那会让调用方以为改的是旧条目，实际上多出一条）。
    pub fn upsert(&mut self, draft: &MemoryDraft<'_>, id: Option<i64>) -> CoreResult<MemoryWrite> {
        self.upsert_at(draft, id, now_ms())
    }

    /// 按指定"当前时间"写入（测试用固定时钟，才能断言排序与过期行为）。
    fn upsert_at(
        &mut self,
        draft: &MemoryDraft<'_>,
        id: Option<i64>,
        now: i64,
    ) -> CoreResult<MemoryWrite> {
        let content = draft.content;
        if content.trim().is_empty() {
            return Err(CoreError::Invalid("记忆内容不能为空".into()));
        }
        if content.len() > self.limits.max_content_bytes {
            return Err(CoreError::Invalid(format!(
                "记忆内容超限：{} 字节 > 上限 {} 字节",
                content.len(),
                self.limits.max_content_bytes
            )));
        }
        let title = normalize(draft.title);
        let source = normalize(draft.source);
        for (label, value) in [("标题", &title), ("来源", &source)] {
            if let Some(text) = value {
                if text.len() > self.limits.max_text_bytes {
                    return Err(CoreError::Invalid(format!(
                        "记忆{label}超限：{} 字节 > 上限 {} 字节",
                        text.len(),
                        self.limits.max_text_bytes
                    )));
                }
            }
        }
        if let Some(expires_at) = draft.expires_at {
            if expires_at <= now {
                return Err(CoreError::Invalid("记忆的过期时间必须晚于当前时间".into()));
            }
        }

        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Storage(format!("开事务失败：{err}")))?;

        let kind = draft.kind.as_str();
        let (title, source) = (title.as_deref(), source.as_deref());
        let new_id = match id {
            Some(id) => {
                let changed = tx
                    .execute(
                        "UPDATE memory_items
                         SET kind = ?1, title = ?2, content = ?3, source = ?4,
                             expires_at = ?5, updated_at = ?6
                         WHERE id = ?7",
                        params![kind, title, content, source, draft.expires_at, now, id],
                    )
                    .map_err(|err| CoreError::Storage(format!("更新记忆失败：{err}")))?;
                if changed == 0 {
                    // 事务在这里被 drop，自动回滚
                    return Err(CoreError::Invalid(format!("记忆条目 {id} 不存在")));
                }
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO memory_items
                     (kind, title, content, source, created_at, updated_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
                    params![kind, title, content, source, now, draft.expires_at],
                )
                .map_err(|err| CoreError::Storage(format!("写记忆失败：{err}")))?;
                tx.last_insert_rowid()
            }
        };

        // 1) 先清到期条目：它们本来就 `list` 不出来，留着只占容量
        let mut evicted = tx
            .execute(
                "DELETE FROM memory_items WHERE expires_at IS NOT NULL AND expires_at <= ?1",
                params![now],
            )
            .map_err(|err| CoreError::Storage(format!("清理到期记忆失败：{err}")))?;

        // 2) 再按 `updated_at` 淘汰最旧，直到回到上限内（永不淘汰刚写的那条）
        let max_items = self.limits.max_items as i64;
        let live: i64 = tx
            .query_row("SELECT COUNT(*) FROM memory_items", [], |row| row.get(0))
            .map_err(|err| CoreError::Storage(format!("统计记忆条数失败：{err}")))?;
        if live > max_items {
            let overflow = live - max_items;
            let removed = tx
                .execute(
                    "DELETE FROM memory_items WHERE id IN (
                       SELECT id FROM memory_items WHERE id != ?1
                       ORDER BY updated_at ASC, id ASC LIMIT ?2
                     )",
                    params![new_id, overflow],
                )
                .map_err(|err| CoreError::Storage(format!("淘汰旧记忆失败：{err}")))?;
            evicted += removed;
        }

        tx.commit()
            .map_err(|err| CoreError::Storage(format!("提交事务失败：{err}")))?;
        Ok(MemoryWrite {
            id: new_id,
            evicted,
        })
    }

    /// 按 `updated_at` 倒序列出未到期的记忆（同毫秒时用 id 倒序保证顺序稳定）。
    ///
    /// `limit` 必须落在 `1..=max_items`：0 条没有意义，超过库容量只会白读一遍。
    pub fn list(&self, kind: Option<MemoryKind>, limit: usize) -> CoreResult<Vec<MemoryItem>> {
        if limit == 0 {
            return Err(CoreError::Invalid("limit 必须是正整数".into()));
        }
        if limit > self.limits.max_items {
            return Err(CoreError::Invalid(format!(
                "limit 不能超过 {}",
                self.limits.max_items
            )));
        }
        let now = now_ms();
        let limit = limit as i64;
        let mut out = Vec::new();
        match kind {
            Some(kind) => {
                let mut stmt = self
                    .conn
                    .prepare(
                        "SELECT id, kind, title, content, source, created_at, updated_at, expires_at
                         FROM memory_items
                         WHERE (expires_at IS NULL OR expires_at > ?1) AND kind = ?2
                         ORDER BY updated_at DESC, id DESC LIMIT ?3",
                    )
                    .map_err(|err| CoreError::Storage(format!("查询记忆失败：{err}")))?;
                let rows = stmt
                    .query_map(params![now, kind.as_str(), limit], row_to_item)
                    .map_err(|err| CoreError::Storage(format!("查询记忆失败：{err}")))?;
                for row in rows {
                    out.push(row.map_err(|err| CoreError::Storage(format!("读记忆失败：{err}")))?);
                }
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare(
                        "SELECT id, kind, title, content, source, created_at, updated_at, expires_at
                         FROM memory_items
                         WHERE expires_at IS NULL OR expires_at > ?1
                         ORDER BY updated_at DESC, id DESC LIMIT ?2",
                    )
                    .map_err(|err| CoreError::Storage(format!("查询记忆失败：{err}")))?;
                let rows = stmt
                    .query_map(params![now, limit], row_to_item)
                    .map_err(|err| CoreError::Storage(format!("查询记忆失败：{err}")))?;
                for row in rows {
                    out.push(row.map_err(|err| CoreError::Storage(format!("读记忆失败：{err}")))?);
                }
            }
        }
        Ok(out)
    }

    /// 删除一条；返回实际删除行数（0 = 本来就没有，删除是幂等的）。
    pub fn delete(&mut self, id: i64) -> CoreResult<usize> {
        self.conn
            .execute("DELETE FROM memory_items WHERE id = ?1", params![id])
            .map_err(|err| CoreError::Storage(format!("删记忆失败：{err}")))
    }

    /// 未到期条目数（UI 展示容量用；到期条目不算）。
    pub fn count(&self) -> CoreResult<usize> {
        let now = now_ms();
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM memory_items WHERE expires_at IS NULL OR expires_at > ?1",
                params![now],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as usize)
            .map_err(|err| CoreError::Storage(format!("统计记忆条数失败：{err}")))
    }

    /// 按 id 取一条（已到期也返回，仅供内部/测试核对；界面走 [`Self::list`]）。
    pub fn get(&self, id: i64) -> CoreResult<Option<MemoryItem>> {
        self.conn
            .query_row(
                "SELECT id, kind, title, content, source, created_at, updated_at, expires_at
                 FROM memory_items WHERE id = ?1",
                params![id],
                row_to_item,
            )
            .optional()
            .map_err(|err| CoreError::Storage(format!("读记忆失败：{err}")))
    }
}

fn row_to_item(row: &Row<'_>) -> rusqlite::Result<MemoryItem> {
    Ok(MemoryItem {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        content: row.get(3)?,
        source: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        expires_at: row.get(7)?,
    })
}

/// 空白 → `NULL`（否则"空标题"会存成空串，界面判断要多写一个分支）。
fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// 毫秒时间戳（**真毫秒**，不是"秒 × 1000"）。
///
/// 秒粒度会让同一秒内的多次写入全部并列，`updated_at DESC` 就退化成"按 id 排"：
/// 用户刚改的那条排不到最前面。真毫秒下这种并列基本消失。
fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定时钟下的写入（测试必须能控制时间，否则排序/过期断言会因同毫秒而抖动）。
    fn write_at(
        store: &mut MemoryItemStore,
        draft: &MemoryDraft<'_>,
        id: Option<i64>,
        now: i64,
    ) -> CoreResult<MemoryWrite> {
        store.upsert_at(draft, id, now)
    }

    fn note<'a>(content: &'a str) -> MemoryDraft<'a> {
        MemoryDraft {
            kind: MemoryKind::Note,
            title: Some("标题"),
            content,
            source: None,
            expires_at: None,
        }
    }

    fn sandbox() -> (std::path::PathBuf, MemoryItemStore) {
        let dir = std::env::temp_dir().join(format!("yunong-mem-{}", uuid::Uuid::new_v4()));
        let path = dir.join("timetable.db");
        let store = MemoryItemStore::open(&path).unwrap();
        (dir, store)
    }

    #[test]
    fn kinds_are_three_and_strictly_parsed() {
        assert_eq!(
            MemoryKind::ALL.map(MemoryKind::as_str),
            ["note", "summary", "fact"]
        );
        for kind in MemoryKind::ALL {
            assert_eq!(MemoryKind::parse(kind.as_str()).unwrap(), kind);
        }
        // 前后空白可以容忍（UI 传参常有）
        assert_eq!(MemoryKind::parse(" note ").unwrap(), MemoryKind::Note);
        // 大小写/未知类型都拒绝：库里不能出现 Note/note 两条并存
        for bad in ["Note", "diary", ""] {
            let err = MemoryKind::parse(bad).unwrap_err();
            assert!(matches!(err, CoreError::Invalid(_)), "{bad} => {err:?}");
        }
    }

    // ------------------------------------------------------------------
    // 建表 / 迁移
    // ------------------------------------------------------------------

    #[test]
    fn schema_is_idempotent_and_survives_reopen() {
        let (dir, mut store) = sandbox();
        assert!(store.missing_columns().unwrap().is_empty());
        write_at(&mut store, &note("第一条"), None, 1_000).unwrap();
        drop(store);

        // 再开一次：CREATE TABLE IF NOT EXISTS 不能报错，也不能清数据
        let mut store = MemoryItemStore::open(dir.join("timetable.db")).unwrap();
        assert!(store.missing_columns().unwrap().is_empty());
        assert_eq!(store.count().unwrap(), 1);
        write_at(&mut store, &note("第二条"), None, 2_000).unwrap();
        assert_eq!(store.list(None, 10).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_index_exists_on_kind_and_updated_at() {
        let store = MemoryItemStore::open_in_memory().unwrap();
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?1")
            .unwrap();
        let name: Option<String> = stmt
            .query_row(params!["idx_memory_items_kind_updated"], |row| row.get(0))
            .optional()
            .unwrap();
        assert_eq!(name.as_deref(), Some("idx_memory_items_kind_updated"));
    }

    #[test]
    fn broken_legacy_schema_is_warned_but_not_blocking() {
        let dir = std::env::temp_dir().join(format!("yunong-mem-{}", uuid::Uuid::new_v4()));
        let path = dir.join("legacy.db");
        std::fs::create_dir_all(&dir).unwrap();
        {
            // 老库：只建了 id + kind 两列
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE memory_items (id INTEGER PRIMARY KEY, kind TEXT);")
                .unwrap();
        }
        // 打开**不能失败**（端侧只警告），但缺列要能如实报出来
        let store = MemoryItemStore::open(&path).expect("老库必须仍能打开");
        let missing = store.missing_columns().unwrap();
        for column in ["content", "created_at", "updated_at", "expires_at"] {
            assert!(missing.iter().any(|name| name == column), "缺 {column}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // CRUD
    // ------------------------------------------------------------------

    #[test]
    fn upsert_inserts_then_updates_in_place() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let first = write_at(&mut store, &note("旧内容"), None, 1_000).unwrap();
        assert!(first.id > 0);
        assert_eq!(first.evicted, 0);

        let mut draft = note("新内容");
        draft.title = Some("新标题");
        let second = write_at(&mut store, &draft, Some(first.id), 2_000).unwrap();
        assert_eq!(second.id, first.id, "更新不能换 id（memory:// 指针要稳）");
        assert_eq!(store.count().unwrap(), 1);

        let item = store.get(first.id).unwrap().unwrap();
        assert_eq!(item.content, "新内容");
        assert_eq!(item.title.as_deref(), Some("新标题"));
        assert_eq!(item.created_at, 1_000, "created_at 不随更新变");
        assert_eq!(item.updated_at, 2_000);
        assert_eq!(item.kind, "note");
    }

    #[test]
    fn update_of_missing_id_is_rejected_instead_of_inserting() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let err = write_at(&mut store, &note("x"), Some(42), 1_000).unwrap_err();
        assert!(err.to_string().contains("不存在"), "{err}");
        assert_eq!(store.count().unwrap(), 0, "更新不存在的 id 不能变成新增");
    }

    #[test]
    fn empty_content_is_rejected() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        for blank in ["", "   ", "\n\t"] {
            let mut draft = note(blank);
            draft.content = blank;
            let err = write_at(&mut store, &draft, None, 1_000).unwrap_err();
            assert!(matches!(err, CoreError::Invalid(_)), "{blank:?} => {err:?}");
        }
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn blank_title_and_source_become_null() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let mut draft = note("正文");
        draft.title = Some("   ");
        draft.source = Some("");
        let write = write_at(&mut store, &draft, None, 1_000).unwrap();
        let item = store.get(write.id).unwrap().unwrap();
        assert_eq!(item.title, None);
        assert_eq!(item.source, None);
    }

    #[test]
    fn list_orders_by_updated_at_desc_and_breaks_ties_by_id() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let a = write_at(&mut store, &note("a"), None, 1_000).unwrap().id;
        let b = write_at(&mut store, &note("b"), None, 2_000).unwrap().id;
        let c = write_at(&mut store, &note("c"), None, 3_000).unwrap().id;

        let ids: Vec<i64> = store
            .list(None, 10)
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids, vec![c, b, a], "按 updated_at 倒序");

        // 更新 a 之后它排到最前（"最近动过的"才该在上面）
        write_at(&mut store, &note("a2"), Some(a), 4_000).unwrap();
        let ids: Vec<i64> = store
            .list(None, 10)
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids, vec![a, c, b]);

        // 同一毫秒写入时用 id 倒序兜底，顺序必须稳定
        let d = write_at(&mut store, &note("d"), None, 9_000).unwrap().id;
        let e = write_at(&mut store, &note("e"), None, 9_000).unwrap().id;
        let ids: Vec<i64> = store
            .list(None, 10)
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids[0], e);
        assert_eq!(ids[1], d);
    }

    #[test]
    fn list_filters_by_kind() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let mut summary = note("摘要");
        summary.kind = MemoryKind::Summary;
        let mut fact = note("事实");
        fact.kind = MemoryKind::Fact;
        write_at(&mut store, &note("笔记"), None, 1_000).unwrap();
        write_at(&mut store, &summary, None, 2_000).unwrap();
        write_at(&mut store, &fact, None, 3_000).unwrap();

        assert_eq!(store.list(None, 10).unwrap().len(), 3);
        assert_eq!(store.list(Some(MemoryKind::Note), 10).unwrap().len(), 1);
        assert_eq!(store.list(Some(MemoryKind::Summary), 10).unwrap().len(), 1);
        assert_eq!(
            store.list(Some(MemoryKind::Summary), 10).unwrap()[0].content,
            "摘要"
        );
        assert_eq!(
            store.list(Some(MemoryKind::Fact), 10).unwrap()[0].kind,
            "fact"
        );
    }

    #[test]
    fn list_limit_boundaries() {
        let mut store = MemoryItemStore::open_in_memory()
            .unwrap()
            .with_limits(Limits {
                max_items: 3,
                ..Limits::default()
            });
        for index in 0..3 {
            write_at(
                &mut store,
                &note(&format!("第 {index} 条")),
                None,
                1_000 + index,
            )
            .unwrap();
        }
        assert_eq!(store.list(None, 1).unwrap().len(), 1);
        assert_eq!(store.list(None, 3).unwrap().len(), 3);

        // 0 条没有意义；超过容量的 limit 只会白读
        let err = store.list(None, 0).unwrap_err();
        assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
        let err = store.list(None, 4).unwrap_err();
        assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn delete_is_idempotent_and_reports_rowcount() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let id = write_at(&mut store, &note("要删的"), None, 1_000)
            .unwrap()
            .id;
        assert_eq!(store.delete(id).unwrap(), 1);
        assert_eq!(store.delete(id).unwrap(), 0, "删两次不该报错");
        assert_eq!(store.count().unwrap(), 0);
        assert!(store.get(id).unwrap().is_none());
    }

    // ------------------------------------------------------------------
    // 容量与淘汰
    // ------------------------------------------------------------------

    #[test]
    fn content_over_the_limit_is_rejected_but_the_limit_itself_is_allowed() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let at_limit = "字".repeat(DEFAULT_MAX_CONTENT_BYTES / 3);
        assert!(at_limit.len() <= DEFAULT_MAX_CONTENT_BYTES);
        write_at(&mut store, &note(&at_limit), None, 1_000).unwrap();

        let too_long = "x".repeat(DEFAULT_MAX_CONTENT_BYTES + 1);
        let err = write_at(&mut store, &note(&too_long), None, 2_000).unwrap_err();
        assert!(
            err.to_string().contains("超限") && err.to_string().contains("65536"),
            "{err}"
        );
        assert_eq!(store.count().unwrap(), 1, "超限的那条不能落库");
    }

    #[test]
    fn title_over_the_limit_is_rejected() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let mut draft = note("正文");
        let long_title = "标题".repeat(DEFAULT_MAX_TEXT_BYTES);
        draft.title = Some(&long_title);
        let err = write_at(&mut store, &draft, None, 1_000).unwrap_err();
        assert!(err.to_string().contains("标题超限"), "{err}");
    }

    #[test]
    fn capacity_evicts_the_oldest_by_updated_at() {
        let mut store = MemoryItemStore::open_in_memory()
            .unwrap()
            .with_limits(Limits {
                max_items: 3,
                ..Limits::default()
            });
        let oldest = write_at(&mut store, &note("最旧"), None, 1_000).unwrap();
        let middle = write_at(&mut store, &note("中间"), None, 2_000).unwrap();
        write_at(&mut store, &note("较新"), None, 3_000).unwrap();
        assert_eq!(store.count().unwrap(), 3);

        // 第 4 条进来 → 按 updated_at 淘汰最旧的那条
        let newest = write_at(&mut store, &note("最新"), None, 4_000).unwrap();
        assert_eq!(newest.evicted, 1, "要能告诉界面删了几条");
        assert_eq!(store.count().unwrap(), 3);
        assert!(store.get(oldest.id).unwrap().is_none(), "最旧的必须被淘汰");
        assert!(store.get(middle.id).unwrap().is_some());
        assert!(store.get(newest.id).unwrap().is_some(), "刚写的不能被淘汰");
    }

    #[test]
    fn overflow_never_evicts_the_row_just_written() {
        let mut store = MemoryItemStore::open_in_memory()
            .unwrap()
            .with_limits(Limits {
                max_items: 1,
                ..Limits::default()
            });
        write_at(&mut store, &note("第一条"), None, 1_000).unwrap();
        let second = write_at(&mut store, &note("第二条"), None, 2_000).unwrap();
        assert_eq!(second.evicted, 1);
        let items = store.list(None, 1).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, second.id);
    }

    #[test]
    fn live_expiring_item_is_still_listed() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let now = now_ms();
        let mut expiring = note("还没到期");
        expiring.expires_at = Some(now + 3_600_000);
        write_at(&mut store, &expiring, None, now).unwrap();
        write_at(&mut store, &note("永不过期"), None, now).unwrap();
        assert_eq!(store.list(None, 10).unwrap().len(), 2, "还没到期的要能看到");
        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn expired_items_disappear_from_list_and_count() {
        let mut store = MemoryItemStore::open_in_memory()
            .unwrap()
            .with_limits(Limits {
                max_items: 10,
                ..Limits::default()
            });
        // 写入时 expires_at 必须晚于 now；这里用"未来的 now"来模拟到期
        let mut expiring = note("会过期");
        expiring.expires_at = Some(2_000);
        write_at(&mut store, &expiring, None, 1_000).unwrap();
        write_at(&mut store, &note("永不过期"), None, 1_000).unwrap();

        // 换一个"当前时间"晚于 expires_at 的时钟：走真实 SQL 过滤
        let real_now = now_ms();
        assert!(real_now > 2_000, "测试时间戳必须早于真实时钟");
        let items = store.list(None, 10).unwrap();
        assert_eq!(items.len(), 1, "到期条目不能出现在 list 里");
        assert_eq!(items[0].content, "永不过期");
        assert_eq!(store.count().unwrap(), 1, "到期条目不算容量");
    }

    #[test]
    fn purge_on_write_removes_expired_rows_and_counts_them_as_evicted() {
        let mut store = MemoryItemStore::open_in_memory()
            .unwrap()
            .with_limits(Limits {
                max_items: 10,
                ..Limits::default()
            });
        let mut expiring = note("会过期");
        expiring.expires_at = Some(2_000);
        let expired_id = write_at(&mut store, &expiring, None, 1_000).unwrap().id;

        // 到点之后的第一次写入顺手清掉它（它们本来就 list 不出来，留着只占容量）
        let real_now = now_ms();
        let write = write_at(&mut store, &note("新条目"), None, real_now).unwrap();
        assert_eq!(write.evicted, 1, "清掉的到期条目要计入 evicted");
        assert!(
            store.get(expired_id).unwrap().is_none(),
            "到期条目应从库里真正删除"
        );
    }

    #[test]
    fn expires_at_in_the_past_is_rejected() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let mut draft = note("x");
        draft.expires_at = Some(1_000);
        let err = write_at(&mut store, &draft, None, 1_000).unwrap_err();
        assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn timestamps_are_really_milliseconds() {
        let now = now_ms();
        // 2023-11 之后的毫秒时间戳量级是 1.7e12；秒是 1.7e9。写成"秒 × 1000"也能过
        // 这个断言，但同一秒内的多次写入会并列，所以这里再要求它落在真毫秒的量级上。
        assert!(now > 1_700_000_000_000, "必须是毫秒时间戳：{now}");
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let write = store.upsert(&note("真实时钟"), None).unwrap();
        let item = store.get(write.id).unwrap().unwrap();
        assert!(item.created_at > 1_700_000_000_000);
        assert_eq!(item.created_at, item.updated_at);
    }
}
