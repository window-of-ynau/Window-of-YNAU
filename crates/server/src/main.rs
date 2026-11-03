//! 云农之窗服务端入口。

use std::net::SocketAddr;

use yunong_server::{app, config::Config, state::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // .env 只影响本地开发；生产用真实环境变量
    let _ = dotenvy::dotenv();

    yunong_server::init_tracing();

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

    // 启动自检（评审 F12）：漏跑迁移的话，抽样记账会静默失败、成本统计永远是 0，
    // 没人会发现。这里宁可**启动即失败**，把该执行哪个迁移文件说清楚。
    if let Some(pool) = &db {
        if let Err(message) = yunong_server::accounting::verify_schema(pool).await {
            tracing::error!("数据库结构检查失败：{message}");
            anyhow::bail!("{message}");
        }
        tracing::info!("数据库结构检查通过（usage_samples 已含 turn_id / estimated）");
    }

    let state = AppState::new(config.clone(), db);
    // 内存后端意味着重启即丢失账号与令牌：启动时必须让人看见。
    tracing::info!(auth_backend = state.auth.backend_kind(), "鉴权后端已就绪");
    let router = app(state.clone());

    // 抓取 worker：默认关闭（CI/离线环境不联网）。开启后按 6 小时间隔跑一轮，
    // 幂等（只处理 pending），失败有退避重试，状态落 rag_candidates。
    if config.rag_fetch_enabled {
        if let (Some(pool), Some(rag)) = (state.db.clone(), Some(state.rag.clone())) {
            let pipeline_config = yunong_server::rag_fetch::FetchConfig::from_config(&config);
            let timeout = std::time::Duration::from_secs(config.rag_fetch_timeout_secs);
            // M3/M4：DOI 重定向白名单与正文字节上限都来自配置
            match yunong_server::rag_fetch::HttpFetcher::new(
                timeout,
                config.rag_fetch_max_body_bytes,
                config.rag_fetch_doi_allowed_hosts.clone(),
            ) {
                Ok(fetcher) => {
                    let pipeline =
                        std::sync::Arc::new(yunong_server::rag_fetch::FetchPipeline::new(
                            pool,
                            rag,
                            std::sync::Arc::new(fetcher),
                            pipeline_config,
                        ));
                    tokio::spawn(async move {
                        let mut ticker =
                            tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
                        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        loop {
                            ticker.tick().await;
                            match pipeline.run_once().await {
                                Ok(report) => tracing::info!(
                                    summary = %yunong_server::rag_fetch::summarize(&report),
                                    "rag fetch 完成"
                                ),
                                Err(error) => tracing::warn!(?error, "rag fetch 失败"),
                            }
                        }
                    });
                    tracing::info!("RAG 抓取 worker 已启用（每 6 小时一轮）");
                }
                Err(error) => tracing::error!(%error, "构建抓取 HTTP client 失败，worker 未启动"),
            }
        } else {
            tracing::warn!("RAG_FETCH_ENABLED=1 但没有数据库，抓取 worker 不会启动");
        }
    }

    let addr: SocketAddr = config.bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "监听中");

    // 带上连接信息：注册限流按客户端 IP 分桶（R2-F6）依赖 `ConnectInfo`。
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
