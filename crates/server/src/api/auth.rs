//! 账号鉴权端点：注册 / 登录 / 刷新 / 登出。
//!
//! 请求体手写解析（与 `chat` 一致）：axum 的 `Json` 拒绝时会返回它自己的
//! 422/400 响应体，不是我们的错误信封。

use axum::{
    body::Bytes,
    extract::{Extension, State},
    response::Response,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;
use yunong_shared::ErrorCode;

use crate::api::{envelope, error::AppError, request_id::ReqId};
use crate::auth::Principal;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct CredentialsRequest {
    email: String,
    password: String,
}

#[derive(Debug, Deserialize)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(Debug, Serialize)]
struct SessionBody {
    user_id: Uuid,
    access_token: String,
    expires_in: i64,
    refresh_token: String,
}

#[derive(Debug, Serialize)]
struct RefreshBody {
    access_token: String,
    expires_in: i64,
    refresh_token: String,
}

/// `POST /api/v1/auth/register` → 201 `{user_id, access_token, expires_in, refresh_token}`
pub async fn register(
    State(state): State<AppState>,
    Extension(req_id): Extension<ReqId>,
    body: Bytes,
) -> Result<Response, AppError> {
    let credentials: CredentialsRequest = parse_body(&body, &req_id)?;
    let session = state
        .auth
        .register(&credentials.email, &credentials.password)
        .await
        .map_err(|err| AppError::from_auth(err, &req_id.0))?;

    Ok(envelope::created(
        SessionBody {
            user_id: session.user_id,
            access_token: session.access_token,
            expires_in: session.expires_in,
            refresh_token: session.refresh_token,
        },
        &req_id.0,
    ))
}

/// `POST /api/v1/auth/login` → 200 同注册
pub async fn login(
    State(state): State<AppState>,
    Extension(req_id): Extension<ReqId>,
    body: Bytes,
) -> Result<Response, AppError> {
    let credentials: CredentialsRequest = parse_body(&body, &req_id)?;
    let session = state
        .auth
        .login(&credentials.email, &credentials.password)
        .await
        .map_err(|err| AppError::from_auth(err, &req_id.0))?;

    Ok(envelope::ok(
        SessionBody {
            user_id: session.user_id,
            access_token: session.access_token,
            expires_in: session.expires_in,
            refresh_token: session.refresh_token,
        },
        &req_id.0,
    ))
}

/// `POST /api/v1/auth/refresh` → 200 新的一对令牌（旧刷新令牌立刻失效）
pub async fn refresh(
    State(state): State<AppState>,
    Extension(req_id): Extension<ReqId>,
    body: Bytes,
) -> Result<Response, AppError> {
    let request: RefreshRequest = parse_body(&body, &req_id)?;
    let session = state
        .auth
        .refresh(&request.refresh_token)
        .await
        .map_err(|err| AppError::from_auth(err, &req_id.0))?;

    Ok(envelope::ok(
        RefreshBody {
            access_token: session.access_token,
            expires_in: session.expires_in,
            refresh_token: session.refresh_token,
        },
        &req_id.0,
    ))
}

/// `POST /api/v1/auth/logout`（需 Bearer）→ 200 `{"ok": true}`
pub async fn logout(
    State(state): State<AppState>,
    Extension(req_id): Extension<ReqId>,
    Extension(principal): Extension<Principal>,
) -> Result<Response, AppError> {
    state
        .auth
        .logout(&principal)
        .await
        .map_err(|err| AppError::from_auth(err, &req_id.0))?;

    Ok(envelope::ok(json!({ "ok": true }), &req_id.0))
}

/// 请求体解析失败 → 422（而不是 axum 默认的非信封响应）。
fn parse_body<T: serde::de::DeserializeOwned>(body: &[u8], req_id: &ReqId) -> Result<T, AppError> {
    serde_json::from_slice(body).map_err(|err| {
        AppError::new(
            ErrorCode::ValidationError,
            format!("请求体不合法：{err}"),
            &req_id.0,
        )
    })
}
