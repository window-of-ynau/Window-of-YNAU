//! 索引"已知语料库"（ADR-19 决策 3 的前置步骤）。
//!
//! 用法：
//!
//! ```text
//! DATABASE_URL=postgres://… cargo run -p yunong-server --bin rag_index_known_docs -- \
//!     --seed scripts/rag_known_docs.seed.json [--month 2026-11] [--retention-months 3]
//! ```
//!
//! 做的事：
//! 1. 读 seed（JSON：`{"docs":[{"identifier":"2401.01234","source":"arxiv"}]}`）；
//! 2. 规范化标识（与服务端/客户端同一套 `canonical_identifier`）；
//! 3. 写 `rag_known_docs(month, identifier, source)` —— **不写 hash**：
//!    哈希只在抓取 worker 运行期按当月盐现算，静态数据里不存在"哈希→明文"的映射；
//! 4. 顺手清理过期月份（语料与月度盐各自独立的保留期）。
//!
//! 注意：脚本**不打印任何哈希**，只打印计数——日志不该变成可 join 的字典。

use std::process::ExitCode;

use sqlx::postgres::PgPoolOptions;

use yunong_server::config::Config;
use yunong_server::rag::RagService;
use yunong_server::rag_fetch::{self, index_known_docs, prune_salts};

#[tokio::main]
async fn main() -> ExitCode {
    yunong_server::init_tracing();

    let mut seed_path: Option<String> = None;
    let mut month: Option<String> = None;
    let mut retention_months: Option<u32> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seed" => seed_path = args.next(),
            "--month" => month = args.next(),
            "--retention-months" => {
                retention_months = args.next().and_then(|value| value.parse().ok())
            }
            "--help" | "-h" => {
                println!(
                    "用法：rag_index_known_docs --seed <path> [--month YYYY-MM] [--retention-months N]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("未知参数：{other}（--help 看用法）");
                return ExitCode::from(2);
            }
        }
    }

    let Some(seed_path) = seed_path else {
        eprintln!("缺少 --seed（--help 看用法）");
        return ExitCode::from(2);
    };

    let config = Config::from_env();
    let Some(database_url) = config.database_url.clone() else {
        eprintln!("缺少 DATABASE_URL：索引已知语料需要数据库");
        return ExitCode::from(2);
    };

    let month = month.unwrap_or_else(rag_fetch::current_month);
    let retention = retention_months.unwrap_or(config.rag_known_docs_retention_months);

    let seed_text = match std::fs::read_to_string(&seed_path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("读 seed 失败（{seed_path}）：{error}");
            return ExitCode::FAILURE;
        }
    };

    #[derive(serde::Deserialize)]
    struct Seed {
        docs: Vec<SeedDoc>,
    }
    #[derive(serde::Deserialize)]
    struct SeedDoc {
        identifier: String,
        source: String,
    }

    let seed: Seed = match serde_json::from_str(&seed_text) {
        Ok(seed) => seed,
        Err(error) => {
            eprintln!("seed 不是合法 JSON：{error}");
            return ExitCode::FAILURE;
        }
    };

    let pool = match PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("连接数据库失败：{error}");
            return ExitCode::FAILURE;
        }
    };

    // 盐必须已存在（或由 RagService 现场生成）：索引本身不写 hash，但抓取 worker 会用同一个盐。
    let rag = RagService::new(&config, Some(pool.clone()));
    let salt = match rag.salt_for_month(&month).await {
        Ok(salt) => salt,
        Err(error) => {
            eprintln!("取 {month} 的盐失败：{error:?}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(month = %month, salt_len = salt.len(), "开始索引已知语料");

    let docs: Vec<(String, String)> = seed
        .docs
        .into_iter()
        .map(|doc| (doc.identifier, doc.source))
        .collect();
    let total = docs.len();

    match index_known_docs(&pool, &month, &docs, retention).await {
        Ok(written) => {
            tracing::info!(month = %month, total, written, retention, "已知语料索引完成");
            println!("indexed {written}/{total} docs for {month} (retention {retention} months)");
        }
        Err(error) => {
            eprintln!("写库失败：{error}");
            return ExitCode::FAILURE;
        }
    }

    // 月度盐的保留期与语料保留期分开配置
    match prune_salts(&pool, &month, config.rag_salts_retention_months).await {
        Ok(removed) => {
            if removed > 0 {
                tracing::info!(
                    removed,
                    retention = config.rag_salts_retention_months,
                    "清理过期月度盐"
                );
            }
        }
        Err(error) => {
            eprintln!("清理月度盐失败：{error}");
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}
