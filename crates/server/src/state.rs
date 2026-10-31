//! 应用状态。

use std::sync::Arc;
use std::time::Instant;

use sqlx::PgPool;

use crate::auth::AuthService;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// `None` 表示无数据库模式（本地骨架 / 测试）
    pub db: Option<PgPool>,
    /// 鉴权服务。`db` 为 `None` 时自动退化为内存后端，离线也能跑完整个注册/登录流程。
    pub auth: Arc<AuthService>,
    started_at: Instant,
}

impl AppState {
    pub fn new(config: Config, db: Option<PgPool>) -> Self {
        let auth = Arc::new(AuthService::new(db.clone()));
        Self {
            config: Arc::new(config),
            db,
            auth,
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
            .field("uptime_secs", &self.uptime_secs())
            .finish()
    }
}
