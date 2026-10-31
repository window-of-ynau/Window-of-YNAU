//! 存活/就绪探针。这两个接口**不需要鉴权**（K8s / compose healthcheck 用）。

use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

use crate::state::AppState;

pub async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "data": {
            "status": "alive",
            "version": crate::version(),
            "uptime_secs": state.uptime_secs()
        }
    }))
}

pub async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let (ready, db) = match &state.db {
        None => (true, "not_configured"),
        Some(pool) => {
            let probe = tokio::time::timeout(
                Duration::from_millis(800),
                sqlx::query("select 1").execute(pool),
            )
            .await;
            match probe {
                Ok(Ok(_)) => (true, "ok"),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "就绪探针：数据库不可用");
                    (false, "error")
                }
                Err(_) => {
                    tracing::warn!("就绪探针：数据库超时");
                    (false, "timeout")
                }
            }
        }
    };

    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "ok": ready,
            "data": { "status": if ready { "ready" } else { "not_ready" }, "db": db }
        })),
    )
}
