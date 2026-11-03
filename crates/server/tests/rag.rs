//! RAG 2.0 服务端地基的集成测试（ADR-19 决策 2/3）。
//!
//! 分两类：
//! - **无数据库**（默认 `cargo test` 会跑）：盐下发、参数校验、配额、鉴权；
//! - **需要 PostgreSQL**（带 `#[ignore]`，`--include-ignored` 才跑）：盐持久化跨"重启"、
//!   跨月不同、上报累加、审计不落 hash、Top N 视图、表里没有 user_id。

mod common;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{sha256_hex, TestDb};
use yunong_server::config::Config;
use yunong_server::state::AppState;

const PASSWORD: &str = "password-123456";

struct RagApp {
    state: AppState,
}

impl RagApp {
    fn new(config: Config, db: Option<sqlx::PgPool>) -> Self {
        Self {
            state: AppState::new(config, db),
        }
    }

    fn memory() -> Self {
        Self::new(Config::for_tests(), None)
    }

    async fn call(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = yunong_server::app(self.state.clone())
            .oneshot(request)
            .await
            .expect("服务应能处理请求");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn register(&self, email: &str) -> String {
        let (status, body) = self
            .call(json_req(
                "POST",
                "/api/v1/auth/register",
                json!({ "email": email, "password": PASSWORD }),
                None,
            ))
            .await;
        assert_eq!(status, StatusCode::CREATED, "注册应成功：{body}");
        body["data"]["access_token"]
            .as_str()
            .expect("access_token")
            .to_owned()
    }

    async fn get_salt(&self, token: &str) -> (StatusCode, Value) {
        self.call(json_req(
            "GET",
            "/api/v1/rag/salt",
            Value::Null,
            Some(token),
        ))
        .await
    }

    async fn post_referrals(&self, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(json_req("POST", "/api/v1/rag/referrals", body, Some(token)))
            .await
    }
}

fn json_req(method: &str, uri: &str, body: Value, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Value::Null => builder.body(Body::empty()).expect("构造请求"),
        value => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .expect("构造请求"),
    }
}

/// 64 位小写十六进制的假哈希（内容无所谓，只要格式合法）。
fn fake_hash(seed: &str) -> String {
    sha256_hex(seed)
}

/// 轮询直到某条 `count(*)` 查询达到期望值（最多 5s）。
///
/// 为什么必须轮询：审计是**异步写**（`record_audit_detached` 里 `tokio::spawn`），
/// HTTP 响应返回时那一行可能还没落库。本地常常抢在写之前跑完、CI 上却会输掉竞速
/// （PR #39 的 pg job 就是这样红的）。固定 `sleep` 只是把竞态窗口挪小，不能消除。
async fn wait_for_count(pool: &sqlx::PgPool, sql: &str, expected: i64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if common::count(pool, sql).await >= expected {
            return;
        }
        assert!(Instant::now() < deadline, "5s 内没等到异步写入落库：{sql}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn current_month() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!("{}-{:02}", now.year(), u8::from(now.month()))
}

// ===================== 无数据库模式（默认跑） =====================

#[tokio::test]
async fn rag_endpoints_require_bearer() {
    let app = RagApp::memory();
    let (status, _) = app.get_salt("").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "盐下发需要令牌");

    let (status, _) = app
        .post_referrals("", json!({ "month": current_month(), "items": [] }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "上报需要令牌");
}

#[tokio::test]
async fn salt_is_64_hex_stable_and_has_month_expiry() {
    let app = RagApp::memory();
    let token = app.register("rag-salt@ynau.edu.cn").await;

    let (status, first) = app.get_salt(&token).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let data = &first["data"];
    let salt = data["salt"].as_str().expect("salt");
    assert_eq!(salt.len(), 64, "盐是 32 字节 = 64 位十六进制");
    assert!(
        salt.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "盐必须是小写十六进制：{salt}"
    );
    assert_eq!(
        data["month"].as_str().unwrap(),
        current_month(),
        "month 应是当前 UTC 月"
    );
    let expires_at = data["expires_at"].as_str().expect("expires_at");
    assert!(expires_at.contains('T'), "RFC3339：{expires_at}");

    // 同月稳定（缓存命中 + 同月不再生成）
    let (_, second) = app.get_salt(&token).await;
    assert_eq!(
        first["data"]["salt"], second["data"]["salt"],
        "同月盐必须稳定"
    );
    assert_eq!(
        first["data"]["expires_at"], second["data"]["expires_at"],
        "expires_at 也要稳定"
    );
}

#[tokio::test]
async fn salt_cache_can_be_bypassed_with_zero_ttl() {
    let mut config = Config::for_tests();
    config.rag_salt_ttl_secs = 0; // 每次都"过期"，但仍同月 → 盐不变
    let app = RagApp::new(config, None);
    let token = app.register("rag-ttl@ynau.edu.cn").await;
    let (_, first) = app.get_salt(&token).await;
    let (_, second) = app.get_salt(&token).await;
    assert_eq!(
        first["data"]["salt"], second["data"]["salt"],
        "TTL 只影响「是否查库」，不影响同月盐本身"
    );
}

#[tokio::test]
async fn referrals_reject_bad_month_hash_and_count() {
    let app = RagApp::memory();
    let token = app.register("rag-bad@ynau.edu.cn").await;
    let good = fake_hash("turn-1");

    // 月份：只接受当前月/上月
    let (status, body) = app
        .post_referrals(
            &token,
            json!({ "month": "2020-01", "items": [{ "hash": good, "count": 1 }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");

    // 哈希：大写 / 短 / 非十六进制
    for bad in [good.to_uppercase(), good[..63].to_string(), "z".repeat(64)] {
        let (status, body) = app
            .post_referrals(
                &token,
                json!({ "month": current_month(), "items": [{ "hash": bad, "count": 1 }] }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "应拒绝：{body}");
    }

    // count = 0 与服务端上限
    let (status, _) = app
        .post_referrals(
            &token,
            json!({ "month": current_month(), "items": [{ "hash": good, "count": 0 }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = app
        .post_referrals(
            &token,
            json!({ "month": current_month(), "items": [{ "hash": good, "count": 10_001 }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 空 items
    let (status, _) = app
        .post_referrals(&token, json!({ "month": current_month(), "items": [] }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn referrals_reject_more_than_200_items_per_request() {
    let app = RagApp::memory();
    let token = app.register("rag-many@ynau.edu.cn").await;
    let items: Vec<Value> = (0..201)
        .map(|index| json!({ "hash": fake_hash(&format!("h-{index}")), "count": 1 }))
        .collect();
    let (status, body) = app
        .post_referrals(&token, json!({ "month": current_month(), "items": items }))
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "单次 201 条应被拒：{body}"
    );
}

#[tokio::test]
async fn referrals_reject_over_monthly_quota_with_429() {
    let mut config = Config::for_tests();
    config.rag_max_items_per_month = 3;
    let app = RagApp::new(config, None);
    let token = app.register("rag-quota@ynau.edu.cn").await;
    let items: Vec<Value> = (0..4)
        .map(|index| json!({ "hash": fake_hash(&format!("q-{index}")), "count": 1 }))
        .collect();
    let (status, body) = app
        .post_referrals(&token, json!({ "month": current_month(), "items": items }))
        .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "超过单月配额应 429：{body}"
    );
    assert_eq!(body["error"]["code"], "RATE_LIMITED");
}

#[tokio::test]
async fn reporting_without_database_returns_not_implemented() {
    let app = RagApp::memory();
    let token = app.register("rag-nodb@ynau.edu.cn").await;
    let (status, body) = app
        .post_referrals(
            &token,
            json!({
                "month": current_month(),
                "items": [{ "hash": fake_hash("nodb"), "count": 1 }]
            }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_IMPLEMENTED,
        "无数据库模式不支持上报（不假装成功）：{body}"
    );
    assert_eq!(body["error"]["code"], "NOT_IMPLEMENTED");
}

// ===================== 需要 PostgreSQL（--include-ignored） =====================

/// 盐必须**落库**：换一个进程（新的 AppState）也要拿到同一个月的同一个盐，
/// 否则同月哈希无法聚合。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_salt_is_persisted_across_restarts() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let config = Config::for_tests();
    let first = RagApp::new(config.clone(), Some(db.pool.clone()));
    let salt_a = first.state.rag.salt().await.expect("取盐").salt;

    // 模拟重启：另一个 AppState（新的内存缓存），同一个库
    let second = RagApp::new(config, Some(db.pool.clone()));
    let salt_b = second.state.rag.salt().await.expect("取盐").salt;

    assert_eq!(
        salt_a, salt_b,
        "重启后同月盐必须不变（靠 rag_salts 持久化）"
    );
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM rag_salts WHERE length(salt) = 64"
        )
        .await,
        1,
        "同一个月只应有一行盐"
    );
    db.finish().await;
}

/// 跨月必须换盐（否则不同月份的哈希会混在一起）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_salt_differs_per_month_and_is_stable_within_month() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let app = RagApp::new(Config::for_tests(), Some(db.pool.clone()));

    let nov_a = app
        .state
        .rag
        .salt_for_month("2026-11")
        .await
        .expect("11 月盐");
    let nov_b = app
        .state
        .rag
        .salt_for_month("2026-11")
        .await
        .expect("11 月盐");
    let dec = app
        .state
        .rag
        .salt_for_month("2026-12")
        .await
        .expect("12 月盐");

    assert_eq!(nov_a, nov_b, "同月稳定");
    assert_ne!(nov_a, dec, "跨月必须换盐");
    db.finish().await;
}

/// 同 `(month, hash)` 累加；候选表同步累加且初始 `pending`。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_referrals_accumulate_and_create_pending_candidates() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let app = RagApp::new(Config::for_tests(), Some(db.pool.clone()));
    let token = app.register("rag-accumulate@ynau.edu.cn").await;
    let hash = fake_hash("accumulate-me");
    let month = current_month();

    for count in [2, 3] {
        let (status, body) = app
            .post_referrals(
                &token,
                json!({ "month": month, "items": [{ "hash": hash, "count": count }] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["accepted"], 1);
    }

    assert_eq!(
        common::count(
            &db.pool,
            &format!("SELECT count FROM rag_referrals WHERE month = '{month}' AND hash = '{hash}'")
        )
        .await,
        5,
        "2 + 3 应累加成 5"
    );
    assert_eq!(
        common::count(
            &db.pool,
            &format!("SELECT count(*) FROM rag_candidates WHERE month = '{month}' AND hash = '{hash}' AND status = 'pending'")
        )
        .await,
        1,
        "候选表应有一行 pending"
    );
    assert_eq!(
        common::text_column(
            &db.pool,
            &format!(
                "SELECT status FROM rag_candidates WHERE month = '{month}' AND hash = '{hash}'"
            )
        )
        .await
        .as_deref(),
        Some("pending")
    );
    db.finish().await;
}

/// 审计**不记 hash**：只有条数与月份。否则审计日志就成了浏览记录。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_audit_records_counts_but_never_the_hash() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let app = RagApp::new(Config::for_tests(), Some(db.pool.clone()));
    let token = app.register("rag-audit@ynau.edu.cn").await;
    let hash = fake_hash("secret-browsing-topic");
    let month = current_month();

    let (status, _) = app
        .post_referrals(
            &token,
            json!({ "month": month, "items": [{ "hash": hash, "count": 1 }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 有一条 rag.referrals 审计，且 reason 里只有 accepted=N。
    // 审计是异步写 → 先轮询等它落库（不能直接断言，也不能 sleep 固定时长）。
    wait_for_count(
        &db.pool,
        "SELECT count(*) FROM audit_logs WHERE action = 'rag.referrals' AND decision = 'allow'",
        1,
    )
    .await;
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM audit_logs WHERE action = 'rag.referrals' AND decision = 'allow'"
        )
        .await,
        1,
        "只应留一条 allow 审计（多了说明重复写）"
    );
    assert_eq!(
        common::text_column(
            &db.pool,
            "SELECT reason FROM audit_logs WHERE action = 'rag.referrals' ORDER BY created_at DESC LIMIT 1"
        )
        .await
        .as_deref(),
        Some("accepted=1")
    );

    // 关键断言：**任何审计字段都不含这个 hash**
    assert_eq!(
        common::count(
            &db.pool,
            &format!(
                "SELECT count(*) FROM audit_logs \
                 WHERE coalesce(target, '') LIKE '%{hash}%' \
                    OR coalesce(reason, '') LIKE '%{hash}%' \
                    OR coalesce(action, '') LIKE '%{hash}%'"
            )
        )
        .await,
        0,
        "审计里不允许出现 hash 内容（会变成浏览记录）"
    );
    db.finish().await;
}

/// 聚合视图按 count 排名（Top N）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_top_view_ranks_by_count() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let app = RagApp::new(Config::for_tests(), Some(db.pool.clone()));
    let token = app.register("rag-top@ynau.edu.cn").await;
    let month = current_month();

    let (top, mid, low) = (fake_hash("top"), fake_hash("mid"), fake_hash("low"));
    for (hash, count) in [(top.clone(), 9u32), (mid.clone(), 5), (low.clone(), 1)] {
        let (status, _) = app
            .post_referrals(
                &token,
                json!({ "month": month, "items": [{ "hash": hash, "count": count }] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    assert_eq!(
        common::text_column(
            &db.pool,
            &format!("SELECT hash FROM rag_top_referrals WHERE month = '{month}' AND rank = 1")
        )
        .await
        .as_deref(),
        Some(top.as_str()),
        "rank=1 应是 count 最大的"
    );
    assert_eq!(
        common::text_column(
            &db.pool,
            &format!("SELECT hash FROM rag_top_referrals WHERE month = '{month}' AND rank = 3")
        )
        .await
        .as_deref(),
        Some(low.as_str())
    );
    db.finish().await;
}

/// R5-F1：配额按 **count 总和** 算，不按条数。
///
/// 在旧实现（按条数）下，第二条请求（1 条）会被放行 ⇒ 这个用例会红。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_quota_counts_sum_of_counts() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut config = Config::for_tests();
    config.rag_max_items_per_month = 5;
    let app = RagApp::new(config, Some(db.pool.clone()));
    let token = app.register("rag-sum-quota@ynau.edu.cn").await;
    let month = current_month();

    // 3 + 2 = 5：条数只有 2，但 sum(count) 正好用满
    let (status, body) = app
        .post_referrals(
            &token,
            json!({ "month": month, "items": [
                { "hash": fake_hash("sum-a"), "count": 3 },
                { "hash": fake_hash("sum-b"), "count": 2 }
            ]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 再报 1 条（count=1）：按条数算会被放行，按 sum(count) 必须 429
    let (status, body) = app
        .post_referrals(
            &token,
            json!({ "month": month, "items": [
                { "hash": fake_hash("sum-c"), "count": 1 }
            ]}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "配额必须按 count 总和算（R5-F1）：{body}"
    );
    db.finish().await;
}

/// security 复核缺失的 PG 断言 ①：`rag_referrals` 的 `sum(count)` 实值。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_sum_count_matches_reported_totals() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let app = RagApp::new(Config::for_tests(), Some(db.pool.clone()));
    let token = app.register("rag-sum@ynau.edu.cn").await;
    let month = current_month();
    let hash = fake_hash("sum-value");

    for count in [3u32, 4, 5] {
        let (status, body) = app
            .post_referrals(
                &token,
                json!({ "month": month, "items": [{ "hash": hash, "count": count }] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    assert_eq!(
        common::count(
            &db.pool,
            &format!(
                "SELECT coalesce(sum(count), 0)::bigint FROM rag_referrals WHERE month = '{month}'"
            )
        )
        .await,
        12,
        "3 + 4 + 5 = 12"
    );
    assert_eq!(
        common::count(
            &db.pool,
            &format!("SELECT count FROM rag_referrals WHERE month = '{month}' AND hash = '{hash}'")
        )
        .await,
        12
    );
    assert_eq!(
        common::count(
            &db.pool,
            &format!(
                "SELECT count FROM rag_candidates WHERE month = '{month}' AND hash = '{hash}'"
            )
        )
        .await,
        12,
        "候选表与聚合表必须同步累加"
    );
    db.finish().await;
}

/// security 复核缺失的 PG 断言 ②：审计表 SQL 断言。
///
/// - 成功上报：`rag.referrals` + `allow` + `reason='accepted=N'`（N 是**条数**，不是 count 总和）；
/// - 配额拒绝：`rag.referrals` + `deny` + reason 含 `monthly_quota_exceeded`；
/// - 两种审计都**不含 hash**。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_audit_rows_distinguish_allow_and_deny() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut config = Config::for_tests();
    config.rag_max_items_per_month = 2;
    let app = RagApp::new(config, Some(db.pool.clone()));
    let token = app.register("rag-audit-sql@ynau.edu.cn").await;
    let month = current_month();
    let allowed = fake_hash("audit-allow");
    let rejected = fake_hash("audit-deny");

    // 1) 放行一次（2 条）
    let (status, _) = app
        .post_referrals(
            &token,
            json!({ "month": month, "items": [
                { "hash": allowed, "count": 1 },
                { "hash": fake_hash("audit-allow-2"), "count": 1 }
            ]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 2) 用满配额后再报：拒绝
    let (status, _) = app
        .post_referrals(
            &token,
            json!({ "month": month, "items": [{ "hash": rejected, "count": 1 }] }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // 审计异步写：allow 与 deny 各等一次落库（顺序不保证）
    wait_for_count(
        &db.pool,
        "SELECT count(*) FROM audit_logs WHERE action = 'rag.referrals' AND decision = 'allow'",
        1,
    )
    .await;
    wait_for_count(
        &db.pool,
        "SELECT count(*) FROM audit_logs WHERE action = 'rag.referrals' AND decision = 'deny'",
        1,
    )
    .await;
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM audit_logs WHERE action = 'rag.referrals' AND decision = 'allow'"
        )
        .await,
        1,
        "成功上报应有一条 allow 审计"
    );
    assert_eq!(
        common::text_column(
            &db.pool,
            "SELECT reason FROM audit_logs WHERE action = 'rag.referrals' AND decision = 'allow' LIMIT 1"
        )
        .await
        .as_deref(),
        Some("accepted=2"),
        "reason 里是**条数**（accepted=N）"
    );
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM audit_logs WHERE action = 'rag.referrals' AND decision = 'deny' \
             AND reason LIKE 'monthly_quota_exceeded%'"
        )
        .await,
        1,
        "配额拒绝也要留 deny 审计（便于解释为什么被限）"
    );

    // 两种审计都不含 hash
    for hash in [allowed, rejected] {
        assert_eq!(
            common::count(
                &db.pool,
                &format!(
                    "SELECT count(*) FROM audit_logs \
                     WHERE coalesce(target,'') LIKE '%{hash}%' \
                        OR coalesce(reason,'') LIKE '%{hash}%'"
                )
            )
            .await,
            0,
            "审计里不允许出现 hash"
        );
    }
    db.finish().await;
}

/// R5-F1：库侧封顶——即使有人长期刷，单个 `(month, hash)` 也不会超过 100_000。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_count_is_capped_at_hard_limit() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut config = Config::for_tests();
    // 让配额不挡路，只验库侧封顶
    config.rag_max_items_per_month = 1_000_000;
    let app = RagApp::new(config, Some(db.pool.clone()));
    let token = app.register("rag-cap@ynau.edu.cn").await;
    let month = current_month();
    let hash = fake_hash("cap-me");

    // 预置到 99_990（比上限少 10），再报 100 → 应被截断在 100_000
    sqlx::query("INSERT INTO rag_referrals (month, hash, count) VALUES ($1, $2, 99990)")
        .bind(&month)
        .bind(&hash)
        .execute(&db.pool)
        .await
        .expect("预置聚合行");
    sqlx::query(
        "INSERT INTO rag_candidates (month, hash, count, status) VALUES ($1, $2, 99990, 'pending')",
    )
    .bind(&month)
    .bind(&hash)
    .execute(&db.pool)
    .await
    .expect("预置候选行");

    let (status, body) = app
        .post_referrals(
            &token,
            json!({ "month": month, "items": [{ "hash": hash, "count": 100 }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        common::count(
            &db.pool,
            &format!("SELECT count FROM rag_referrals WHERE month = '{month}' AND hash = '{hash}'")
        )
        .await,
        100_000,
        "聚合值必须被 LEAST(..., 100000) 截断"
    );
    assert_eq!(
        common::count(
            &db.pool,
            &format!(
                "SELECT count FROM rag_candidates WHERE month = '{month}' AND hash = '{hash}'"
            )
        )
        .await,
        100_000,
        "候选表同样受上限约束"
    );
    db.finish().await;
}

/// R5-F5 端到端：**真正的上月**必须被接受（旧实现恒 422，这条会红）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_previous_month_is_accepted() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let app = RagApp::new(Config::for_tests(), Some(db.pool.clone()));
    let token = app.register("rag-prev-month@ynau.edu.cn").await;
    let previous = yunong_server::rag::RagService::previous_month_now();
    assert_ne!(previous, current_month(), "上月不能等于本月");

    let (status, body) = app
        .post_referrals(
            &token,
            json!({ "month": previous, "items": [{ "hash": fake_hash("prev-month"), "count": 1 }] }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "上月宽限必须生效（旧实现恒 422，这条会红）：{body}"
    );
    db.finish().await;
}

/// 隐私边界：三张表里都**没有** `user_id` 列（ADR-19 决策 2）。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_rag_tables_have_no_user_id_column() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name IN ('rag_salts', 'rag_referrals', 'rag_candidates') \
               AND column_name = 'user_id'"
        )
        .await,
        0,
        "RAG 表不允许有 user_id（服务端不该能回溯到账号）"
    );
    // 三张表 + 视图都在
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = current_schema() \
               AND table_name IN ('rag_salts', 'rag_referrals', 'rag_candidates', 'rag_top_referrals')"
        )
        .await,
        4
    );
    db.finish().await;
}

/// 迁移可重入：把 0005 再跑一遍（老库升级 / 重入场景）不应报错。
#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见 tests/common/mod.rs"]
async fn pg_migration_0005_is_idempotent() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migrations/0005_rag_referrals.sql"
    ))
    .expect("读迁移文件");
    // 重入两次：约束"先删后加"、建表/索引 IF NOT EXISTS、视图 CREATE OR REPLACE。
    // 注意用 `raw_sql`：迁移文件是多条语句，预编译协议（`sqlx::query`）一次只能一条。
    for attempt in 0..2 {
        sqlx::raw_sql(&sql)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("第 {} 次重入迁移失败：{error}", attempt + 1));
    }
    assert_eq!(
        common::count(
            &db.pool,
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = 'rag_referrals'"
        )
        .await,
        1
    );

    // 状态机受 CHECK 约束：非法状态写不进去（抓取管线将来只在这四个值里流转）
    let rejected = sqlx::query(
        "INSERT INTO rag_candidates (month, hash, count, status) \
         VALUES ('2026-11', 'deadbeef', 1, 'bogus')",
    )
    .execute(&db.pool)
    .await;
    assert!(rejected.is_err(), "非法 status 应被 CHECK 约束拒绝");

    db.finish().await;
}
