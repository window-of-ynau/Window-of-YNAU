//! 派生数据回填（升级路径，schema 版本机制）。
//!
//! # 为什么需要
//!
//! "新增派生数据"最容易漏的一环是**老数据**：功能上线那天起新写入的会同步维护，
//! 但**上线前就在库里的数据**永远不会被补。真实事故：`yunong_memory_upsert` 开始自动写
//! 检索索引后，上个版本存的记忆既没索引也不会被重建 —— 用户看得到记忆、**却永远搜不到**，
//! 只能"删掉重加"。
//!
//! # 机制
//!
//! 用 SQLite 的 `PRAGMA user_version` 记"派生数据已经回填到第几版"：
//!
//! 1. 启动时（或首次搜索时）调用 [`run`]；
//! 2. 库里的 `user_version` 小于 [`STEPS`] 里某个步骤的版本，就执行那一步；
//! 3. **一次最多处理 `batch` 条**，跑不完就留着，下次接着跑（`remaining > 0`）；
//! 4. 该步骤**全部**补齐后才把 `user_version` 抬上去 —— 所以中途崩溃/杀进程不会丢进度，
//!    也不会重复干活（补齐的条目在下次查询里自然就"不缺"了）。
//!
//! # 新增派生数据时（重要）
//!
//! 1. 在 [`DERIVED_STORES`] 里登记这张表；
//! 2. 在 [`STEPS`] 里追加一个**新的、更大的版本号**，并在 `covers` 里写上表名；
//! 3. 在 [`StepJob`] 里实现回填逻辑。
//!
//! 单测 `every_derived_store_has_a_step` 会卡住第 1/2 步没做的情况：**漏登记就红**。

use rusqlite::Connection;

use crate::error::{CoreError, CoreResult};
use crate::local_search::SearchStore;
use crate::memory_items::MemoryItemStore;

/// 一次调用最多回填多少条（保证不阻塞启动；剩下的下次继续）。
pub const DEFAULT_BATCH: usize = 200;

/// 由主数据派生的表的登记表。
///
/// **新增派生数据必须在这里登记**，否则 [`tests::every_derived_store_has_a_step`] 会失败。
pub const DERIVED_STORES: &[&str] = &["search_docs", "search_postings"];

/// 主数据表（不是派生的，不需要回填）。
///
/// 与 [`DERIVED_STORES`] 合起来必须覆盖库里**所有**表——单测
/// `every_table_is_classified` 会扫源码里的 `CREATE TABLE` 卡住漏登记。
pub const PRIMARY_STORES: &[&str] = &["terms", "courses", "memory_items"];

/// 一个回填步骤：`version` 只会往上走。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// 版本号（严格递增，从 1 开始）
    pub version: i64,
    /// 名字（写进日志/报告，便于排障）
    pub name: &'static str,
    /// 这一步覆盖哪些派生表（必须覆盖 [`DERIVED_STORES`] 里的每一项）
    pub covers: &'static [&'static str],
}

/// 全部回填步骤（按版本升序）。
///
/// 新增派生数据 → 追加一步，**不要改老步骤的版本号**（老库的 `user_version` 已经是它了）。
pub const STEPS: &[Step] = &[Step {
    version: 1,
    name: "memory_search_index",
    covers: &["search_docs", "search_postings"],
}];

/// 一次回填的结果（也是 `yunong_maintenance_backfill` 的返回体）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Report {
    /// 开始前的 `user_version`
    pub from_version: i64,
    /// 结束后的 `user_version`（没补齐就是原值）
    pub to_version: i64,
    /// 最新版本号（全部做完时 `to_version == latest_version`）
    pub latest_version: i64,
    /// 本次实际补了多少条
    pub processed: usize,
    /// 还没补的条数（0 = 这一步做完了）
    pub remaining: usize,
    /// 本次执行了哪些步骤
    pub steps_run: Vec<i64>,
}

impl Report {
    /// 是不是已经完全补齐（调用方可以停止轮询）。
    pub fn done(&self) -> bool {
        self.to_version >= self.latest_version && self.remaining == 0
    }
}

/// 回填入口：幂等、可中断、有单次上限。
///
/// `db_path` 是 `crates/core` 用的那个 SQLite 文件（课表/记忆/索引同库）；
/// 空路径表示"没有库"（直接当完成，不报错 —— 首次启动还没建库是正常的）。
pub fn run(db_path: &str, batch: usize) -> CoreResult<Report> {
    let latest_version = STEPS.iter().map(|step| step.version).max().unwrap_or(0);
    if db_path.trim().is_empty() {
        return Ok(Report {
            from_version: latest_version,
            to_version: latest_version,
            latest_version,
            processed: 0,
            remaining: 0,
            steps_run: Vec::new(),
        });
    }
    // 打开（必要时创建）主数据表与派生表，保证 join/查询不会因缺表失败
    let mut memories = MemoryItemStore::open(db_path)?;
    let mut search = SearchStore::open(db_path)?;
    let mut meta = Connection::open(db_path)
        .map_err(|err| CoreError::Storage(format!("打开 SQLite 失败：{err}")))?;

    let from_version = user_version(&meta)?;
    let mut processed = 0_usize;
    let mut remaining = 0_usize;
    let mut steps_run = Vec::new();
    let mut version = from_version;

    for step in STEPS.iter().filter(|step| step.version > from_version) {
        let outcome = run_step(&mut meta, &mut memories, &mut search, *step, batch)?;
        processed += outcome.processed;
        steps_run.push(step.version);
        if outcome.remaining > 0 {
            // 这一批没做完：不抬版本号，下次接着做
            remaining = outcome.remaining;
            break;
        }
        version = step.version;
        set_user_version(&mut meta, step.version)?;
    }

    Ok(Report {
        from_version,
        to_version: version,
        latest_version,
        processed,
        remaining,
        steps_run,
    })
}

/// 某一步还差多少条没补（看版本号就能判断要不要干活，这里给"进度"用）。
pub fn pending(db_path: &str) -> CoreResult<Report> {
    run(db_path, 0)
}

struct StepOutcome {
    processed: usize,
    remaining: usize,
}

/// 执行一步（目前只有第 1 步：把缺索引的记忆补进 `search_docs`）。
fn run_step(
    meta: &mut Connection,
    memories: &mut MemoryItemStore,
    search: &mut SearchStore,
    step: Step,
    batch: usize,
) -> CoreResult<StepOutcome> {
    match step.version {
        1 => backfill_search_index(meta, memories, search, batch),
        other => Err(CoreError::NotImplemented(format!(
            "回填步骤 {other}（{}）还没有实现",
            step.name
        ))),
    }
}

/// 第 1 步：`memory_items` 里有、`search_docs` 里没有的，逐条补索引。
fn backfill_search_index(
    meta: &Connection,
    memories: &mut MemoryItemStore,
    search: &mut SearchStore,
    batch: usize,
) -> CoreResult<StepOutcome> {
    // 索引有容量上限（`SearchStore::limits().docs`，默认 5000）：
    // 超过上限的老库只补**最新**的那些，否则每次 upsert 都会淘汰一条、永远收敛不了。
    let capacity = search.limits().docs.max(1);
    let missing = missing_memory_index_ids(meta, batch, capacity)?;
    let mut processed = 0_usize;
    for id in &missing {
        let Some(item) = memories.get(*id)? else {
            // 查询与读取之间被删了：跳过（下轮 join 自然没有它）
            continue;
        };
        search.index_memory_item(&item)?;
        processed += 1;
    }
    let remaining = count_missing_memory_index(meta, capacity)?;
    Ok(StepOutcome {
        processed,
        remaining,
    })
}

/// 缺索引的记忆 id（最多 `limit` 条，只取**最新**的 `capacity` 条里的）。
fn missing_memory_index_ids(
    conn: &Connection,
    limit: usize,
    capacity: usize,
) -> CoreResult<Vec<i64>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut statement = conn
        .prepare(
            "SELECT m.id FROM memory_items m \
             LEFT JOIN search_docs d ON d.id = m.id \
             WHERE d.id IS NULL \
             ORDER BY m.updated_at DESC, m.id DESC LIMIT ?1",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([limit.min(capacity) as i64], |row| row.get::<_, i64>(0))
        .map_err(storage)?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.map_err(storage)?);
    }
    Ok(ids)
}

/// 还差多少条（只看最新 `capacity` 条；0 = 补齐了）。
///
/// 超过索引容量的老条目**故意不算"欠账"**：它们本来就会被容量策略挤掉，
/// 否则回填永远收敛不了（补一条、挤一条）。
fn count_missing_memory_index(conn: &Connection, capacity: usize) -> CoreResult<usize> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ( \
               SELECT m.id FROM memory_items m \
               LEFT JOIN search_docs d ON d.id = m.id \
               WHERE d.id IS NULL \
               ORDER BY m.updated_at DESC, m.id DESC LIMIT ?1)",
            [capacity.max(1) as i64],
            |row| row.get(0),
        )
        .map_err(storage)?;
    Ok(count.max(0) as usize)
}

fn user_version(conn: &Connection) -> CoreResult<i64> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(storage)
}

fn set_user_version(conn: &mut Connection, version: i64) -> CoreResult<()> {
    // `PRAGMA` 不支持参数绑定；version 只来自本模块的常量，不含外部输入
    conn.execute_batch(&format!("PRAGMA user_version = {version};"))
        .map_err(storage)
}

fn storage(err: rusqlite::Error) -> CoreError {
    CoreError::Storage(format!("回填派生数据失败：{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_items::{MemoryDraft, MemoryKind};

    fn temp_db() -> String {
        let dir = std::env::temp_dir().join(format!("yunong-backfill-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("app.db").to_string_lossy().into_owned()
    }

    /// 造"上个版本的库"：只有 memory_items，没有 search_docs 条目。
    fn legacy_db(count: usize) -> (String, Vec<i64>) {
        let db_path = temp_db();
        let mut store = MemoryItemStore::open(&db_path).unwrap();
        let mut ids = Vec::new();
        for index in 0..count {
            let title = format!("老记忆 {index}");
            let content = format!("第 {index} 条老记忆：周三带雨具，宿舍楼 3 栋 502");
            let write = store
                .upsert(
                    &MemoryDraft {
                        kind: MemoryKind::Note,
                        title: Some(&title),
                        content: &content,
                        source: Some("user"),
                        expires_at: None,
                    },
                    None,
                )
                .unwrap();
            ids.push(write.id);
        }
        (db_path, ids)
    }

    fn search_count(db_path: &str) -> usize {
        let store = SearchStore::open(db_path).unwrap();
        store.len().unwrap()
    }

    #[test]
    fn every_derived_store_has_a_step() {
        // 回归护栏：新增派生数据（如图片/向量）却没登记回填步骤 → 这里直接红
        for store in DERIVED_STORES {
            let covered = STEPS.iter().any(|step| step.covers.contains(store));
            assert!(
                covered,
                "派生表 `{store}` 没有回填步骤：请在 STEPS 里追加一步并在 StepJob 里实现"
            );
        }
        // 版本号严格递增且从 1 开始
        let mut last = 0;
        for step in STEPS {
            assert!(step.version > last, "回填版本号必须严格递增：{step:?}");
            last = step.version;
        }
        assert_eq!(STEPS.first().map(|step| step.version), Some(1));
    }

    /// 回归护栏 2：源码里出现的每张表，都必须是"主数据"或"派生数据"之一。
    ///
    /// 将来加一张新表（比如 `memory_blobs` / 向量表）如果忘了分类，
    /// 就会在这里失败——逼你想清楚"老数据要不要回填"。
    #[test]
    fn every_table_is_classified() {
        const SOURCES: &[(&str, &str)] = &[
            ("local_search.rs", include_str!("local_search.rs")),
            ("memory_items.rs", include_str!("memory_items.rs")),
            ("store.rs", include_str!("store.rs")),
        ];
        let marker = "CREATE TABLE IF NOT EXISTS ";
        let mut tables: Vec<String> = Vec::new();
        for (_file, source) in SOURCES {
            for piece in source.split(marker).skip(1) {
                // 只认 ASCII 标识符：注释里出现的 "CREATE TABLE IF NOT EXISTS …" 会解析成空
                let name: String = piece
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if name.is_empty() {
                    continue;
                }
                tables.push(name);
            }
        }
        assert!(tables.len() >= 4, "只扫到 {:?}，解析逻辑坏了", tables);
        for table in &tables {
            let classified = PRIMARY_STORES.contains(&table.as_str())
                || DERIVED_STORES.contains(&table.as_str());
            assert!(
                classified,
                "表 `{table}` 没有分类：主数据请加进 PRIMARY_STORES，\
                 派生数据请加进 DERIVED_STORES 并在 STEPS 里补一步回填"
            );
        }
    }

    #[test]
    fn legacy_memories_become_searchable_after_backfill() {
        let (db_path, ids) = legacy_db(5);
        assert_eq!(search_count(&db_path), 0, "老库本来没有索引条目");

        let report = run(&db_path, DEFAULT_BATCH).unwrap();
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, 1);
        assert_eq!(report.processed, 5);
        assert_eq!(report.remaining, 0);
        assert!(report.done(), "{report:?}");
        assert_eq!(report.steps_run, vec![1]);
        assert_eq!(search_count(&db_path), 5);

        // 真的能搜到
        let search = SearchStore::open(&db_path).unwrap();
        let hits = search.search("雨具", 10).unwrap();
        assert_eq!(hits.len(), 5, "{hits:?}");
        assert!(ids.contains(&hits[0].id));
    }

    /// 真实尺度：1000 条老记忆，按默认批量 200 分 5 轮补齐（不阻塞启动）。
    #[test]
    fn backfill_handles_a_thousand_legacy_rows() {
        let (db_path, _) = legacy_db(1_000);
        assert_eq!(search_count(&db_path), 0);

        let started = std::time::Instant::now();
        let mut rounds = 0;
        let mut total = 0;
        loop {
            let report = run(&db_path, DEFAULT_BATCH).unwrap();
            rounds += 1;
            total += report.processed;
            if report.done() {
                break;
            }
            assert!(rounds < 20, "回填不收敛：{report:?}");
        }
        let elapsed = started.elapsed();
        println!(
            "1000 条老记忆：{rounds} 轮、共补 {total} 条、用时 {:?}；索引文档数 {}",
            elapsed,
            search_count(&db_path)
        );
        assert_eq!(total, 1_000);
        assert_eq!(search_count(&db_path), 1_000);
        assert_eq!(rounds, 5, "默认批量 200 → 5 轮");
    }

    #[test]
    fn backfill_is_idempotent() {
        let (db_path, _) = legacy_db(3);
        let first = run(&db_path, DEFAULT_BATCH).unwrap();
        assert_eq!(first.processed, 3);
        let second = run(&db_path, DEFAULT_BATCH).unwrap();
        assert_eq!(second.from_version, 1);
        assert_eq!(second.processed, 0, "第二次不该再补：{second:?}");
        assert!(second.steps_run.is_empty());
        assert_eq!(search_count(&db_path), 3, "幂等：文档数不变");
    }

    #[test]
    fn backfill_is_bounded_and_resumable() {
        let (db_path, _) = legacy_db(7);
        // 一次只补 3 条：版本号不能抬，remaining 要说实话
        let first = run(&db_path, 3).unwrap();
        assert_eq!(first.processed, 3);
        assert_eq!(first.remaining, 4);
        assert_eq!(first.to_version, 0, "没做完就不许抬版本号：{first:?}");
        assert!(!first.done());
        assert_eq!(search_count(&db_path), 3);

        // 再跑：接着补，直到补齐
        let second = run(&db_path, 3).unwrap();
        assert_eq!(second.processed, 3);
        assert_eq!(second.remaining, 1);
        assert_eq!(second.to_version, 0);
        let third = run(&db_path, 3).unwrap();
        assert_eq!(third.processed, 1);
        assert_eq!(third.remaining, 0);
        assert_eq!(third.to_version, 1);
        assert!(third.done());
        assert_eq!(search_count(&db_path), 7);
    }

    #[test]
    fn zero_batch_reports_progress_without_working() {
        let (db_path, _) = legacy_db(4);
        let report = run(&db_path, 0).unwrap();
        assert_eq!(report.processed, 0);
        assert_eq!(report.remaining, 4, "0 条上限时只报进度：{report:?}");
        assert_eq!(report.to_version, 0);
        assert!(!report.done());
        assert_eq!(search_count(&db_path), 0);
    }

    #[test]
    fn empty_path_is_a_no_op() {
        let report = run("", DEFAULT_BATCH).unwrap();
        assert!(report.done(), "{report:?}");
        assert_eq!(report.processed, 0);
        assert_eq!(report.to_version, report.latest_version);
    }

    #[test]
    fn new_memories_are_not_double_indexed() {
        // 新写入的记忆本来就有索引：回填只补缺的，不会重复插
        let db_path = temp_db();
        let mut store = MemoryItemStore::open(&db_path).unwrap();
        let mut search = SearchStore::open(&db_path).unwrap();
        let write = store
            .upsert(
                &MemoryDraft {
                    kind: MemoryKind::Note,
                    title: Some("新记忆"),
                    content: "新写入的记忆自带索引",
                    source: Some("user"),
                    expires_at: None,
                },
                None,
            )
            .unwrap();
        search
            .index_memory_item(&store.get(write.id).unwrap().unwrap())
            .unwrap();
        drop(search);

        let report = run(&db_path, DEFAULT_BATCH).unwrap();
        assert_eq!(report.processed, 0, "已经有索引的不该再补：{report:?}");
        assert!(report.done());
        assert_eq!(search_count(&db_path), 1);
    }
}
