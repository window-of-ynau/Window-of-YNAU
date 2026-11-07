//! 端侧本地检索（ADR-19 决策 1）：SQLite 上的 BM25，**不做 embedding**。
//!
//! > 与 [`crate::search`] 区分：那个是**联网搜索**（`SearchProvider`，走服务端/上游/自带 Key）；
//! > 本模块是**本地语料**检索（自有文档与记忆，纯离线、永不出端）。两者无关，不要合并。
//!
//! # 为什么是 bigram，而不是分词库
//!
//! 1. **零依赖**：`jieba`/`tantivy` 这类库会把端侧二进制推大几十 MB，还会拖慢冷启动；
//!    我们的场景是"几十到几千份私有文档"，不值得为分词引入一个词典引擎。
//! 2. **中文没有空格**：不做任何切分就没法建倒排。字符 bigram（2 字滑窗）是**无词典**方案里
//!    最稳的一档——它天然覆盖未登录词（课程名、楼名、人名），不会因为词表缺失把"尚农楼"
//!    切成"尚/农/楼"而漏召回。
//! 3. **召回优先**：bigram 的召回率高于"单字"（更精确）且高于"整句"（更全），代价是索引变大
//!    （见下"膨胀率"）与精度略降（"学生"会命中"大学生"）。
//!
//! **已知局限（明确不做的事）**：
//! - 只做**词面匹配**：同义词（"餐厅" vs "食堂"）、语序、语义相关性一概不管（那是 embedding 的活）；
//! - **单个汉字的查询召回为 0**（孤立汉字除外）：倒排里只有 bigram，没有 unigram。
//!   要单字召回就得再存一份 unigram，倒排直接翻倍，不值当；查询请用 ≥2 个汉字。
//!
//! 分词实现复用 [`crate::index::tokenize`]（ADR-09 时代就有）：CJK 按 bigram、孤立汉字保留单字、
//! ASCII 字母数字按小写单词。
//!
//! # 表结构（与 `memory_items` / `courses` 同一个库文件）
//!
//! ```sql
//! CREATE TABLE search_docs (             -- 每篇文档一行
//!   id INTEGER PRIMARY KEY,              -- 数字 id：记忆页用 memory_items.id
//!   kind TEXT NOT NULL,                  -- note / summary / fact（与 memory_items 一致）
//!   title TEXT,
//!   content TEXT NOT NULL,               -- 原文（用于取 snippet；**不上云**，ADR-02）
//!   length INTEGER NOT NULL,             -- token 数 = BM25 的 dl
//!   updated_at INTEGER NOT NULL
//! );
//! CREATE TABLE search_postings (         -- 倒排表：term → (doc, 频次)
//!   term TEXT NOT NULL,
//!   doc_id INTEGER NOT NULL,
//!   freq INTEGER NOT NULL,
//!   PRIMARY KEY (term, doc_id)
//! ) WITHOUT ROWID;
//! ```
//!
//! 主键 `(term, doc_id)` 就是倒排需要的索引：查一个词 = 一次主键前缀范围扫描，
//! 顺手得到 `df`（扫描行数）。`WITHOUT ROWID` 让行直接活在索引里，省一份 rowid 开销。
//!
//! # 膨胀率
//!
//! 每篇文档产生的倒排行数 ≈ **不同 term 数**：中文按字符数 × ~0.6–1.0（L 个汉字 → L−1 个
//! bigram，重复的合并、标点不产生 bigram），英文按词数。实测（1000 篇 × ~115 汉字）：
//! **0.61 行/字符**、**~57 字节/字符**（含 term 文本、doc_id、freq、B-tree 开销），
//! 约 **6.6 KB/篇**。1000 篇 ≈ 6.6 MB；到万篇级要先把 `content` 挪出倒排表或分片。
//!
//! # 查询复杂度
//!
//! `q` 个不同 term、库中 `N` 篇文档：每个 term 一次主键前缀扫描 `O(df_t)`，
//! 总代价 `O(Σ df_t + 命中数 × 取长度)`，与 `N` 无直接关系（倒排的意义）；
//! 排序 `O(H log H)`，`H` = 候选数，之后截断到 `limit`。
//! 实测（1000 篇，本机 Windows，release）：**avg 1.6 ms / p50 1.1 ms / p95 5.3 ms**。
//!
//! # 增量更新
//!
//! [`SearchStore::upsert`] / [`SearchStore::delete`] 在**一个事务**里只动该文档的倒排行
//! （先删旧倒排再插新倒排），查询路径从不重建索引。

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::index::{bm25_idf, bm25_term_score, tokenize};
use crate::memory_items::{MemoryItem, MemoryKind};

/// snippet 的取词半径：命中词前后各 60 个**字符**（不是字节，避免劈开 UTF-8）。
pub const SNIPPET_RADIUS: usize = 60;

/// `search_local` 不传 limit 时的默认条数（与 FFI 契约一致）。
pub const DEFAULT_SEARCH_LIMIT: usize = 10;

/// `limit` 的硬上限：FFI 侧超出按 100 处理（**不报错**），内部 API 仍是严格校验。
pub const MAX_SEARCH_LIMIT: usize = 100;

/// 单篇文档的字节上限：再大就该先分块再入库，否则一次导入能把倒排表撑爆。
pub const MAX_DOC_BYTES: usize = 256 * 1024;

/// 单次查询的字符上限（R5-F2）。
///
/// 分词与倒排扫描都是**线性**的：实测 339 KB 的 query 在 debug 构建里要 200–305 ms，
/// 足够让移动端掉帧。512 字符已经远超正常查询（一句话），超限直接 `BAD_INPUT`——
/// 而且判断在**进入分词之前**，先 `take` 再比，别为超长串白算一遍。
pub const MAX_QUERY_CHARS: usize = 512;

/// 索引文档数上限（R5-F3）。与 `memory_items` 的容量策略一致：超了按 `updated_at` 淘汰最旧。
pub const MAX_INDEXED_DOCS: usize = 5_000;

/// 索引总量上限（R5-F3，按倒排近似字节算：每个 term 占 `term.len() + 24`）。
///
/// 64 MB 是"端侧本地私文档"能接受的上限：按实测 6 KB/篇（115 汉字）能放 1 万篇；
/// 满额 256 KiB 的长文档则先撞这条上限。
pub const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;

/// 分数保留的小数位（FFI 契约要求 4 位）。
const SCORE_DECIMALS: f64 = 10_000.0;

/// 一条检索结果（FFI 的 `data.hits[]` 元素）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    /// 文档 id（数字）：记忆页就是 `memory_items.id`
    pub id: i64,
    pub kind: String,
    pub title: Option<String>,
    /// 命中词附近 ±[`SNIPPET_RADIUS`] 字符；超出的部分用 `…` 标出
    pub snippet: String,
    /// BM25 分数，已保留 4 位小数（只用于排序，不要当概率用）。
    ///
    /// 用 `f64` 承载"4 位小数后的值"：`f32` 序列化成 JSON 会带
    /// `0.6100000143051147` 这种尾巴，而契约要求 `0.61`。
    pub score: f64,
}

/// 索引规模（测试与"设置页"展示用）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchStats {
    pub docs: usize,
    pub postings: usize,
    pub terms: usize,
    pub avg_len: f64,
}

/// 一篇待索引文档。
#[derive(Debug, Clone, Copy)]
pub struct SearchDoc<'a> {
    /// 数字 id（记忆页 = `memory_items.id`）。同一 id 反复 upsert = 覆盖。
    pub id: i64,
    /// 业务类型，透传给 UI；FFI 只接受 `note` / `summary` / `fact`（与记忆一致）
    pub kind: &'a str,
    pub title: Option<&'a str>,
    /// 正文（**只在本机**，ADR-02）
    pub content: &'a str,
}

const TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS search_docs (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  title TEXT,
  content TEXT NOT NULL,
  length INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  index_bytes INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS search_postings (
  term TEXT NOT NULL,
  doc_id INTEGER NOT NULL,
  freq INTEGER NOT NULL,
  PRIMARY KEY (term, doc_id)
) WITHOUT ROWID;
"#;

/// 索引单独建：老库结构不完整时只警告，不让整个库打不开（与 `memory_items` 一致）。
const INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS idx_search_docs_updated ON search_docs(updated_at DESC);";

/// 容量上限（测试里可以调小，不用插 5000 篇）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimits {
    pub docs: usize,
    pub bytes: usize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            docs: MAX_INDEXED_DOCS,
            bytes: MAX_INDEX_BYTES,
        }
    }
}

/// 一次 `upsert` 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpsertOutcome {
    /// 这篇文档的倒排行数（不同 term 数）
    pub terms: usize,
    /// 为了腾容量淘汰掉的最旧文档数（正常是 0）
    pub evicted: usize,
}

/// 本地 BM25 索引（SQLite 持久化）。
pub struct SearchStore {
    conn: Connection,
    limits: SearchLimits,
}

impl SearchStore {
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

    /// 调小容量上限（测试用；生产用默认值）。
    pub fn with_limits(mut self, limits: SearchLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 当前容量上限。
    pub fn limits(&self) -> SearchLimits {
        self.limits
    }

    fn from_connection(conn: Connection) -> CoreResult<Self> {
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute_batch(TABLE_SQL)
            .map_err(|err| CoreError::Storage(format!("建检索表失败：{err}")))?;
        if let Err(err) = conn.execute_batch(INDEX_SQL) {
            eprintln!("[yunong-core] search_docs 索引创建失败（老库结构不完整？）：{err}");
        }
        // 老库没有 `index_bytes` 列（R5-F3 之前的库）：补一列，补不上只警告。
        // 补上之后老文档该列是 0，容量统计会低估——但总数上限仍然生效，不会失控。
        if let Err(err) = conn.execute_batch(
            "ALTER TABLE search_docs ADD COLUMN index_bytes INTEGER NOT NULL DEFAULT 0;",
        ) {
            let text = err.to_string();
            if !text.contains("duplicate column name") {
                eprintln!("[yunong-core] search_docs 补 index_bytes 列失败：{err}");
            }
        }
        Ok(Self {
            conn,
            limits: SearchLimits::default(),
        })
    }

    /// 写一篇文档：删旧倒排 → 重建该文档的倒排 → 覆盖文档行。**只动这一篇**。
    ///
    /// 返回写入的倒排行数（= 不同 term 数），便于测试与容量观测。
    pub fn upsert(&mut self, doc: &SearchDoc<'_>) -> CoreResult<UpsertOutcome> {
        if doc.id <= 0 {
            return Err(CoreError::Invalid("文档 id 必须是正整数".into()));
        }
        let kind = doc.kind.trim();
        if kind.is_empty() {
            return Err(CoreError::Invalid("文档 kind 不能为空".into()));
        }
        if doc.content.len() > MAX_DOC_BYTES {
            return Err(CoreError::Invalid(format!(
                "文档正文超限：{} 字节 > 上限 {} 字节（先分块再入库）",
                doc.content.len(),
                MAX_DOC_BYTES
            )));
        }

        // 标题也进索引：用户搜的常常是标题；但 snippet 只从正文取
        let mut searchable = String::with_capacity(doc.content.len() + 64);
        if let Some(title) = doc.title {
            searchable.push_str(title);
            searchable.push('\n');
        }
        searchable.push_str(doc.content);
        let tokens = tokenize(&searchable);
        let length = tokens.len() as u32;
        let mut freq: HashMap<&str, u32> = HashMap::new();
        for token in &tokens {
            *freq.entry(token.as_str()).or_insert(0) += 1;
        }
        // 倒排写进库之后大约占多少字节（term + doc_id + freq + B-tree 分摊，取 24 字节/行）
        let index_bytes: usize = freq.keys().map(|term| term.len() + 24).sum();
        let terms = freq.len();

        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Storage(format!("开事务失败：{err}")))?;
        tx.execute(
            "DELETE FROM search_postings WHERE doc_id = ?1",
            params![doc.id],
        )
        .map_err(|err| CoreError::Storage(format!("清旧倒排失败：{err}")))?;
        {
            let mut stmt = tx
                .prepare("INSERT INTO search_postings (term, doc_id, freq) VALUES (?1, ?2, ?3)")
                .map_err(|err| CoreError::Storage(format!("写倒排失败：{err}")))?;
            for (term, count) in &freq {
                stmt.execute(params![term, doc.id, *count as i64])
                    .map_err(|err| CoreError::Storage(format!("写倒排失败：{err}")))?;
            }
        }
        tx.execute(
            "INSERT INTO search_docs (id, kind, title, content, length, updated_at, index_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
               kind = excluded.kind, title = excluded.title, content = excluded.content,
               length = excluded.length, updated_at = excluded.updated_at,
               index_bytes = excluded.index_bytes",
            params![
                doc.id,
                kind,
                doc.title,
                doc.content,
                length as i64,
                now_ms(),
                index_bytes as i64
            ],
        )
        .map_err(|err| CoreError::Storage(format!("写文档失败：{err}")))?;
        tx.commit()
            .map_err(|err| CoreError::Storage(format!("提交事务失败：{err}")))?;
        // 容量兜底（R5-F3）：写完之后再判，保证"刚写进来的这条"不会被自己的容量策略吃掉
        let evicted = self.enforce_limits(doc.id)?;
        Ok(UpsertOutcome { terms, evicted })
    }

    /// 超限就按 `updated_at` 淘汰最旧，直到回到上限以内（永不淘汰 `keep`）。
    ///
    /// 与 `memory_items` 的容量策略一致：先保"用户刚存的"，再淘汰最旧的。
    fn enforce_limits(&mut self, keep: i64) -> CoreResult<usize> {
        let mut evicted = 0_usize;
        loop {
            let (docs, bytes): (i64, i64) = self
                .conn
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(index_bytes), 0) FROM search_docs",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|err| CoreError::Storage(format!("统计索引规模失败：{err}")))?;
            let within_docs = (docs.max(0) as usize) <= self.limits.docs;
            let within_bytes = (bytes.max(0) as usize) <= self.limits.bytes;
            if within_docs && within_bytes {
                return Ok(evicted);
            }
            let victim: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM search_docs WHERE id != ?1 ORDER BY updated_at ASC, id ASC LIMIT 1",
                    params![keep],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| CoreError::Storage(format!("挑淘汰对象失败：{err}")))?;
            match victim {
                Some(id) => {
                    self.delete(id)?;
                    evicted += 1;
                }
                // 只剩刚写的那条还超限（单篇就超过总量上限）：不再淘汰，交给调用方处理
                None => return Ok(evicted),
            }
        }
    }

    /// 删一篇文档（倒排 + 文档行），返回实际删除的文档行数（0 = 本来没有）。
    pub fn delete(&mut self, id: i64) -> CoreResult<usize> {
        if id <= 0 {
            return Err(CoreError::Invalid("文档 id 必须是正整数".into()));
        }
        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Storage(format!("开事务失败：{err}")))?;
        tx.execute("DELETE FROM search_postings WHERE doc_id = ?1", params![id])
            .map_err(|err| CoreError::Storage(format!("删倒排失败：{err}")))?;
        let deleted = tx
            .execute("DELETE FROM search_docs WHERE id = ?1", params![id])
            .map_err(|err| CoreError::Storage(format!("删文档失败：{err}")))?;
        tx.commit()
            .map_err(|err| CoreError::Storage(format!("提交事务失败：{err}")))?;
        Ok(deleted)
    }

    /// 把一条本地记忆（`memory_items`）编进索引：id 就是 `memory_items.id`，kind 原样。
    ///
    /// 记忆写入/更新/删除后调这个即可保持索引同步（增量，不重建）。
    pub fn index_memory_item(&mut self, item: &MemoryItem) -> CoreResult<UpsertOutcome> {
        // kind 只允许三种记忆类型；库里万一有脏值就当 note 处理（不阻断索引）
        let kind = MemoryKind::parse(&item.kind).unwrap_or(MemoryKind::Note);
        self.upsert(&SearchDoc {
            id: item.id,
            kind: kind.as_str(),
            title: item.title.as_deref(),
            content: &item.content,
        })
    }

    /// 把一条本地记忆移出索引。
    pub fn unindex_memory_item(&mut self, memory_id: i64) -> CoreResult<usize> {
        self.delete(memory_id)
    }

    /// 库里有几篇文档（空 = 没有索引）。
    pub fn len(&self) -> CoreResult<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM search_docs", [], |row| row.get(0))
            .map_err(|err| CoreError::Storage(format!("统计文档数失败：{err}")))?;
        Ok(count as usize)
    }

    pub fn is_empty(&self) -> CoreResult<bool> {
        Ok(self.len()? == 0)
    }

    /// 索引规模（文档数 / 倒排行数 / 不同 term 数 / 平均长度）。
    pub fn stats(&self) -> CoreResult<SearchStats> {
        let docs: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM search_docs", [], |row| row.get(0))
            .map_err(|err| CoreError::Storage(format!("统计文档数失败：{err}")))?;
        let postings: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM search_postings", [], |row| row.get(0))
            .map_err(|err| CoreError::Storage(format!("统计倒排失败：{err}")))?;
        let terms: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT term) FROM search_postings",
                [],
                |row| row.get(0),
            )
            .map_err(|err| CoreError::Storage(format!("统计词表失败：{err}")))?;
        let avg_len: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(AVG(length), 0) FROM search_docs",
                [],
                |row| row.get(0),
            )
            .map_err(|err| CoreError::Storage(format!("统计平均长度失败：{err}")))?;
        Ok(SearchStats {
            docs: docs as usize,
            postings: postings as usize,
            terms: terms as usize,
            avg_len,
        })
    }

    /// BM25 检索：按分数降序返回前 `limit` 条（同分按 id 升序，保证结果稳定）。
    ///
    /// - 空查询 / 只含标点 / 无命中 → 空结果（**不是错误**）；
    /// - `limit` 必须在 `1..=MAX_SEARCH_LIMIT`（FFI 侧把超限夹到 100）；
    /// - 命中词只出现在标题时也会返回，snippet 退化成正文开头。
    pub fn search(&self, query: &str, limit: usize) -> CoreResult<Vec<SearchResult>> {
        // R5-F2：长度上限必须在**分词之前**判。只数到上限+1 个字符就停，
        // 不为 300 KB 的串白算一遍（`.chars().count()` 会整串扫）。
        if query.chars().take(MAX_QUERY_CHARS + 1).count() > MAX_QUERY_CHARS {
            return Err(CoreError::Invalid(format!(
                "查询太长：超过 {MAX_QUERY_CHARS} 个字符（实际 ≥{}）",
                MAX_QUERY_CHARS + 1
            )));
        }
        if limit == 0 {
            return Err(CoreError::Invalid("limit 必须是正整数".into()));
        }
        if limit > MAX_SEARCH_LIMIT {
            return Err(CoreError::Invalid(format!(
                "limit 不能超过 {MAX_SEARCH_LIMIT}"
            )));
        }
        let terms = unique_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let doc_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM search_docs", [], |row| row.get(0))
            .map_err(|err| CoreError::Storage(format!("统计文档数失败：{err}")))?;
        if doc_count == 0 {
            return Ok(Vec::new());
        }
        let avg_len: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(AVG(length), 0) FROM search_docs",
                [],
                |row| row.get(0),
            )
            .map_err(|err| CoreError::Storage(format!("统计平均长度失败：{err}")))?;

        let mut scores: HashMap<i64, f32> = HashMap::new();
        let mut lengths: HashMap<i64, u32> = HashMap::new();
        for term in &terms {
            let mut stmt = self
                .conn
                .prepare("SELECT doc_id, freq FROM search_postings WHERE term = ?1")
                .map_err(|err| CoreError::Storage(format!("查倒排失败：{err}")))?;
            let rows: Vec<(i64, u32)> = stmt
                .query_map(params![term], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as u32))
                })
                .map_err(|err| CoreError::Storage(format!("查倒排失败：{err}")))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| CoreError::Storage(format!("读倒排失败：{err}")))?;
            let idf = bm25_idf(doc_count as u32, rows.len() as u32);
            for (doc_id, freq) in rows {
                let doc_len = match lengths.get(&doc_id) {
                    Some(len) => *len,
                    None => {
                        let len: i64 = self
                            .conn
                            .query_row(
                                "SELECT length FROM search_docs WHERE id = ?1",
                                params![doc_id],
                                |row| row.get(0),
                            )
                            .map_err(|err| CoreError::Storage(format!("读文档长度失败：{err}")))?;
                        let len = len.max(0) as u32;
                        lengths.insert(doc_id, len);
                        len
                    }
                };
                let contribution = bm25_term_score(freq, doc_len, avg_len as f32, idf);
                if contribution > 0.0 {
                    *scores.entry(doc_id).or_insert(0.0) += contribution;
                }
            }
        }
        if scores.is_empty() {
            return Ok(Vec::new());
        }

        let mut ranked: Vec<(i64, f32)> = scores.into_iter().collect();
        // `total_cmp` 而不是 `partial_cmp`：分数是有限值，但为了排序**全序**（同分再按 id）
        // 和可复现，这里用 f32 的全序比较，避免 NaN 让 `sort_by` 的次序不稳定。
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked.truncate(limit);

        let mut out = Vec::with_capacity(ranked.len());
        for (id, score) in ranked {
            let (kind, title, content): (String, Option<String>, String) = self
                .conn
                .query_row(
                    "SELECT kind, title, content FROM search_docs WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(|err| CoreError::Storage(format!("读文档失败：{err}")))?;
            // 先算出 snippet（借用 title/content），再交出 title 的所有权
            let snippet = {
                let source = if content.trim().is_empty() {
                    title.as_deref().unwrap_or_default()
                } else {
                    content.as_str()
                };
                snippet_for(source, &terms)
            };
            out.push(SearchResult {
                id,
                kind,
                title,
                snippet,
                // 契约：分数保留 4 位小数；在 f64 上取整，JSON 里才会是 `0.61`
                // 而不是 f32 的 `0.6100000143051147`
                score: ((score as f64) * SCORE_DECIMALS).round() / SCORE_DECIMALS,
            });
        }
        Ok(out)
    }
}

/// 查询词去重：同一个 term 重复出现不该把分数翻倍。
fn unique_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    tokenize(query)
        .into_iter()
        .filter(|term| seen.insert(term.clone()))
        .collect()
}

/// 取命中词附近 ±[`SNIPPET_RADIUS`] 个字符。
///
/// 全部按**字符**下标切片（`chars()`），不会把多字节字符劈成半个；横竖换行拍平成空格。
fn snippet_for(text: &str, terms: &[String]) -> String {
    let hit = terms.iter().filter_map(|term| find_term(text, term)).min();
    let total = text.chars().count();
    let hit_char = hit.map_or(0, |byte| text[..byte].chars().count());
    let start = hit_char.saturating_sub(SNIPPET_RADIUS);
    let end = (hit_char + SNIPPET_RADIUS).min(total);

    let mut out = String::with_capacity(end.saturating_sub(start) + 2);
    if start > 0 {
        out.push('…');
    }
    for ch in text.chars().skip(start).take(end - start) {
        out.push(if ch == '\n' || ch == '\r' { ' ' } else { ch });
    }
    if end < total {
        out.push('…');
    }
    out
}

/// 找一个 term 在文本里的**字节**偏移。
///
/// ASCII 词大小写不敏感（`to_ascii_lowercase` 不改变字节长度，偏移可直接用）；
/// CJK bigram 原样匹配（没有大小写概念）。
fn find_term(haystack: &str, term: &str) -> Option<usize> {
    if term.is_empty() {
        return None;
    }
    if term.is_ascii() {
        haystack.to_ascii_lowercase().find(term)
    } else {
        haystack.find(term)
    }
}

fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SearchStore {
        SearchStore::open_in_memory().unwrap()
    }

    fn add(
        store: &mut SearchStore,
        id: i64,
        kind: &str,
        title: &str,
        content: &str,
    ) -> UpsertOutcome {
        let doc = SearchDoc {
            id,
            kind,
            title: Some(title),
            content,
        };
        store.upsert(&doc).unwrap()
    }

    fn ids(results: &[SearchResult]) -> Vec<i64> {
        results.iter().map(|hit| hit.id).collect()
    }

    // ------------------------------------------------------------------
    // 召回
    // ------------------------------------------------------------------

    #[test]
    fn chinese_bigram_recall() {
        let mut store = store();
        add(
            &mut store,
            1,
            "note",
            "课表提醒",
            "下午第3节有高等数学，东校区1教305，张老师",
        );
        add(
            &mut store,
            2,
            "fact",
            "图书馆",
            "图书馆开放时间：周一至周日 8:00-22:00",
        );
        add(
            &mut store,
            3,
            "summary",
            "培养方案",
            "培养方案要求：高等数学为必修课，6 学分",
        );

        // 4 字查询切成 bigram 后能命中两篇
        let hits = store.search("高等数学", 10).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(ids(&hits).contains(&1) && ids(&hits).contains(&3));

        // 2 字查询（单个 bigram）也能召回
        let hits = store.search("数学", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(store.search("量子力学", 10).unwrap().is_empty());
    }

    /// bigram 的**已知局限**：单个汉字不在倒排里（除孤立汉字），所以单字查询召回为 0。
    /// 这是设计取舍（要单字召回就得再存一份 unigram，倒排直接翻倍），文档里写清即可。
    #[test]
    fn single_cjk_char_query_does_not_match_runs() {
        let mut store = store();
        add(&mut store, 1, "note", "课表", "周三下午三门课");
        assert!(
            store.search("课", 10).unwrap().is_empty(),
            "单字不在 bigram 倒排里"
        );
        assert_eq!(ids(&store.search("三门", 10).unwrap()), vec![1]);

        // 孤立汉字（整段正文只有一个字）会以单字入库，仍能命中
        let mut alone = SearchStore::open_in_memory().unwrap();
        add(&mut alone, 2, "note", "标题", "课");
        assert_eq!(ids(&alone.search("课", 10).unwrap()), vec![2]);
    }

    #[test]
    fn english_word_recall() {
        let mut store = store();
        add(
            &mut store,
            1,
            "note",
            "Library hours",
            "The library opens at 8:00 and closes at 22:00.",
        );
        add(
            &mut store,
            2,
            "note",
            "Canteen",
            "The canteen serves lunch from 11:30.",
        );
        let hits = store.search("library", 10).unwrap();
        assert_eq!(ids(&hits), vec![1]);
        // 大小写不敏感
        assert_eq!(ids(&store.search("LIBRARY", 10).unwrap()), vec![1]);
        // 词内不误命中
        assert!(store.search("libr", 10).unwrap().is_empty());
    }

    #[test]
    fn mixed_chinese_and_english_query() {
        let mut store = store();
        add(
            &mut store,
            1,
            "note",
            "GPA 计算",
            "GPA 计算方式：加权平均，体育课不计入。",
        );
        add(
            &mut store,
            2,
            "note",
            "选课",
            "选课系统开放到本周五，注意冲突。",
        );
        let hits = store.search("GPA 计算", 10).unwrap();
        assert_eq!(ids(&hits), vec![1], "{hits:?}");
        let hits = store.search("gpa 选课", 10).unwrap();
        // 两个词各命中一篇，按分数排序都应在结果里
        assert_eq!(hits.len(), 2, "{hits:?}");
    }

    #[test]
    fn empty_and_punctuation_queries_return_nothing() {
        let mut store = store();
        add(&mut store, 1, "note", "笔记", "高等数学笔记");
        assert!(store.search("", 10).unwrap().is_empty());
        assert!(store.search("   ", 10).unwrap().is_empty());
        assert!(store.search("，。！", 10).unwrap().is_empty());
        assert!(store.search("不存在的内容", 10).unwrap().is_empty());
    }

    #[test]
    fn title_is_searchable_but_snippet_comes_from_content() {
        let mut store = store();
        add(
            &mut store,
            1,
            "note",
            "尚农楼",
            "该楼在东校区，共有五层，一层是实验室。",
        );
        let hits = store.search("尚农楼", 10).unwrap();
        assert_eq!(ids(&hits), vec![1]);
        assert!(
            hits[0].snippet.contains("东校区"),
            "snippet 应来自正文：{}",
            hits[0].snippet
        );
    }

    // ------------------------------------------------------------------
    // 增量更新
    // ------------------------------------------------------------------

    #[test]
    fn incremental_upsert_replaces_postings() {
        let mut store = store();
        add(&mut store, 1, "note", "笔记", "苹果香蕉");
        assert_eq!(ids(&store.search("苹果", 5).unwrap()), vec![1]);

        // 同一 id 换内容：旧词必须消失（否则删除/编辑后还能搜到旧内容）
        add(&mut store, 1, "note", "笔记", "橘子西瓜");
        assert!(
            store.search("苹果", 5).unwrap().is_empty(),
            "旧倒排必须被清掉"
        );
        assert_eq!(ids(&store.search("橘子", 5).unwrap()), vec![1]);
        assert_eq!(store.len().unwrap(), 1, "覆盖不是新增");
        let stats = store.stats().unwrap();
        assert_eq!(stats.postings, stats.terms, "单文档时倒排行数 = 词数");
    }

    #[test]
    fn incremental_delete_removes_doc_and_postings() {
        let mut store = store();
        add(&mut store, 1, "note", "a", "高等数学");
        add(&mut store, 2, "note", "b", "线性代数");
        assert_eq!(store.delete(1).unwrap(), 1);
        assert_eq!(store.delete(1).unwrap(), 0, "删除幂等");
        assert!(store.search("高等数学", 5).unwrap().is_empty());
        assert_eq!(ids(&store.search("线性代数", 5).unwrap()), vec![2]);
        assert_eq!(store.len().unwrap(), 1);
        // 倒排里不该留孤儿行
        let orphan: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM search_postings WHERE doc_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan, 0);
    }

    #[test]
    fn index_and_unindex_memory_item() {
        let item = MemoryItem {
            id: 7,
            kind: "fact".into(),
            title: Some("班级".into()),
            content: "我在 3 班（水利水电工程）".into(),
            source: Some("user".into()),
            created_at: 1,
            updated_at: 2,
            expires_at: None,
        };
        let mut store = store();
        store.index_memory_item(&item).unwrap();
        let hits = store.search("水利水电", 5).unwrap();
        assert_eq!(ids(&hits), vec![7]);
        assert_eq!(hits[0].kind, "fact");
        assert_eq!(store.unindex_memory_item(7).unwrap(), 1);
        assert!(store.search("水利水电", 5).unwrap().is_empty());
    }

    // ------------------------------------------------------------------
    // 边界
    // ------------------------------------------------------------------

    #[test]
    fn limit_boundaries() {
        let mut store = store();
        for index in 1..=3 {
            add(&mut store, index, "note", "笔记", "高等数学笔记");
        }
        assert_eq!(store.search("数学", 1).unwrap().len(), 1);
        assert_eq!(store.search("数学", 3).unwrap().len(), 3);
        assert_eq!(store.search("数学", MAX_SEARCH_LIMIT).unwrap().len(), 3);

        let error = store.search("数学", 0).unwrap_err();
        assert!(matches!(error, CoreError::Invalid(_)), "{error:?}");
        let error = store.search("数学", MAX_SEARCH_LIMIT + 1).unwrap_err();
        assert!(matches!(error, CoreError::Invalid(_)), "{error:?}");
    }

    // ------------------------------------------------------------------
    // 容量上限（R5-F2 / R5-F3）
    // ------------------------------------------------------------------

    /// R5-F2：超过 512 字符的 query 直接拒绝，且**在分词之前**就停。
    #[test]
    fn query_longer_than_cap_is_rejected_before_tokenizing() {
        let mut store = store();
        add(&mut store, 1, "note", "笔记", "高等数学笔记");

        // 正好 512 字符：允许（会命中很多次，但不会被拒）
        let at_cap = "数学".repeat(MAX_QUERY_CHARS / 2);
        assert_eq!(at_cap.chars().count(), MAX_QUERY_CHARS);
        assert!(store.search(&at_cap, 5).is_ok(), "512 字符应当放行");

        // 513 字符：拒绝
        let over = "数".repeat(MAX_QUERY_CHARS + 1);
        let error = store.search(&over, 5).unwrap_err();
        assert!(matches!(error, CoreError::Invalid(_)), "{error:?}");
        assert!(error.to_string().contains("查询太长"), "{error}");

        // 339 KB（security 实测的那个量级）：必须立刻拒绝，不能先分词
        let huge = "数".repeat(339 * 1024 / 3);
        let started = std::time::Instant::now();
        let error = store.search(&huge, 5).unwrap_err();
        let elapsed = started.elapsed();
        assert!(matches!(error, CoreError::Invalid(_)), "{error:?}");
        assert!(
            elapsed < std::time::Duration::from_millis(5),
            "拒绝超长 query 花了 {elapsed:?}，说明没有在分词前拦住"
        );
    }

    /// R5-F3：文档数超限 → 按 `updated_at` 淘汰最旧，并在结果里报 `evicted`。
    #[test]
    fn index_evicts_oldest_when_doc_cap_is_exceeded() {
        let mut store = store().with_limits(SearchLimits {
            docs: 3,
            bytes: usize::MAX,
        });
        for id in 1..=3 {
            add(&mut store, id, "note", "笔记", "高等数学");
        }
        assert_eq!(store.len().unwrap(), 3);

        let outcome = add(&mut store, 4, "note", "笔记", "线性代数");
        assert_eq!(outcome.evicted, 1, "超限该淘汰 1 篇");
        assert_eq!(store.len().unwrap(), 3, "淘汰后回到上限");
        assert!(
            store
                .search("数学", 5)
                .unwrap()
                .iter()
                .all(|hit| hit.id != 1),
            "淘汰的是最旧的（id=1）"
        );
        assert_eq!(
            ids(&store.search("线性代数", 5).unwrap()),
            vec![4],
            "新写的不受影响"
        );
    }

    /// R5-F3：索引字节超限也淘汰（不只看篇数）。
    #[test]
    fn index_evicts_oldest_when_byte_cap_is_exceeded() {
        let mut store = store().with_limits(SearchLimits {
            docs: usize::MAX,
            bytes: 400,
        });
        // 每篇 8 个汉字 → 7 个 bigram → 7 ×（6+24）= 210 字节
        let first = add(&mut store, 1, "note", "", "高等数学线性代数");
        assert_eq!(first.evicted, 0);
        // 第二篇把总量顶到 420 > 400 → 淘汰最旧的（不是刚写的）
        let second = add(&mut store, 2, "note", "", "概率论与数理统计");
        assert_eq!(second.evicted, 1);
        assert_eq!(store.len().unwrap(), 1);
        assert_eq!(
            ids(&store.search("概率", 5).unwrap()),
            vec![2],
            "刚写的一定还在"
        );
    }

    /// 更新已有文档不该触发淘汰（总数没变）。
    #[test]
    fn updating_existing_doc_does_not_evict() {
        let mut store = store().with_limits(SearchLimits {
            docs: 2,
            bytes: usize::MAX,
        });
        add(&mut store, 1, "note", "笔记", "高等数学");
        add(&mut store, 2, "note", "笔记", "线性代数");
        let outcome = add(&mut store, 1, "note", "笔记", "高等数学 第二版");
        assert_eq!(outcome.evicted, 0);
        assert_eq!(store.len().unwrap(), 2);
    }

    #[test]
    fn invalid_document_input() {
        let mut store = store();
        let zero = SearchDoc {
            id: 0,
            kind: "note",
            title: None,
            content: "x",
        };
        assert!(matches!(
            store.upsert(&zero).unwrap_err(),
            CoreError::Invalid(_)
        ));
        let blank_kind = SearchDoc {
            id: 1,
            kind: " ",
            title: None,
            content: "x",
        };
        assert!(matches!(
            store.upsert(&blank_kind).unwrap_err(),
            CoreError::Invalid(_)
        ));
        assert!(matches!(
            store.delete(0).unwrap_err(),
            CoreError::Invalid(_)
        ));
    }

    #[test]
    fn oversized_document_is_rejected() {
        let mut store = store();
        let huge = "字".repeat(MAX_DOC_BYTES / 3 + 1);
        let doc = SearchDoc {
            id: 1,
            kind: "note",
            title: None,
            content: &huge,
        };
        let error = store.upsert(&doc).unwrap_err();
        assert!(error.to_string().contains("超限"), "{error}");
        assert_eq!(store.len().unwrap(), 0, "超限文档不能入库");
    }

    // ------------------------------------------------------------------
    // snippet
    // ------------------------------------------------------------------

    #[test]
    fn snippet_is_centered_and_marked() {
        let mut store = store();
        let prefix = "前".repeat(200);
        let suffix = "后".repeat(200);
        let content = format!("{prefix}这里是关键内容高等数学{suffix}");
        add(&mut store, 1, "note", "标题", &content);

        let hits = store.search("高等数学", 1).unwrap();
        let snippet = &hits[0].snippet;
        assert!(snippet.contains("高等数学"), "{snippet}");
        assert!(
            snippet.starts_with('…') && snippet.ends_with('…'),
            "{snippet}"
        );
        // ±60 字符 + 两个省略号 + 命中词本身
        let chars = snippet.chars().count();
        assert!(chars <= 2 * SNIPPET_RADIUS + 2 + 4, "snippet 过长：{chars}");
    }

    #[test]
    fn snippet_never_splits_utf8() {
        let mut store = store();
        // 前后都是 4 字节 emoji，边界最容易劈坏
        let content = format!("{}📚高等数学📚{}", "🌾".repeat(40), "🌾".repeat(40));
        add(&mut store, 1, "note", "标题", &content);
        let hits = store.search("高等数学", 1).unwrap();
        let snippet = &hits[0].snippet;
        // 能安全转成 str 就已经说明没劈坏（这里再断言内容）
        assert!(snippet.contains("高等数学"), "{snippet}");
        assert!(!snippet.contains('\u{FFFD}'), "出现替换符：{snippet}");
        assert!(snippet.chars().count() <= 2 * SNIPPET_RADIUS + 2 + 4);
    }

    #[test]
    fn snippet_of_short_content_has_no_ellipsis() {
        let mut store = store();
        add(&mut store, 1, "note", "笔记", "只有高等数学两个字");
        let hits = store.search("数学", 1).unwrap();
        assert!(!hits[0].snippet.starts_with('…'));
        assert!(!hits[0].snippet.ends_with('…'));
    }

    // ------------------------------------------------------------------
    // 排序 / 持久化 / 契约
    // ------------------------------------------------------------------

    #[test]
    fn ranking_prefers_documents_with_more_matches() {
        let mut store = store();
        add(&mut store, 1, "note", "a", "高等数学 高等数学 高等数学");
        add(&mut store, 2, "note", "b", "高等数学 顺便提一句");
        let hits = store.search("高等数学", 5).unwrap();
        assert_eq!(hits[0].id, 1, "{hits:?}");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn score_is_rounded_to_four_decimals() {
        let mut store = store();
        add(&mut store, 1, "note", "笔记", "高等数学笔记");
        let hits = store.search("高等数学", 1).unwrap();
        let score = hits[0].score;
        let rounded = (score * 10_000.0).round() / 10_000.0;
        assert_eq!(score, rounded, "分数必须是 4 位小数：{score}");
    }

    #[test]
    fn index_survives_reopen_without_rebuild() {
        let dir = std::env::temp_dir().join(format!("yunong-search-{}", uuid::Uuid::new_v4()));
        let path = dir.join("timetable.db");
        {
            let mut store = SearchStore::open(&path).unwrap();
            add(&mut store, 1, "note", "笔记", "高等数学笔记");
        }
        // 重新打开：不重建索引也能查（数据在表里）
        let store = SearchStore::open(&path).unwrap();
        assert_eq!(ids(&store.search("高等数学", 5).unwrap()), vec![1]);
        assert_eq!(store.len().unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_content_document_is_stored_but_unmatched() {
        let mut store = store();
        let doc = SearchDoc {
            id: 1,
            kind: "note",
            title: None,
            content: "",
        };
        assert_eq!(store.upsert(&doc).unwrap().terms, 0);
        assert_eq!(store.len().unwrap(), 1);
        assert!(store.search("任意", 5).unwrap().is_empty());
        assert_eq!(store.stats().unwrap().avg_len, 0.0);
    }
}
