//! PostgreSQL 集成测试的共用脚手架（`pg_*` 与 chat 记账测试都用它）。
//!
//! 默认 `cargo test` **不会跑**依赖它的用例：那些用例都带 `#[ignore]`，
//! 没装 PostgreSQL 的机器依然全绿。显式开启（本地或 CI）：
//!
//! ```text
//! docker run --rm -e POSTGRES_PASSWORD=postgres -p 5432:5432 pgvector/pgvector:pg16
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo test -p yunong-server --test pg_auth -- --include-ignored
//! ```
//!
//! 约定：
//!
//! - **没有 `DATABASE_URL` 就跳过**：打印一行提示后让用例直接 return，算通过；
//! - **设了 `DATABASE_URL` 就必须跑通**：连不上库、装不了 pgvector、迁移失败都算失败。
//!   显式开启的路径不允许静默跳过，否则"绿了但什么都没测"是最贵的假绿灯；
//! - CI 里额外设 `YUNONG_PG_TESTS_REQUIRED=1`，把"忘了传 `DATABASE_URL`"也变成失败；
//! - 每条用例在**自己的 schema**（`yunong_pg_<uuid>`）里跑，连接池的 `search_path`
//!   指过去，因此可以并行、互不污染；用例结束把 schema 删掉；
//! - 表结构来自 `crates/server/migrations/*.sql`：跑的是真实迁移，不是手抄的建表语句。
#![allow(dead_code)] // 每个测试二进制只用到其中一部分

use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, PgConnection, Row};

use uuid::Uuid;

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
pub struct TestDb {
    pub pool: PgPool,
    /// 建/删 schema 用的直连串（`search_path` 保持默认，不受测试 schema 影响）。
    url: String,
    schema: String,
}

impl TestDb {
    /// 只含**本用例 schema** 的池（search_path 不带 `public`）。
    ///
    /// 用于"库里到底有没有这一列 / 这一行"这类**结构断言**：开发机和 CI 的 public
    /// 往往已经跑过迁移，若 search_path 里带着 public，断言会观察到 public 的表而失效。
    pub async fn pool_without_public(&self) -> PgPool {
        let options = PgConnectOptions::from_str(&self.url)
            .unwrap_or_else(|error| panic!("DATABASE_URL 不是合法的连接串：{error}"))
            .application_name("yunong-pg-tests-isolated")
            .options([("search_path", self.schema.clone())]);
        PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap_or_else(|error| panic!("建隔离连接池失败：{error}"))
    }

    /// 没有 `DATABASE_URL` → `None`（用例直接 return，等于优雅跳过）。
    ///
    /// 有 `DATABASE_URL` 却连不上 → panic：这条路是显式开启的，环境坏了必须看得见。
    pub async fn setup() -> Option<Self> {
        let Some(url) = database_url() else {
            assert!(
                !tests_required(),
                "设了 {REQUIRE_ENV}=1 却没给 DATABASE_URL：这个 job 必须真的连库跑 SQL"
            );
            eprintln!("跳过 PostgreSQL 集成测试：没有设置 DATABASE_URL（见 tests/common/mod.rs）");
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

    /// 在测试 schema 里执行一条 DDL/任意 SQL（用例需要"制造故障"时用）。
    pub async fn execute(&self, sql: &str) {
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .unwrap_or_else(|error| panic!("执行 SQL 失败（{sql}）：{error}"));
    }

    /// 用完删掉 schema；删不掉只提示，不改写已经得出的结论。
    pub async fn finish(self) {
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
        panic!("连不上 PostgreSQL：{error}（起库方法见 tests/common/mod.rs 文件头）")
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
pub fn sha256_hex(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// 只查一个 `bigint` 的 `SELECT`（用例里都是 count）。
pub async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|error| panic!("查询失败（{sql}）：{error}"))
}

/// 取一行里某个 `text` 列（没有行就返回 `None`）。
pub async fn text_column(pool: &PgPool, sql: &str) -> Option<String> {
    sqlx::query(sql)
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|error| panic!("查询失败（{sql}）：{error}"))
        .map(|row| row.get::<String, _>(0))
}
