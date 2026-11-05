//! 邀请码管理（ADR-11 1.0 邀请制方案，t141）。
//!
//! 用法：
//!
//! ```text
//! DATABASE_URL=postgres://… cargo run -p yunong-server --bin invite_admin -- \
//!     generate --batch 2026-fall --count 50 [--max-uses 1] [--expires-days 30] \
//!              [--issued-by ops] [--note "2026 秋季"]
//! DATABASE_URL=… cargo run -p yunong-server --bin invite_admin -- revoke --batch 2026-fall
//! DATABASE_URL=… cargo run -p yunong-server --bin invite_admin -- stats  --batch 2026-fall
//! ```
//!
//! 约定：
//! - 明文邀请码**只在 `generate` 时打印一次**；库里只落 SHA-256（`invite_codes.code_hash`）。
//!   丢了明文就只能作废重发——这是刻意的（库被读走也拿不到能用的码）。
//! - 邀请码是 32 个小写 hex 字符（128 bit 随机），前缀 `ynau-` 便于人工识别。
//! - `revoke` 按**批次**停发：已核销的账号不受影响（撤销只影响未使用的码）。

use std::process::ExitCode;

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use yunong_server::auth::AuthService;
use yunong_server::config::Config;

/// 邀请码前缀：便于在聊天/邮件里一眼认出来。
const CODE_PREFIX: &str = "ynau-";

fn main_usage() -> ExitCode {
    println!(
        "用法：
  invite_admin generate --batch <批次> --count <N> [--max-uses <M>] [--expires-days <D>]
                        [--issued-by <谁>] [--note <备注>]
  invite_admin revoke   --batch <批次>
  invite_admin stats    --batch <批次>

环境变量：DATABASE_URL（必需，邀请码只落库）"
    );
    ExitCode::SUCCESS
}

/// 生成一个邀请码：`ynau-` + 32 个 hex（两段 UUID v4 拼起来再截断 = 128 bit 随机）。
fn new_code() -> String {
    let mut hex = String::with_capacity(32);
    for uuid in [Uuid::new_v4(), Uuid::new_v4()] {
        let mut buffer = Uuid::encode_buffer();
        hex.push_str(uuid.simple().encode_lower(&mut buffer));
    }
    hex.truncate(32);
    format!("{CODE_PREFIX}{hex}")
}

#[tokio::main]
async fn main() -> ExitCode {
    yunong_server::init_tracing();

    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return main_usage();
    };
    // 顶层 `--help` 也要认（否则会被当成子命令，然后报"缺少 --batch"）
    if matches!(command.as_str(), "--help" | "-h") {
        return main_usage();
    }

    let mut batch: Option<String> = None;
    let mut count: Option<usize> = None;
    let mut max_uses: i32 = 1;
    let mut expires_days: Option<i64> = Some(30);
    let mut issued_by = "ops".to_owned();
    let mut note: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--batch" => batch = args.next(),
            "--count" => count = args.next().and_then(|value| value.parse().ok()),
            "--max-uses" => {
                max_uses = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1)
                    .max(1)
            }
            "--expires-days" => {
                expires_days = match args.next().as_deref() {
                    // 显式 `0` = 永不过期（与"不传"区分开）
                    Some("0") | Some("never") => None,
                    Some(value) => Some(value.parse().unwrap_or(30)),
                    None => Some(30),
                }
            }
            "--issued-by" => issued_by = args.next().unwrap_or_else(|| "ops".to_owned()),
            "--note" => note = args.next(),
            "--help" | "-h" => return main_usage(),
            other => {
                eprintln!("未知参数：{other}（--help 看用法）");
                return ExitCode::from(2);
            }
        }
    }

    let Some(batch) = batch.filter(|value| !value.trim().is_empty()) else {
        eprintln!("缺少 --batch（--help 看用法）");
        return ExitCode::from(2);
    };

    let config = Config::from_env();
    let Some(database_url) = config.database_url.clone() else {
        eprintln!("缺少 DATABASE_URL：邀请码只落库，没有数据库这件事没有意义");
        return ExitCode::from(2);
    };

    let pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            // 连接串里可能有密码：只打错误本身，不打环境变量
            eprintln!("连接数据库失败：{error}");
            return ExitCode::from(1);
        }
    };
    let auth = AuthService::new(Some(pool));

    match command.as_str() {
        "generate" => {
            let Some(count) = count.filter(|value| *value > 0 && *value <= 10_000) else {
                eprintln!("--count 必须在 1..=10000 之间");
                return ExitCode::from(2);
            };
            let expires_at =
                expires_days.map(|days| OffsetDateTime::now_utc() + Duration::days(days));

            println!("批次：{batch}");
            println!(
                "每码可用次数：{max_uses}；过期：{}",
                match expires_days {
                    Some(days) => format!("{days} 天后"),
                    None => "永不过期".to_owned(),
                }
            );
            println!("—— 以下明文只显示这一次，请立刻分发给学生 ——");

            let mut issued = 0usize;
            for index in 0..count {
                let code = new_code();
                match auth
                    .insert_invite(
                        &code,
                        &batch,
                        &issued_by,
                        expires_at,
                        max_uses,
                        note.as_deref(),
                    )
                    .await
                {
                    Ok(()) => {
                        issued += 1;
                        println!("{code}");
                    }
                    Err(error) => {
                        eprintln!("第 {} 张生成失败：{error}", index + 1);
                        eprintln!("已成功 {issued} 张；剩余请修复后重跑（已发的不会重复）");
                        return ExitCode::from(1);
                    }
                }
            }

            println!("—— 共 {issued} 张（只存了哈希，明文不再可查）——");
            ExitCode::SUCCESS
        }
        "revoke" => match auth.revoke_invite_batch(&batch).await {
            Ok(revoked) => {
                println!("批次 {batch}：撤销 {revoked} 张（已核销的账号不受影响）");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("撤销失败：{error}");
                ExitCode::from(1)
            }
        },
        "stats" => match auth.invite_batch_stats(&batch).await {
            Ok((used, capacity, codes)) => {
                println!("批次 {batch}");
                println!("  张数     ：{codes}");
                println!("  已用/容量：{used}/{capacity}");
                println!("  剩余     ：{}", (capacity - used).max(0));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("查询失败：{error}");
                ExitCode::from(1)
            }
        },
        other => {
            eprintln!("未知子命令：{other}（--help 看用法）");
            ExitCode::from(2)
        }
    }
}
