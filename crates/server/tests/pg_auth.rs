//! PostgreSQL 集成测试：让 `auth::pg` 里的**真实 SQL** 真的跑一遍。
//!
//! 默认 `cargo test` **不会跑这里**：每条用例都带 `#[ignore]`，没装 PostgreSQL 的机器
//! 依然全绿。显式开启（本地或 CI）：
//!
//! ```text
//! docker run --rm -e POSTGRES_PASSWORD=postgres -p 5432:5432 pgvector/pgvector:pg16
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo test -p yunong-server --test pg_auth -- --include-ignored
//! ```
//!
//! 约定：
//!
//! - **没有 `DATABASE_URL` 就跳过**：打印一行提示后直接返回，用例算通过；
//! - **设了 `DATABASE_URL` 就必须跑通**：连不上库、装不了 pgvector、迁移失败都算失败。
//!   显式开启的路径不允许静默跳过，否则"绿了但什么都没测"是最贵的假绿灯；
//! - CI 里额外设 `YUNONG_PG_TESTS_REQUIRED=1`，把"忘了传 `DATABASE_URL`"也变成失败；
//! - 每条用例在**自己的 schema**（`yunong_pg_<uuid>`）里跑，连接池的 `search_path`
//!   指过去，因此可以并行、互不污染；用例结束把 schema 删掉；
//! - 表结构来自 `crates/server/migrations/*.sql`：跑的是真实迁移，不是手抄的建表语句。

use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, PgConnection, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use yunong_server::auth::{AuthService, ACCESS_TOKEN_TTL_SECS, REFRESH_TOKEN_TTL_SECS};
use yunong_shared::ErrorCode;

const EMAIL: &str = "student@ynau.edu.cn";
const PASSWORD: &str = "correct-horse-battery";

/// 设成 `1` 时，"没有 DATABASE_URL 就跳过"升级为失败（CI 里就是这么设的）。
const REQUIRE_ENV: &str = "YUNONG_PG_TESTS_REQUIRED";

/// 真实迁移目录。集成测试里的 `CARGO_MANIFEST_DIR` 正是 `crates/server`。
const MIGRATIONS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");

/// 串行化"建扩展 + 建 schema"：并发 `CREATE EXTENSION IF NOT EXISTS` 会撞
/// `pg_extension` 的唯一索引，用事务级 advisory lock 排队最省事。
/// 数值本身没有含义，只要全仓库唯一。
const SETUP_LOCK_ID: i64 = 0x796E_756E_6F6E_6700;

fn database_url() -> Option<String> {
    env::var("DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn tests_required() -> bool {
    matches!(
        env::var(REQUIRE_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// 一条用例独占的库环境：独立 schema + 指向它的连接池。
struct TestDb {
    pool: PgPool,
    /// 建/删 schema 用的直连串（`search_path` 保持默认，不受测试 schema 影响）。
    url: String,
    schema: String,
}

impl TestDb {
    /// 没有 `DATABASE_URL` → `None`（用例直接 return，等于优雅跳过）。
    ///
    /// 有 `DATABASE_URL` 却连不上 → panic：这条路是显式开启的，环境坏了必须看得见。
    async fn setup() -> Option<Self> {
        let Some(url) = database_url() else {
            assert!(
                !tests_required(),
                "设了 {REQUIRE_ENV}=1 却没给 DATABASE_URL：这个 job 必须真的连库跑 SQL"
            );
            eprintln!("跳过 PostgreSQL 集成测试：没有设置 DATABASE_URL（见 tests/pg_auth.rs）");
            return None;
        };

        let schema = format!("yunong_pg_{}", Uuid::new_v4().simple());
        let mut admin = connect(&url).await;

        exec(&mut admin, "BEGIN").await;
        exec(
            &mut admin,
            &format!("SELECT pg_advisory_xact_lock({SETUP_LOCK_ID})"),
        )
        .await;
        // 迁移 0001 的第一句就是 CREATE EXTENSION vector：装在 public 一次，
        // 后面每个测试 schema 的迁移就只是一句无害的 no-op。
        if let Err(error) = sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&mut admin)
            .await
        {
            panic!(
                "建 vector 扩展失败：{error}\n\
                 迁移 0001 需要 pgvector；请用带扩展的镜像：\n\
                 docker run --rm -e POSTGRES_PASSWORD=postgres -p 5432:5432 pgvector/pgvector:pg16"
            );
        }
        exec(&mut admin, &format!("CREATE SCHEMA {schema}")).await;
        exec(&mut admin, "COMMIT").await;

        // 业务连接池：search_path 里测试 schema 排第一，public 兜住 pgvector。
        // 注意逗号后不能有空格——startup options 是按空白切分的，写成 "a, public"
        // 会被 PostgreSQL 当成两个参数。
        let connect_options = PgConnectOptions::from_str(&url)
            .unwrap_or_else(|error| panic!("DATABASE_URL 不是合法的连接串：{error}"))
            .application_name("yunong-pg-tests")
            .options([("search_path", format!("{schema},public"))]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(connect_options)
            .await
            .unwrap_or_else(|error| panic!("建立连接池失败：{error}"));

        apply_migrations(&pool).await;

        Some(Self { pool, url, schema })
    }

    /// 用完删掉 schema；删不掉只提示，不改写已经得出的结论。
    async fn finish(self) {
        let Self { pool, url, schema } = self;
        pool.close().await;
        let mut admin = connect(&url).await;
        if let Err(error) = sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
            .execute(&mut admin)
            .await
        {
            eprintln!("清理测试 schema {schema} 失败（不影响结论）：{error}");
        }
    }
}

async fn connect(url: &str) -> PgConnection {
    PgConnection::connect(url).await.unwrap_or_else(|error| {
        panic!("连不上 PostgreSQL：{error}（起库方法见 tests/pg_auth.rs 文件头）")
    })
}

async fn exec(conn: &mut PgConnection, sql: &str) {
    if let Err(error) = sqlx::query(sql).execute(&mut *conn).await {
        panic!("执行 SQL 失败（{sql}）：{error}");
    }
}

/// 按文件名顺序把真实迁移跑进当前 `search_path`。
///
/// 一个文件一次执行（`raw_sql` 走简单查询协议，支持多语句），
/// 迁移里有 `CREATE EXTENSION` / `CREATE INDEX` 这类必须按序执行的 DDL。
async fn apply_migrations(pool: &PgPool) {
    let mut files = migration_files();
    assert!(!files.is_empty(), "迁移目录里没有 .sql：{MIGRATIONS_DIR}");
    files.sort();
    for file in files {
        let sql = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("读迁移文件 {} 失败：{error}", file.display()));
        sqlx::raw_sql(&sql)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("执行迁移 {} 失败：{error}", file.display()));
    }
}

fn migration_files() -> Vec<PathBuf> {
    std::fs::read_dir(MIGRATIONS_DIR)
        .unwrap_or_else(|error| panic!("读迁移目录 {MIGRATIONS_DIR} 失败：{error}"))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sql"))
        .collect()
}

/// 令牌摘要算法（与应用侧 `token::hash` 同一套）：SHA-256 的 hex。
fn sha256_hex(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// 一张令牌表里的一行（只取断言要用的列）。
struct TokenRow {
    user_id: Uuid,
    expires_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
}

async fn token_row(pool: &PgPool, table: &str, token_hash: &str) -> Option<TokenRow> {
    assert!(
        matches!(table, "access_tokens" | "refresh_tokens"),
        "只允许这两张令牌表：{table}"
    );
    let sql = format!("SELECT user_id, expires_at, revoked_at FROM {table} WHERE token_hash = $1");
    sqlx::query(&sql)
        .bind(token_hash)
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|error| panic!("查 {table} 失败：{error}"))
        .map(|row| TokenRow {
            user_id: row.get("user_id"),
            expires_at: row.get("expires_at"),
            revoked_at: row.get("revoked_at"),
        })
}

/// 只查一个 `bigint` 的 `SELECT`（用例里都是 count）。
async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|error| panic!("查询失败（{sql}）：{error}"))
}

// ============ 注册 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn register_writes_argon2id_hash_and_only_token_digests() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    assert_eq!(
        auth.backend_kind(),
        "postgres",
        "这条用例必须跑在 Pg 后端上，否则等于什么都没测"
    );

    // 大小写与空白都由服务层规范化：落库必须是 lower(email)，与唯一索引一致
    let session = auth
        .register("  Student@YNAU.edu.cn ", PASSWORD)
        .await
        .expect("注册应成功");
    assert_eq!(session.expires_in, ACCESS_TOKEN_TTL_SECS);

    let user = sqlx::query("SELECT id, email, password_hash FROM users")
        .fetch_one(&db.pool)
        .await
        .expect("应能从库里读出刚注册的用户");
    assert_eq!(user.get::<Uuid, _>("id"), session.user_id);
    assert_eq!(user.get::<String, _>("email"), EMAIL);
    let phc: String = user.get("password_hash");
    assert!(
        phc.starts_with("$argon2id$"),
        "密码只存 Argon2id PHC：{phc}"
    );
    assert!(!phc.contains(PASSWORD), "库里不能出现明文密码");

    // 访问令牌：只落 SHA-256 摘要，有效期 15 分钟
    let access = token_row(
        &db.pool,
        "access_tokens",
        &sha256_hex(&session.access_token),
    )
    .await
    .expect("访问令牌摘要应已落库");
    assert_eq!(access.user_id, session.user_id);
    assert!(access.revoked_at.is_none());
    let access_ttl = (access.expires_at - OffsetDateTime::now_utc()).whole_seconds();
    assert!(
        (ACCESS_TOKEN_TTL_SECS - 120..=ACCESS_TOKEN_TTL_SECS).contains(&access_ttl),
        "访问令牌有效期应约 {ACCESS_TOKEN_TTL_SECS}s，实际 {access_ttl}s"
    );

    // 刷新令牌：30 天
    let refresh = token_row(
        &db.pool,
        "refresh_tokens",
        &sha256_hex(&session.refresh_token),
    )
    .await
    .expect("刷新令牌摘要应已落库");
    assert!(refresh.revoked_at.is_none());
    let refresh_ttl = (refresh.expires_at - OffsetDateTime::now_utc()).whole_seconds();
    assert!(
        (REFRESH_TOKEN_TTL_SECS - 120..=REFRESH_TOKEN_TTL_SECS).contains(&refresh_ttl),
        "刷新令牌有效期应约 {REFRESH_TOKEN_TTL_SECS}s，实际 {refresh_ttl}s"
    );

    // 明文令牌一次都不许落库
    for (table, token) in [
        ("access_tokens", session.access_token.as_str()),
        ("refresh_tokens", session.refresh_token.as_str()),
    ] {
        let sql = format!("SELECT count(*) FROM {table} WHERE token_hash = $1");
        let hits: i64 = sqlx::query_scalar(&sql)
            .bind(token)
            .fetch_one(&db.pool)
            .await
            .expect("查明文令牌");
        assert_eq!(hits, 0, "{table} 里不许出现明文令牌");
    }

    db.finish().await;
}

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn duplicate_email_is_validation_error_not_internal() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    auth.register(EMAIL, PASSWORD)
        .await
        .expect("首次注册应成功");

    let error = auth
        .register(&EMAIL.to_ascii_uppercase(), PASSWORD)
        .await
        .expect_err("同邮箱第二次注册必须失败");
    // 唯一索引冲突要映射成 422（`pg::register` 的 is_unique_violation 分支），而不是 500
    assert_eq!(error.code, ErrorCode::ValidationError, "{}", error.message);
    assert_eq!(count(&db.pool, "SELECT count(*) FROM users").await, 1);
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM access_tokens").await,
        1,
        "失败路径不能再签发令牌"
    );

    db.finish().await;
}

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn concurrent_register_of_same_email_has_exactly_one_winner() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));

    let (first, second) = tokio::join!(
        auth.register(EMAIL, PASSWORD),
        auth.register(EMAIL, PASSWORD)
    );
    let winners = [first.is_ok(), second.is_ok()]
        .iter()
        .filter(|won| **won)
        .count();
    assert_eq!(
        winners, 1,
        "唯一索引只允许一个插入成功：{first:?} / {second:?}"
    );
    let loser = match first {
        Err(error) => error,
        Ok(_) => second.expect_err("必须有一个失败"),
    };
    assert_eq!(loser.code, ErrorCode::ValidationError, "{}", loser.message);
    assert_eq!(count(&db.pool, "SELECT count(*) FROM users").await, 1);

    db.finish().await;
}

// ============ 登录 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn login_reads_user_row_and_reissues_a_fresh_pair() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));

    let registered = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");
    // 登录时邮箱同样规范化：大小写不影响命中
    let logged_in = auth
        .login(&EMAIL.to_ascii_uppercase(), PASSWORD)
        .await
        .expect("登录应成功");
    assert_eq!(logged_in.user_id, registered.user_id);
    assert_ne!(logged_in.access_token, registered.access_token);
    assert_ne!(logged_in.refresh_token, registered.refresh_token);
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM access_tokens").await,
        2,
        "注册与登录各签发一对"
    );
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM refresh_tokens").await,
        2
    );

    // 密码错与账号不存在：同一个错误码、同一句话，且都不签发令牌
    let wrong_password = auth
        .login(EMAIL, "not-the-password")
        .await
        .expect_err("密码错必须失败");
    let unknown_user = auth
        .login("nobody@ynau.edu.cn", PASSWORD)
        .await
        .expect_err("账号不存在必须失败");
    assert_eq!(wrong_password.code, ErrorCode::Unauthorized);
    assert_eq!(unknown_user.code, ErrorCode::Unauthorized);
    assert_eq!(
        wrong_password.message, unknown_user.message,
        "不能泄露账号是否存在"
    );
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM access_tokens").await,
        2,
        "登录失败不能再签发令牌"
    );

    // `disabled_at IS NULL` 这条过滤必须真的生效
    let disabled = sqlx::query("UPDATE users SET disabled_at = now()")
        .execute(&db.pool)
        .await
        .expect("停用用户");
    assert_eq!(disabled.rows_affected(), 1, "应正好停用一个用户");
    let error = auth
        .login(EMAIL, PASSWORD)
        .await
        .expect_err("停用后不允许登录");
    assert_eq!(error.code, ErrorCode::Unauthorized);

    db.finish().await;
}

// ============ 访问令牌校验 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn authenticate_tells_expired_apart_from_revoked() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    let session = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");

    let principal = auth
        .authenticate(&session.access_token)
        .await
        .expect("刚签发的访问令牌应可用");
    assert_eq!(principal.user_id, session.user_id);
    assert_eq!(principal.device_id, None, "W2 还没有设备维度");
    assert_eq!(principal.token_hash, sha256_hex(&session.access_token));

    // 库里没有、但格式合法的令牌 → 401
    let unknown = auth
        .authenticate(&"a".repeat(64))
        .await
        .expect_err("未知令牌必须失败");
    assert_eq!(unknown.code, ErrorCode::Unauthorized);

    // 过期 → TOKEN_EXPIRED：客户端据此走 refresh，而不是弹登录框
    let hash = sha256_hex(&session.access_token);
    let sql =
        "UPDATE access_tokens SET expires_at = now() - interval '1 second' WHERE token_hash = $1";
    sqlx::query(sql)
        .bind(&hash)
        .execute(&db.pool)
        .await
        .expect("把令牌改成过期");
    let expired = auth
        .authenticate(&session.access_token)
        .await
        .expect_err("过期令牌必须失败");
    assert_eq!(expired.code, ErrorCode::TokenExpired, "{}", expired.message);

    // 已吊销（且没过期）→ 401 而不是 TOKEN_EXPIRED：重试也好不了，得重新登录
    let sql = "UPDATE access_tokens \
               SET expires_at = now() + interval '15 minutes', revoked_at = now() \
               WHERE token_hash = $1";
    sqlx::query(sql)
        .bind(&hash)
        .execute(&db.pool)
        .await
        .expect("把令牌改成已吊销");
    let revoked = auth
        .authenticate(&session.access_token)
        .await
        .expect_err("已吊销令牌必须失败");
    assert_eq!(revoked.code, ErrorCode::Unauthorized, "{}", revoked.message);

    db.finish().await;
}

// ============ 刷新轮换 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn refresh_rotates_pair_and_revokes_the_old_refresh_token() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    let session = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");

    let old_hash = sha256_hex(&session.refresh_token);
    let rotated = auth
        .refresh(&session.refresh_token)
        .await
        .expect("首次刷新应成功");
    assert_eq!(rotated.user_id, session.user_id);
    assert_ne!(rotated.refresh_token, session.refresh_token);
    assert_ne!(rotated.access_token, session.access_token);

    // 旧刷新令牌：当场吊销（轮换是"用掉即作废"，不是攒着等过期）
    let old = token_row(&db.pool, "refresh_tokens", &old_hash)
        .await
        .expect("旧刷新令牌应还在库里");
    assert!(old.revoked_at.is_some(), "旧刷新令牌必须当场作废");
    // 新的一对都已落库且未吊销
    for (table, token) in [
        ("access_tokens", rotated.access_token.as_str()),
        ("refresh_tokens", rotated.refresh_token.as_str()),
    ] {
        let row = token_row(&db.pool, table, &sha256_hex(token))
            .await
            .expect("新令牌应已落库");
        assert!(row.revoked_at.is_none(), "{table} 的新令牌不该被吊销");
    }
    // 新访问令牌立刻可用
    auth.authenticate(&rotated.access_token)
        .await
        .expect("刷新后新的访问令牌应可用");

    // 重放旧刷新令牌 → 401（轮换后不可复用）
    let replay = auth
        .refresh(&session.refresh_token)
        .await
        .expect_err("旧刷新令牌不能重放");
    assert_eq!(replay.code, ErrorCode::Unauthorized, "{}", replay.message);

    // 过期的刷新令牌 → 401
    let new_hash = sha256_hex(&rotated.refresh_token);
    let sql =
        "UPDATE refresh_tokens SET expires_at = now() - interval '1 second' WHERE token_hash = $1";
    sqlx::query(sql)
        .bind(&new_hash)
        .execute(&db.pool)
        .await
        .expect("把刷新令牌改成过期");
    let expired = auth
        .refresh(&rotated.refresh_token)
        .await
        .expect_err("过期刷新令牌必须失败");
    assert_eq!(expired.code, ErrorCode::Unauthorized, "{}", expired.message);

    // 库里根本没见过的 → 401
    let unknown = auth
        .refresh(&"b".repeat(64))
        .await
        .expect_err("未知刷新令牌必须失败");
    assert_eq!(unknown.code, ErrorCode::Unauthorized);

    db.finish().await;
}

// ============ 登出吊销 ============

#[tokio::test]
#[ignore = "需要 PostgreSQL（DATABASE_URL），见文件头"]
async fn logout_revokes_current_access_token_and_all_refresh_tokens() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let auth = AuthService::new(Some(db.pool.clone()));
    let first = auth.register(EMAIL, PASSWORD).await.expect("注册应成功");
    let second = auth
        .login(EMAIL, PASSWORD)
        .await
        .expect("第二个会话应登录成功");

    let principal = auth
        .authenticate(&first.access_token)
        .await
        .expect("登出前访问令牌应可用");
    auth.logout(&principal).await.expect("登出应成功");

    // 当前访问令牌立刻失效（按 token_hash 精确吊销）
    let error = auth
        .authenticate(&first.access_token)
        .await
        .expect_err("登出后访问令牌必须失效");
    assert_eq!(error.code, ErrorCode::Unauthorized, "{}", error.message);
    // 另一个会话的访问令牌不受牵连
    auth.authenticate(&second.access_token)
        .await
        .expect("另一个会话的访问令牌不该被吊销");
    // 但刷新令牌是按 user_id 清空的：两个会话都得重新登录
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM refresh_tokens WHERE revoked_at IS NULL"
        )
        .await,
        0,
        "登出要把该账号所有未失效的刷新令牌都吊销"
    );
    let error = auth
        .refresh(&second.refresh_token)
        .await
        .expect_err("刷新令牌应已被一并吊销");
    assert_eq!(error.code, ErrorCode::Unauthorized, "{}", error.message);
    assert!(
        error.message.contains("失效"),
        "应是「已失效」而不是「无效」：{}",
        error.message
    );

    db.finish().await;
}
