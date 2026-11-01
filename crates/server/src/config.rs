//! 配置：全部来自环境变量，有安全默认值。

use std::env;
use std::time::Duration;

use yunong_shared::MAX_INPUT_TOKENS;

use crate::upstream::{Timeouts, UpstreamConfig};

/// 哈希并发闸的允许上限（security 报告加固建议 1）。每次 Argon2 占 19 MiB，
/// 32 → 峰值约 608 MiB；再大就接近"匿名并发把进程打爆"的原始问题了。
pub const MAX_AUTH_HASH_CONCURRENCY: usize = 32;

/// 服务端运行期配置。所有字段都可用环境变量覆盖（见 [`Config::from_env`]）。
#[derive(Debug, Clone)]
pub struct Config {
    /// 监听地址
    pub bind: String,
    /// 未设置时以"无数据库模式"启动（方便本地起骨架）
    pub database_url: Option<String>,
    pub db_max_connections: u32,
    /// 全局并发闸门（ADR-12：1.0 单实例、进程内）
    pub concurrency_limit: usize,
    /// 单轮输入 token 上限（保护毛利）
    pub max_input_tokens: u32,

    // ---- W2b：SSE 代理 ----
    /// 全局闸门排队上限（毫秒）。`docs/02` §8.1：排队 ≤ 5s 后 429；0 = 不排队、直接拒
    pub concurrency_queue_ms: u64,
    /// 连上游超时（秒）：DNS + TCP + TLS 握手
    pub upstream_connect_timeout_secs: u64,
    /// 首事件超时（秒）：响应头 + 第一个 token；推理模型常要 30~60s，超时 → 504
    pub upstream_first_event_timeout_secs: u64,
    /// 事件之间允许的最长空转（秒）；**不设总时长上限**，长回答本来就慢
    pub upstream_idle_timeout_secs: u64,
    /// SSE 心跳间隔（秒），0 = 不心跳。`docs/03` §10 要求 15s
    pub sse_heartbeat_secs: u64,
    /// usage 抽样百分比（0..=100），默认 1 = 1%（ADR-03：抽样成本监控，不是账单）
    pub usage_sample_percent: u8,
    /// 未鉴权端点的**哈希并发**上限（红队 H1）：Argon2 一次 19 MiB，
    /// 按次数限流挡不住并发内存峰值，所以这里按"同时在算的哈希数"收口。
    pub auth_hash_concurrency: usize,
    /// 登录限流（次/分钟/邮箱）。0 = 不限次数（并发闸仍然生效）
    pub auth_login_per_min: u32,
    /// 注册限流（次/分钟，全局）
    pub auth_register_per_min: u32,
    /// 刷新限流（次/分钟，全局）
    pub auth_refresh_per_min: u32,
    /// 请求体字节上限（红队 H2）：`MAX_INPUT_TOKENS` 之外的第二道闸，
    /// 防止"正文塞满 + estimated_input_tokens 填 1"绕过准入。
    pub body_limit_bytes: usize,
    /// 抽样判定用的**服务端盐**（红队 M3）：客户端拿不到它，所以无法预先算出
    /// 自己的 turn 会不会被抽中——否则"逐个试 turn_id"就能规避成本监控。
    pub sample_salt: String,
    /// 单条 SSE 流的**总时长**上限（秒，红队 M5）；0 = 不限。
    ///
    /// 分段超时挡不住"缓慢但一直有 token"的流，那会长期占住闸门名额。
    pub stream_total_timeout_secs: u64,
    /// `max_output_tokens` 的服务端上限（红队 M5）：客户端不得自行放大。
    pub max_output_tokens_cap: u32,
    /// `/api/v1/auth/**` 的请求体上限（R2-F5）。登录/注册/刷新只有几个字段，
    /// 给 8 KiB 就够；给 1 MiB 只会让匿名请求白白缓冲内存。
    pub auth_body_limit_bytes: usize,
    /// 是否信任 `X-Forwarded-For`（R2-F6）。默认 **false**：没有反向代理时，
    /// XFF 是攻击者随便填的，按它分桶等于没有限流。部署在 Caddy/Nginx 后面再打开。
    pub trust_proxy: bool,
    /// 套餐通道的上游供应商（三家协议可各配一套；Key 只存在服务端）
    pub upstream: UpstreamConfig,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bind: env_or("YUNONG_BIND", "127.0.0.1:8080"),
            database_url: env::var("DATABASE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            db_max_connections: env_parse("DB_MAX_CONNECTIONS", 10),
            concurrency_limit: env_parse("CONCURRENCY_LIMIT", 50),
            max_input_tokens: env_parse("MAX_INPUT_TOKENS", MAX_INPUT_TOKENS),
            concurrency_queue_ms: env_parse("CONCURRENCY_QUEUE_MS", 5_000),
            upstream_connect_timeout_secs: env_parse("UPSTREAM_CONNECT_TIMEOUT_SECS", 10),
            upstream_first_event_timeout_secs: env_parse("UPSTREAM_FIRST_EVENT_TIMEOUT_SECS", 90),
            upstream_idle_timeout_secs: env_parse("UPSTREAM_IDLE_TIMEOUT_SECS", 30),
            sse_heartbeat_secs: env_parse("SSE_HEARTBEAT_SECS", 15),
            usage_sample_percent: env_parse("USAGE_SAMPLE_PERCENT", 1).min(100),
            // security 报告加固建议 1：并发闸配成 64 等于重开 H1（64×19MiB ≈ 1.2 GiB）。
            // 这里**硬性夹到 1..=32**，并在越界时打 warn，免得配置错误静默变成漏洞。
            auth_hash_concurrency: {
                let configured: usize = env_parse("AUTH_HASH_CONCURRENCY", 8);
                let clamped = clamp_auth_hash_concurrency(configured);
                if configured != clamped {
                    tracing::warn!(
                        configured,
                        clamped,
                        "AUTH_HASH_CONCURRENCY 越界（允许 1..={MAX_AUTH_HASH_CONCURRENCY}），已夹紧；\
                         配得过大等于重开内存放大（每次哈希 19 MiB）"
                    );
                }
                clamped
            },
            auth_login_per_min: env_parse("AUTH_LOGIN_PER_MIN", 20),
            auth_register_per_min: env_parse("AUTH_REGISTER_PER_MIN", 10),
            auth_refresh_per_min: env_parse("AUTH_REFRESH_PER_MIN", 60),
            body_limit_bytes: env_parse("YUNONG_BODY_LIMIT_BYTES", 1024 * 1024),
            auth_body_limit_bytes: env_parse("AUTH_BODY_LIMIT_BYTES", 8 * 1024),
            trust_proxy: env::var("YUNONG_TRUST_PROXY")
                .ok()
                .map(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes"
                    )
                })
                .unwrap_or(false),
            sample_salt: sample_salt_from_env(),
            stream_total_timeout_secs: env_parse("STREAM_TOTAL_TIMEOUT_SECS", 600),
            max_output_tokens_cap: env_parse("MAX_OUTPUT_TOKENS_CAP", 8_192),
            upstream: UpstreamConfig::from_env(),
        }
    }

    /// 测试用：不连数据库、端口随机、不排队、不心跳（心跳会让断言难写）。
    ///
    /// 上游默认是空的——集成测试自己起 mock 上游，再填 [`Config::upstream`]。
    pub fn for_tests() -> Self {
        Self {
            bind: "127.0.0.1:0".into(),
            database_url: None,
            db_max_connections: 1,
            concurrency_limit: 50,
            max_input_tokens: MAX_INPUT_TOKENS,
            concurrency_queue_ms: 0,
            upstream_connect_timeout_secs: 2,
            upstream_first_event_timeout_secs: 5,
            upstream_idle_timeout_secs: 5,
            sse_heartbeat_secs: 0,
            usage_sample_percent: 1,
            // 测试默认值：限流要"够用但不碍事"——单测自己会显式调小来断言拒绝逻辑
            auth_hash_concurrency: 8,
            auth_login_per_min: 1000,
            auth_register_per_min: 1000,
            auth_refresh_per_min: 1000,
            body_limit_bytes: 1024 * 1024,
            auth_body_limit_bytes: 8 * 1024,
            trust_proxy: false,
            // 测试用固定盐：抽样判定在用例之间必须可复现。
            // 这里刻意用"64 字符小写十六进制"——它必须能通过 `validate_sample_salt`，
            // 否则将来有人把校验接到 for_tests 上会整片测试挂掉。
            sample_salt: "9f2c7b1e4d6a8035c1e9f7b2d4a6c8e05b3d1f7a9c2e4b6d8f0a1c3e5b7d9f2c".into(),
            // 测试默认不限总时长（长流用例要稳定）；M5 的用例会显式设小
            stream_total_timeout_secs: 0,
            max_output_tokens_cap: 8_192,
            upstream: UpstreamConfig::default(),
        }
    }

    /// 上游分段超时（语义见 [`Timeouts`]）。
    pub fn upstream_timeouts(&self) -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(self.upstream_connect_timeout_secs),
            first_event: Duration::from_secs(self.upstream_first_event_timeout_secs),
            idle: Duration::from_secs(self.upstream_idle_timeout_secs),
        }
    }
}

/// 抽样盐的最小长度（R3-F2：16 太松，提到 32 字节）。
pub const MIN_SAMPLE_SALT_BYTES: usize = 32;

/// `.env.example` 里出现过的示例值前缀：一旦被原样使用，等于**没加盐**（R3-F1）。
const SAMPLE_SALT_PLACEHOLDER: &str = "CHANGE_ME";

/// 读抽样盐（R3-F1 / R3-F2）。
///
/// - **留空**：启动时随机生成并 `warn`（重启后抽样判定会变；历史行不受影响、不回填）；
/// - **配置了但不合规**（示例值 / 太短 / 低熵）：**启动失败**。
///   理由：盐是成本监控的信任根，配错是**静默**失效——`warn` 会被忽略，
///   security 已实测用它可以把"不被抽中"做成 100% 规避。宁可起不来。
fn sample_salt_from_env() -> String {
    let raw = env::var("YUNONG_SAMPLE_SALT").unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        let generated = uuid::Uuid::new_v4().simple().to_string();
        tracing::warn!(
            "YUNONG_SAMPLE_SALT 未设置：本次启动用随机盐；重启后抽样判定会变\
             （历史行不受影响，也不回填）。生产请固定 ≥{MIN_SAMPLE_SALT_BYTES} 字节的随机值\
             （例如 `openssl rand -hex 32`）。"
        );
        return generated;
    }
    match validate_sample_salt(trimmed) {
        Ok(()) => trimmed.to_owned(),
        Err(reason) => panic!(
            "YUNONG_SAMPLE_SALT 不合规（{reason}）：抽样判定必须不可预测，否则成本监控可被规避。\
             请填 ≥{MIN_SAMPLE_SALT_BYTES} 字节的随机值（`openssl rand -hex 32`），或留空使用随机盐。"
        ),
    }
}

/// 盐的强度校验（R3-F2）。`Ok(())` 表示可用，`Err(原因)` 用于启动失败与测试。
///
/// 只做"明显低熵"的启发式，不过度设计：
/// 示例值 / 太短 / 纯数字 / 字符种类过少 / 明显的递增序列 / 短的小写十六进制串。
pub fn validate_sample_salt(salt: &str) -> Result<(), String> {
    if salt.trim().is_empty() {
        return Err("盐是全空白".to_owned());
    }
    if salt.contains(SAMPLE_SALT_PLACEHOLDER) {
        return Err("这是 .env.example 里的示例值，等于没加盐".to_owned());
    }
    if salt.len() < MIN_SAMPLE_SALT_BYTES {
        return Err(format!(
            "长度 {} 字节，少于 {MIN_SAMPLE_SALT_BYTES}",
            salt.len()
        ));
    }
    if salt.chars().all(|c| c.is_ascii_digit()) {
        return Err("纯数字（可预测）".to_owned());
    }
    let mut distinct: Vec<char> = salt.chars().collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() <= 4 {
        return Err(format!("字符种类只有 {} 种（可预测）", distinct.len()));
    }
    // 明显的递增序列：0123456789 / abcdefghijklmnopqrstuvwxyz 之类出现在盐里
    const OBVIOUS_RUNS: [&str; 4] = ["0123456789", "abcdefghij", "ABCDEFGHIJ", "qwertyuiop"];
    if OBVIOUS_RUNS.iter().any(|run| salt.contains(run)) {
        return Err("包含明显的递增/键盘序列".to_owned());
    }
    // 短的小写十六进制串（例如 32 位的 0123456789abcdef...）：熵不够。
    // 注意 `openssl rand -hex 32` 是 **64** 字符，能通过。
    let is_lower_hex = salt
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if is_lower_hex && salt.len() < 64 {
        return Err("小写十六进制串不足 64 字符（建议 `openssl rand -hex 32`）".to_owned());
    }
    Ok(())
}

/// 把 `AUTH_HASH_CONCURRENCY` 夹到允许区间（security 报告加固建议 1）。
///
/// 单独抽出来是为了可测：配置错误必须能被回归抓住，而不是等上线后被放大。
pub fn clamp_auth_hash_concurrency(configured: usize) -> usize {
    configured.clamp(1, MAX_AUTH_HASH_CONCURRENCY)
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_is_sane() {
        let c = Config::for_tests();
        assert!(c.database_url.is_none());
        assert_eq!(c.concurrency_limit, 50);
        assert!(c.max_input_tokens > 0);
        // 测试环境不排队、不心跳：断言里不能依赖"等 5 秒"这种巧合
        assert_eq!(c.concurrency_queue_ms, 0);
        assert_eq!(c.sse_heartbeat_secs, 0);
        assert!(c.usage_sample_percent <= 100);
        assert!(c.upstream.is_empty(), "测试默认不配上游");
    }

    #[test]
    fn upstream_timeouts_are_staged() {
        // 分段超时：首事件要能容忍推理模型的冷启动，总时长不设上限
        let timeouts = Config::from_env().upstream_timeouts();
        assert_eq!(timeouts.connect, Duration::from_secs(10));
        assert_eq!(timeouts.first_event, Duration::from_secs(90));
        assert_eq!(timeouts.idle, Duration::from_secs(30));
        assert!(
            timeouts.first_event > timeouts.idle,
            "首事件预算必须比空闲预算宽"
        );
    }

    /// R3-F1：`.env.example` 里的示例值一旦被原样使用，等于**没加盐**
    /// （security 实测：可 100% 规避抽样）。这种配置错误必须**启动失败**，不能只 warn。
    #[test]
    fn sample_salt_rejects_example_placeholder() {
        assert!(validate_sample_salt("CHANGE_ME_random_hex_32").is_err());
        assert!(validate_sample_salt("CHANGE_ME").is_err());
        assert!(
            validate_sample_salt("prefix-CHANGE_ME-suffix-0123456789abcdef").is_err(),
            "嵌在中间的示例值也要挡住"
        );
    }

    /// R3-F2：强度校验——太短、全空白、纯数字、字符种类太少、明显序列都要拒。
    #[test]
    fn sample_salt_rejects_weak_values() {
        for weak in [
            "",
            "    ",
            "short",
            "0123456789abcdef",
            "0123456789abcdef0123456789abcdef",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "1234567890123456789012345678901234567890",
            "qwertyuiopasdfghjklzxcvbnm123456",
        ] {
            assert!(
                validate_sample_salt(weak).is_err(),
                "弱盐应被拒绝：{weak:?}"
            );
        }
    }

    /// 合法的随机盐必须能通过（含 `openssl rand -hex 32` 的形态）。
    #[test]
    fn sample_salt_accepts_strong_values() {
        for strong in [
            "9f2c7b1e4d6a8035c1e9f7b2d4a6c8e05b3d1f7a9c2e4b6d8f0a1c3e5b7d9f2c",
            "kR7#pQ2$vN9@wT4&zX6!bM1^cL8*hJ3%fD5(sG0)aE2-uY4+tI6=oP8]rW1[",
        ] {
            assert!(
                validate_sample_salt(strong).is_ok(),
                "强盐应通过：{strong:?}"
            );
        }
    }

    /// 测试默认配置里的盐必须能过校验（否则将来把校验接到 `for_tests` 会整片挂）。
    #[test]
    fn test_config_salt_is_valid() {
        let config = Config::for_tests();
        assert!(validate_sample_salt(&config.sample_salt).is_ok());
        assert!(config.sample_salt.len() >= MIN_SAMPLE_SALT_BYTES);
    }

    /// 加固建议 1（t28）：并发闸配成 64 等于重开内存放大（64×19MiB ≈ 1.2 GiB）。
    #[test]
    fn auth_hash_concurrency_is_clamped() {
        assert_eq!(clamp_auth_hash_concurrency(8), 8);
        assert_eq!(clamp_auth_hash_concurrency(0), 1, "0 会让登录永久不可用");
        assert_eq!(
            clamp_auth_hash_concurrency(64),
            MAX_AUTH_HASH_CONCURRENCY,
            "超上限必须夹紧，否则一个配置错误就重开 H1"
        );
        assert_eq!(MAX_AUTH_HASH_CONCURRENCY, 32);
    }

    /// R2-F5：鉴权端点的 body 上限必须**远小于**业务接口（8 KiB vs 1 MiB）。
    #[test]
    fn auth_body_limit_is_smaller_than_api_limit() {
        let config = Config::for_tests();
        assert!(config.auth_body_limit_bytes < config.body_limit_bytes);
        assert_eq!(config.auth_body_limit_bytes, 8 * 1024);
        assert!(!config.trust_proxy, "默认不信任 XFF");
    }

    #[test]
    fn defaults_match_the_design_doc() {
        // docs/02 §8.1：全局 50 并发、排队 ≤5s；docs/03 §10：15s 心跳
        let c = Config::from_env();
        assert!(c.concurrency_limit > 0);
        assert_eq!(c.sse_heartbeat_secs, 15);
        assert_eq!(c.usage_sample_percent, 1);
    }
}
