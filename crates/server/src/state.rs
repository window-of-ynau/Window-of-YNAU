//! 应用状态。

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;

use crate::accounting::UsageRecorder;
use crate::auth::AuthService;
use crate::config::Config;
use crate::gate::TurnGate;
use crate::upstream::UpstreamClient;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// `None` 表示无数据库模式（本地骨架 / 测试）
    pub db: Option<PgPool>,
    /// 鉴权服务。`db` 为 `None` 时自动退化为内存后端，离线也能跑完整个注册/登录流程。
    pub auth: Arc<AuthService>,
    /// 并发闸门：按 **turn** 计数（全局 50 + 单用户 1），1.0 单实例进程内即可。
    pub gate: Arc<TurnGate>,
    /// 1% 抽样记账 + 审计。写库失败只记 warn，不影响对话流。
    pub accounting: Arc<UsageRecorder>,
    /// 上游客户端：复用一个 `reqwest::Client`（连接池 + TLS 会话复用）。
    pub upstream: Arc<UpstreamClient>,
    started_at: Instant,
}

impl AppState {
    pub fn new(config: Config, db: Option<PgPool>) -> Self {
        let auth = Arc::new(AuthService::new(db.clone()));
        let gate = Arc::new(TurnGate::new(
            config.concurrency_limit,
            Duration::from_millis(config.concurrency_queue_ms),
        ));
        let accounting = Arc::new(UsageRecorder::new(db.clone(), config.usage_sample_percent));
        let upstream = Arc::new(UpstreamClient::new(
            config.upstream.clone(),
            config.upstream_timeouts(),
        ));
        Self {
            config: Arc::new(config),
            db,
            auth,
            gate,
            accounting,
            upstream,
            started_at: Instant::now(),
        }
    }

    pub fn for_tests() -> Self {
        Self::new(Config::for_tests(), None)
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("db", &self.db.is_some())
            .field("auth_backend", &self.auth.backend_kind())
            .field("concurrency_limit", &self.gate.limit())
            .field("in_flight_turns", &self.gate.in_flight())
            .field("usage_sample_percent", &self.accounting.sample_percent())
            .field("uptime_secs", &self.uptime_secs())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_and_accounting_follow_config() {
        let mut config = Config::for_tests();
        config.concurrency_limit = 7;
        config.usage_sample_percent = 3;
        config.upstream_connect_timeout_secs = 3;
        let state = AppState::new(config, None);
        assert_eq!(state.gate.limit(), 7);
        assert_eq!(state.accounting.sample_percent(), 3);
        assert_eq!(state.gate.in_flight(), 0);
        // 上游客户端按配置的超时构建（方便测试里调小首事件超时）
        assert_eq!(state.upstream.timeouts().connect.as_secs(), 3);
        // 没配上游时，provider 查询是 None（chat 端点会回 502）
        assert!(state
            .upstream
            .provider(yunong_shared::Protocol::OpenaiCompletions)
            .is_none());
        // 无数据库模式：鉴权自动落到内存后端，本地骨架照样能跑
        assert_eq!(state.auth.backend_kind(), "memory");
    }
}
