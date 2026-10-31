//! 配置：全部来自环境变量，有安全默认值。

use std::env;

use yunong_shared::MAX_INPUT_TOKENS;

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
        }
    }

    /// 测试用：不连数据库、端口随机。
    pub fn for_tests() -> Self {
        Self {
            bind: "127.0.0.1:0".into(),
            database_url: None,
            db_max_connections: 1,
            concurrency_limit: 50,
            max_input_tokens: MAX_INPUT_TOKENS,
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
    }
}
