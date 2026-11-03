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
    FetchConfig, FetchError, FetchFuture, FetchPipeline, FetchReport, FetchTarget, Fetcher,
};

// ===================== mock 抓取器 =====================

/// 按脚本依次返回结果；脚本用完后一直返回成功。
struct MockFetcher {
    calls: Mutex<Vec<FetchTarget>>,
    script: Mutex<VecDeque<Result<String, FetchError>>>,
}

impl MockFetcher {
    fn new(script: Vec<Result<String, FetchError>>) -> Arc<Self> {
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
            .unwrap_or_else(|| Ok("mock-body".to_owned()));
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
                min_interval: Duration::ZERO, // 测试不限速
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

    let fetcher = MockFetcher::new(vec![Ok("论文摘要正文".to_owned())]);
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
        Ok("第二次成功".to_owned()),
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

    let fetcher = MockFetcher::new(vec![Ok("正文".to_owned())]);
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

    let fetcher = MockFetcher::new(vec![Ok("恢复后的正文".to_owned())]);
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
