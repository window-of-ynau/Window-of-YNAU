//! RAG 增量管线的集成测试（ADR-19 决策 3）。
//!
//! 全部需要 PostgreSQL（`--include-ignored`），因为要验真迁移、真 SQL、真状态机。
//! 抓取器一律用 mock：**CI 与本地都不允许真的联网抓取**。

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::TestDb;
use sqlx::PgPool;

use yunong_server::config::Config;
use yunong_server::rag::RagService;
use yunong_server::rag_fetch::{
    self, canonical_identifier, current_month, hash_identifier, index_known_docs, prune_salts,
    FetchConfig, FetchError, FetchFuture, FetchPipeline, FetchReport, FetchTarget, FetchedDoc,
    Fetcher, StoreOutcome,
};

// ===================== mock 抓取器 =====================

/// 构造一个"纯文本"抓取结果（t81 / M4：抓取器返回 Content-Type + 字节）。
fn doc(content_type: &str, body: &str) -> FetchedDoc {
    FetchedDoc {
        content_type: content_type.to_owned(),
        body: body.as_bytes().to_vec(),
    }
}

/// 按脚本依次返回结果；脚本用完后一直返回成功。
struct MockFetcher {
    calls: Mutex<Vec<FetchTarget>>,
    script: Mutex<VecDeque<Result<FetchedDoc, FetchError>>>,
}

impl MockFetcher {
    fn new(script: Vec<Result<FetchedDoc, FetchError>>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            script: Mutex::new(script.into()),
        })
    }

    fn calls(&self) -> Vec<FetchTarget> {
        self.calls.lock().expect("锁").clone()
    }

    fn call_count(&self) -> usize {
        self.calls.lock().expect("锁").len()
    }
}

impl Fetcher for MockFetcher {
    fn fetch<'a>(&'a self, target: &'a FetchTarget) -> FetchFuture<'a> {
        self.calls.lock().expect("锁").push(target.clone());
        let outcome = self
            .script
            .lock()
            .expect("锁")
            .pop_front()
            .unwrap_or_else(|| Ok(doc("text/plain", "mock-body")));
        Box::pin(async move { outcome })
    }
}

// ===================== 脚手架 =====================

struct Fixture {
    db: TestDb,
    month: String,
}

impl Fixture {
    async fn setup() -> Option<Self> {
        let db = TestDb::setup().await?;
        Some(Self {
            db,
            month: current_month(),
        })
    }

    fn pool(&self) -> PgPool {
        self.db.pool.clone()
    }

    async fn salt(&self) -> String {
        let rag = RagService::new(&Config::for_tests(), Some(self.pool()));
        rag.salt_for_month(&self.month).await.expect("取盐")
    }

    /// 索引一条已知语料，并返回它在上报侧的哈希。
    async fn index_doc(&self, identifier: &str, source: &str) -> String {
        index_known_docs(
            &self.pool(),
            &self.month,
            &[(identifier.to_owned(), source.to_owned())],
            0,
        )
        .await
        .expect("索引已知语料");
        let canonical = canonical_identifier(identifier).expect("规范化");
        hash_identifier(&self.salt().await, &canonical)
    }

    /// 模拟"客户端上报了一条哈希"（直接写 candidates，等价于 referrals 的聚合结果）。
    async fn insert_candidate(&self, hash: &str, count: i64) {
        sqlx::query(
            "INSERT INTO rag_candidates (month, hash, count, status) VALUES ($1, $2, $3, 'pending')",
        )
        .bind(&self.month)
        .bind(hash)
        .bind(count)
        .execute(&self.pool())
        .await
        .expect("插入候选");
    }

    async fn pipeline(&self, fetcher: Arc<MockFetcher>, retries: u32) -> FetchPipeline {
        let rag = Arc::new(RagService::new(&Config::for_tests(), Some(self.pool())));
        FetchPipeline::new(
            self.pool(),
            rag,
            fetcher,
            FetchConfig {
                top_n: 50,
                retries,
                backoff: Duration::ZERO, // 测试不等退避
                retry_failed: false,
                min_interval: Duration::ZERO,            // 测试不限速
                stale_after: Duration::from_secs(1_800), // M1：默认 30 分钟
                max_body_bytes: 2 * 1024 * 1024,         // M4
                doi_allowed_hosts: Vec::new(),           // 单测里不联网，白名单无所谓
            },
        )
    }
}

async fn candidate_status(pool: &PgPool, month: &str, hash: &str) -> Option<String> {
    common::text_column(
        pool,
        &format!("SELECT status FROM rag_candidates WHERE month = '{month}' AND hash = '{hash}'"),
    )
    .await
}

// ===================== 用例 =====================

/// join 命中 → 抓到正文 → `fetched` + 正文落进 `rag_known_docs`。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn join_hit_fetches_and_stores_text() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("arXiv:2401.01234v3", "arxiv").await;
    fx.insert_candidate(&hash, 7).await;

    let fetcher = MockFetcher::new(vec![Ok(doc("text/plain", "论文摘要正文"))]);
    let report: FetchReport = fx
        .pipeline(fetcher.clone(), 3)
        .await
        .run_once()
        .await
        .expect("跑一轮");

    assert_eq!(report.fetched, 1, "{report:?}");
    assert_eq!(report.skipped, 0);
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &hash)
            .await
            .as_deref(),
        Some("fetched")
    );

    // 抓取目标用的是**规范化后**的标识（arXiv:xxxv3 → xxx）
    let calls = fetcher.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].identifier, "2401.01234");
    assert_eq!(calls[0].source, "arxiv");

    assert_eq!(
        common::text_column(
            &fx.pool(),
            &format!(
                "SELECT doc_text FROM rag_known_docs WHERE month = '{}' AND identifier = '2401.01234'",
                fx.month
            )
        )
        .await
        .as_deref(),
        Some("论文摘要正文")
    );
    assert_eq!(
        common::text_column(
            &fx.pool(),
            &format!("SELECT doc_id FROM rag_candidates WHERE hash = '{hash}'")
        )
        .await
        .as_deref(),
        Some("2401.01234"),
    );
    fx.db.finish().await;
}

/// join 未命中 → `skipped`（诚实状态），且**不会**去调用抓取器。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn join_miss_marks_skipped_without_fetching() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let unknown = hash_identifier(&fx.salt().await, "某条从未被索引过的标识");
    fx.insert_candidate(&unknown, 3).await;

    let fetcher = MockFetcher::new(vec![]);
    let report = fx
        .pipeline(fetcher.clone(), 3)
        .await
        .run_once()
        .await
        .expect("跑一轮");

    assert_eq!(report.skipped, 1, "{report:?}");
    assert_eq!(report.fetched, 0);
    assert_eq!(fetcher.call_count(), 0, "反推不出来就不该抓");
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &unknown)
            .await
            .as_deref(),
        Some("skipped")
    );
    assert_eq!(
        common::text_column(
            &fx.pool(),
            &format!("SELECT last_error FROM rag_candidates WHERE hash = '{unknown}'")
        )
        .await
        .as_deref(),
        Some("no_known_doc")
    );
    fx.db.finish().await;
}

/// 失败 → 按 3 次重试 → 最终 `failed`，`attempts = 4`，`last_error` 有原因。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn retries_then_marks_failed() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("10.1234/fail-me", "doi").await;
    fx.insert_candidate(&hash, 5).await;

    let fetcher = MockFetcher::new(vec![
        Err(FetchError::retryable("boom-1")),
        Err(FetchError::retryable("boom-2")),
        Err(FetchError::retryable("boom-3")),
        Err(FetchError::retryable("boom-4")),
    ]);
    let report = fx
        .pipeline(fetcher.clone(), 3)
        .await
        .run_once()
        .await
        .expect("跑一轮");

    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(fetcher.call_count(), 4, "1 次 + 3 次重试");
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &hash)
            .await
            .as_deref(),
        Some("failed")
    );
    assert_eq!(
        common::count(
            &fx.pool(),
            &format!("SELECT attempts::bigint FROM rag_candidates WHERE hash = '{hash}'")
        )
        .await,
        4
    );
    assert_eq!(
        common::text_column(
            &fx.pool(),
            &format!("SELECT last_error FROM rag_candidates WHERE hash = '{hash}'")
        )
        .await
        .as_deref(),
        Some("boom-4")
    );
    fx.db.finish().await;
}

/// 永久失败（404 等）不重试。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn permanent_error_is_not_retried() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("10.1234/gone", "doi").await;
    fx.insert_candidate(&hash, 1).await;

    let fetcher = MockFetcher::new(vec![Err(FetchError::permanent("404"))]);
    let report = fx
        .pipeline(fetcher.clone(), 3)
        .await
        .run_once()
        .await
        .expect("跑一轮");

    assert_eq!(report.failed, 1);
    assert_eq!(fetcher.call_count(), 1, "永久失败不该重试");
    assert_eq!(
        common::count(
            &fx.pool(),
            &format!("SELECT attempts::bigint FROM rag_candidates WHERE hash = '{hash}'")
        )
        .await,
        1
    );
    fx.db.finish().await;
}

/// 重试一次后成功：`attempts = 2`，状态 `fetched`。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn retry_then_success_marks_fetched() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("10.1234/flaky", "doi").await;
    fx.insert_candidate(&hash, 2).await;

    let fetcher = MockFetcher::new(vec![
        Err(FetchError::retryable("timeout")),
        Ok(doc("text/plain", "第二次成功")),
    ]);
    let report = fx
        .pipeline(fetcher.clone(), 3)
        .await
        .run_once()
        .await
        .expect("跑一轮");

    assert_eq!(report.fetched, 1);
    assert_eq!(fetcher.call_count(), 2);
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &hash)
            .await
            .as_deref(),
        Some("fetched")
    );
    assert_eq!(
        common::count(
            &fx.pool(),
            &format!("SELECT attempts::bigint FROM rag_candidates WHERE hash = '{hash}'")
        )
        .await,
        2
    );
    fx.db.finish().await;
}

/// 幂等：连跑两轮，第二轮不再抓（终态不重入），抓取器调用数不变。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn second_run_does_not_refetch_done_items() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("2401.05678", "arxiv").await;
    fx.insert_candidate(&hash, 9).await;

    let fetcher = MockFetcher::new(vec![Ok(doc("text/plain", "正文"))]);
    let pipeline = fx.pipeline(fetcher.clone(), 3).await;

    let first = pipeline.run_once().await.expect("第一轮");
    assert_eq!(first.fetched, 1);

    let second = pipeline.run_once().await.expect("第二轮");
    assert_eq!(
        second.processed, 0,
        "已经 fetched 的候选不该再被处理：{second:?}"
    );
    assert_eq!(second.fetched, 0);
    assert_eq!(fetcher.call_count(), 1, "第二轮不该再调用抓取器");
    assert_eq!(
        common::count(
            &fx.pool(),
            &format!(
                "SELECT count(*) FROM rag_known_docs WHERE month = '{}' AND fetched_at IS NOT NULL",
                fx.month
            )
        )
        .await,
        1,
        "只应有这一条被标记抓取过"
    );
    fx.db.finish().await;
}

/// 隐私红线（captain 在 t58 的裁决）：`rag_known_docs` **不许有 hash 列**，
/// 四张 rag 表都不许有 user_id。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn rag_tables_have_no_user_id_and_known_docs_has_no_hash() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name IN ('rag_salts','rag_referrals','rag_candidates','rag_known_docs') \
               AND column_name = 'user_id'"
        )
        .await,
        0,
        "RAG 表不允许出现 user_id"
    );
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = 'rag_known_docs' AND column_name = 'hash'"
        )
        .await,
        0,
        "rag_known_docs 不许存 hash：那等于在库里放一本去匿名化字典"
    );
    db.finish().await;
}

/// 索引会规范化标识（同一篇文档的不同写法只落一行）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn indexing_normalizes_identifiers() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let month = current_month();
    let docs = vec![
        (
            "https://arxiv.org/abs/2401.01234".to_owned(),
            "arxiv".to_owned(),
        ),
        ("arXiv:2401.01234v2".to_owned(), "arxiv".to_owned()),
    ];
    let written = index_known_docs(&db.pool, &month, &docs, 0)
        .await
        .expect("索引");
    assert_eq!(written, 2, "两行都写入（规范化后是同一个 identifier）");
    assert_eq!(
        common::count(
            &db.pool,
            &format!(
                "SELECT count(*) FROM rag_known_docs WHERE month = '{month}' AND identifier = '2401.01234'"
            )
        )
        .await,
        1,
        "规范化后应只有一行"
    );
    db.finish().await;
}

/// 保留期：旧月份的语料与旧盐都会被清掉（两个保留期独立配置）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn retention_prunes_old_months_and_salts() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let month = current_month();
    let rag = RagService::new(&Config::for_tests(), Some(db.pool.clone()));
    // 造两个旧月份：一个在保留期内、一个超期
    let recent = rag_fetch::previous_months(&month, 1);
    let ancient = rag_fetch::previous_months(&month, 6);
    for old in [&recent, &ancient] {
        rag.salt_for_month(old).await.expect("旧盐");
        index_known_docs(&db.pool, old, &[("10.1/x".to_owned(), "doi".to_owned())], 0)
            .await
            .expect("旧语料");
    }

    // 语料保留 3 个月：ancient 被清、recent 留
    index_known_docs(&db.pool, &month, &[], 3)
        .await
        .expect("清理语料");
    assert_eq!(
        common::count(
            &db.pool,
            &format!("SELECT count(*) FROM rag_known_docs WHERE month = '{ancient}'")
        )
        .await,
        0,
        "超期语料应被清理"
    );
    assert_eq!(
        common::count(
            &db.pool,
            &format!("SELECT count(*) FROM rag_known_docs WHERE month = '{recent}'")
        )
        .await,
        1,
        "保留期内的语料不该被清理"
    );

    // 盐保留 3 个月：同样是 ancient 被清
    let removed = prune_salts(&db.pool, &month, 3).await.expect("清理盐");
    assert!(removed >= 1, "应清掉至少一条过期盐");
    assert_eq!(
        common::count(
            &db.pool,
            &format!("SELECT count(*) FROM rag_salts WHERE month = '{ancient}'")
        )
        .await,
        0
    );
    assert_eq!(
        common::count(
            &db.pool,
            &format!("SELECT count(*) FROM rag_salts WHERE month = '{recent}'")
        )
        .await,
        1
    );
    db.finish().await;
}

/// 迁移可重入：0006 连跑两次不应报错（老库升级 / 重入场景）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_migration_0006_is_idempotent() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migrations/0006_rag_known_docs.sql"
    ))
    .expect("读迁移文件");
    // 多语句必须用 raw_sql（预编译协议一次只能一条）
    for attempt in 0..2 {
        sqlx::raw_sql(&sql)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("第 {} 次重入 0006 失败：{error}", attempt + 1));
    }
    // 重入后结构仍正确：known_docs 仍然没有 hash 列、candidates 的 CHECK 仍含 fetching
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = 'rag_known_docs' AND column_name = 'hash'"
        )
        .await,
        0
    );
    let invalid = sqlx::query(
        "INSERT INTO rag_candidates (month, hash, count, status) \
         VALUES ('2026-11', 'deadbeef', 1, 'bogus')",
    )
    .execute(&db.pool)
    .await;
    assert!(invalid.is_err(), "非法 status 应被 CHECK 拒绝");
    db.finish().await;
}

/// 崩溃恢复：残留的 `fetching` 本轮开头会被放回 `pending` 并重新处理。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn stale_fetching_is_recovered() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("2401.10101", "arxiv").await;
    sqlx::query(
        "INSERT INTO rag_candidates (month, hash, count, status) VALUES ($1, $2, 1, 'fetching')",
    )
    .bind(&fx.month)
    .bind(&hash)
    .execute(&fx.pool())
    .await
    .expect("插入残留 fetching");

    let fetcher = MockFetcher::new(vec![Ok(doc("text/plain", "恢复后的正文"))]);
    let report = fx
        .pipeline(fetcher.clone(), 3)
        .await
        .run_once()
        .await
        .expect("跑一轮");

    assert_eq!(
        report.fetched, 1,
        "残留 fetching 应被恢复并抓取：{report:?}"
    );
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &hash)
            .await
            .as_deref(),
        Some("fetched")
    );
    fx.db.finish().await;
}

// ===================== t81 回归（M1 / M2 / M5 / M8） =====================

/// t81 / M2：库里有 `fetching` 行时重跑 0005/0006 **不得丢 CHECK 约束**。
///
/// 旧的"先 DROP 再 ADD"在非单事务 `psql -f` 下会先丢掉约束、再因 fetching 行报错退出：
/// 之后能插任意 `weird_status`。这个用例在旧实现下必然红（约束丢失 → 断言失败）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_rag_migrations_are_safe_to_rerun_with_fetching_rows() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    // 造一条 fetching 行（0006 之后合法）
    sqlx::query(
        "INSERT INTO rag_candidates (month, hash, count, status, claimed_at) \
         VALUES ($1, 'fetching-row', 1, 'fetching', now())",
    )
    .bind(&fx.month)
    .execute(&fx.pool())
    .await
    .expect("插入 fetching 行");

    // 重跑 0005 与 0006（迁移文件本身，原样执行）
    for name in ["0005_rag_referrals.sql", "0006_rag_known_docs.sql"] {
        let sql = common::migration_sql(name);
        sqlx::raw_sql(&sql)
            .execute(&fx.pool())
            .await
            .unwrap_or_else(|error| panic!("{name} 必须能在有 fetching 行时重跑：{error}"));
    }

    // 约束必须仍在
    assert_eq!(
        common::count(
            &fx.pool(),
            "SELECT count(*) FROM pg_constraint \
             WHERE conname = 'rag_candidates_status_check' \
               AND conrelid = 'rag_candidates'::regclass"
        )
        .await,
        1,
        "重跑后 CHECK 约束丢了（旧实现会这样）"
    );
    // 非法状态必须被拒
    let error = sqlx::query(
        "INSERT INTO rag_candidates (month, hash, count, status) \
         VALUES ($1, 'weird', 1, 'weird_status')",
    )
    .bind(&fx.month)
    .execute(&fx.pool())
    .await
    .expect_err("weird_status 必须被 CHECK 拒绝");
    assert!(
        matches!(error, sqlx::Error::Database(_)),
        "应是数据库约束错误：{error:?}"
    );
    // fetching 行本身还在（重跑没能把它"洗掉"）
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, "fetching-row")
            .await
            .as_deref(),
        Some("fetching")
    );
    // claimed_at 列存在（M1）
    assert_eq!(
        common::count(
            &fx.pool(),
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = 'rag_candidates' AND column_name = 'claimed_at'"
        )
        .await,
        1
    );
    fx.db.finish().await;
}

/// t81 / M1：只回收**超时**的 fetching，正在跑的 worker 的占位不能被抢。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_stale_fetching_is_recovered_but_fresh_is_not() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let stale_hash = fx.index_doc("https://example.com/stale", "web").await;
    let fresh_hash = fx.index_doc("https://example.com/fresh", "web").await;

    // 两条都处于 fetching：一条 2 小时前抢占（视为崩溃残留），一条刚刚抢占（另一个 worker 正在跑）
    for (hash, minutes_ago) in [(&stale_hash, 120i32), (&fresh_hash, 0i32)] {
        sqlx::query(
            "INSERT INTO rag_candidates (month, hash, count, status, claimed_at) \
             VALUES ($1, $2, 5, 'fetching', now() - make_interval(mins => $3))",
        )
        .bind(&fx.month)
        .bind(hash)
        .bind(minutes_ago)
        .execute(&fx.pool())
        .await
        .expect("插入 fetching");
    }

    let fetcher = MockFetcher::new(vec![Ok(doc("text/plain", "恢复后的正文"))]);
    let report = fx
        .pipeline(fetcher.clone(), 0)
        .await
        .run_once()
        .await
        .expect("跑一轮");

    assert_eq!(report.processed, 1, "只该回收那条超时的：{report:?}");
    assert_eq!(report.fetched, 1, "{report:?}");
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &stale_hash)
            .await
            .as_deref(),
        Some("fetched"),
        "超时的应被回收并抓完"
    );
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &fresh_hash)
            .await
            .as_deref(),
        Some("fetching"),
        "另一 worker 正在跑的占位被抢了（M1 回归）"
    );
    fx.db.finish().await;
}

/// t81 / M5：`rag_known_docs` 行不存在时必须**回滚**，不能写悬空 doc_id。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_store_rolls_back_when_known_doc_missing() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("https://example.com/gone", "web").await;
    fx.insert_candidate(&hash, 1).await;
    let updated = sqlx::query(
        "UPDATE rag_candidates SET status = 'fetching', claimed_at = now() \
         WHERE month = $1 AND hash = $2",
    )
    .bind(&fx.month)
    .bind(&hash)
    .execute(&fx.pool())
    .await
    .expect("置为 fetching")
    .rows_affected();
    assert_eq!(updated, 1, "候选行应存在并置为 fetching");
    // 模拟"join 之后语料行被删"
    sqlx::query("DELETE FROM rag_known_docs WHERE month = $1 AND identifier = $2")
        .bind(&fx.month)
        .bind("https://example.com/gone")
        .execute(&fx.pool())
        .await
        .expect("删语料行");

    let pipeline = fx.pipeline(MockFetcher::new(vec![]), 0).await;
    let outcome = pipeline
        .store_fetch_result(
            &fx.month,
            &hash,
            "https://example.com/gone",
            "不该落库的正文",
            1,
        )
        .await
        .expect("落库调用本身不该报错");

    assert_eq!(outcome, StoreOutcome::MissingKnownDoc);
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &hash)
            .await
            .as_deref(),
        Some("fetching"),
        "回滚后状态应保持 fetching（不是 fetched）"
    );
    // doc_id 必须是 NULL（`text_column` 只能取非空列，这里用 count 表达"为空"）
    assert_eq!(
        common::count(
            &fx.pool(),
            &format!(
                "SELECT count(*) FROM rag_candidates \
                 WHERE month = '{}' AND hash = '{}' AND doc_id IS NULL",
                fx.month, hash
            )
        )
        .await,
        1,
        "不能写悬空 doc_id"
    );
    fx.db.finish().await;
}

/// t81 / M5：候选行已经不是 fetching 时也要回滚（返回 Contended）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_store_reports_contended() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("https://example.com/race", "web").await;
    fx.insert_candidate(&hash, 1).await; // 状态是 pending

    let pipeline = fx.pipeline(MockFetcher::new(vec![]), 0).await;
    let outcome = pipeline
        .store_fetch_result(&fx.month, &hash, "https://example.com/race", "正文", 1)
        .await
        .expect("落库调用");
    assert_eq!(outcome, StoreOutcome::Contended);
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &hash)
            .await
            .as_deref(),
        Some("pending")
    );
    fx.db.finish().await;
}

/// t81 / M8：`retry_failed = true` 时本轮必须纳入 failed 项（运维重试手段）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_retry_failed_reprocesses_failed_candidates() {
    let Some(fx) = Fixture::setup().await else {
        return;
    };
    let hash = fx.index_doc("https://example.com/retry", "web").await;
    sqlx::query(
        "INSERT INTO rag_candidates (month, hash, count, status, attempts) \
         VALUES ($1, $2, 3, 'failed', 3)",
    )
    .bind(&fx.month)
    .bind(&hash)
    .execute(&fx.pool())
    .await
    .expect("插入 failed");

    // 默认（retry_failed=false）：不碰 failed
    let pipeline = fx.pipeline(MockFetcher::new(vec![]), 0).await;
    let report = pipeline.run_once().await.expect("跑一轮");
    assert_eq!(report.processed, 0, "默认不该重试 failed：{report:?}");

    // 打开开关：同一行应被重新抓
    let rag = Arc::new(RagService::new(&Config::for_tests(), Some(fx.pool())));
    let fetcher = MockFetcher::new(vec![Ok(doc("text/plain", "重试成功"))]);
    let mut config = FetchConfig {
        top_n: 50,
        retries: 0,
        backoff: Duration::ZERO,
        retry_failed: true,
        min_interval: Duration::ZERO,
        stale_after: Duration::from_secs(1_800),
        max_body_bytes: 2 * 1024 * 1024,
        doi_allowed_hosts: Vec::new(),
    };
    config.retry_failed = true;
    let pipeline = FetchPipeline::new(fx.pool(), rag, fetcher.clone(), config);
    let report = pipeline.run_once().await.expect("跑一轮");
    assert_eq!(report.processed, 1, "开关打开后应重试：{report:?}");
    assert_eq!(report.fetched, 1, "{report:?}");
    assert_eq!(
        candidate_status(&fx.pool(), &fx.month, &hash)
            .await
            .as_deref(),
        Some("fetched")
    );
    fx.db.finish().await;
}
