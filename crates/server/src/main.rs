//! 云农之窗服务端入口。

use std::net::SocketAddr;

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use yunong_server::{app, config::Config, state::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // .env 只影响本地开发；生产用真实环境变量
    let _ = dotenvy::dotenv();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env();
    tracing::info!(
        version = yunong_server::version(),
        bind = %config.bind,
        concurrency_limit = config.concurrency_limit,
        max_input_tokens = config.max_input_tokens,
        "yunong-server 启动中"
    );

    let db = match &config.database_url {
        None => {
            tracing::warn!("DATABASE_URL 未设置：以「无数据库模式」启动（只有探针与骨架接口可用）");
            None
        }
        Some(url) => {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(config.db_max_connections)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect(url)
                .await;
            match pool {
                Ok(pool) => {
                    tracing::info!("已连接 PostgreSQL");
                    Some(pool)
                }
                Err(e) => {
                    // 不直接退出：让学生本地没有数据库时也能起来看页面
                    tracing::error!(error = %e, "数据库连接失败，降级为无数据库模式");
                    None
                }
            }
        }
    };

    let state = AppState::new(config.clone(), db);
    // 内存后端意味着重启即丢失账号与令牌：启动时必须让人看见。
    tracing::info!(auth_backend = state.auth.backend_kind(), "鉴权后端已就绪");
    let router = app(state);

    let addr: SocketAddr = config.bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "监听中");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("已退出");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("注册 Ctrl+C 处理器失败");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("注册 SIGTERM 失败")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("收到关闭信号，正在优雅退出");
}
