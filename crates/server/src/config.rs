//! 配置：全部来自环境变量，有安全默认值。

use std::env;
use std::time::Duration;

use yunong_shared::MAX_INPUT_TOKENS;

use crate::upstream::{Timeouts, UpstreamConfig};

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
            auth_hash_concurrency: env_parse("AUTH_HASH_CONCURRENCY", 8),
            auth_login_per_min: env_parse("AUTH_LOGIN_PER_MIN", 20),
            auth_register_per_min: env_parse("AUTH_REGISTER_PER_MIN", 10),
            auth_refresh_per_min: env_parse("AUTH_REFRESH_PER_MIN", 60),
            body_limit_bytes: env_parse("YUNONG_BODY_LIMIT_BYTES", 1024 * 1024),
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

    #[test]
    fn defaults_match_the_design_doc() {
        // docs/02 §8.1：全局 50 并发、排队 ≤5s；docs/03 §10：15s 心跳
        let c = Config::from_env();
        assert!(c.concurrency_limit > 0);
        assert_eq!(c.sse_heartbeat_secs, 15);
        assert_eq!(c.usage_sample_percent, 1);
    }
}
