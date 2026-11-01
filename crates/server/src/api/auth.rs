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
use crate::limiter::AuthRoute;
use crate::state::AppState;

/// 抢哈希名额最多等多久：等不到就 429（宁可让客户端退避，也不要把内存堆起来）。
const HASH_SLOT_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

/// 邮箱最大字节数（RFC 5321 的路径上限 254）。
///
/// security 复核 F1：这个校验**必须早于限流**——限流桶的键就是这个邮箱，
/// 不先卡长度的话，1 MiB 的假邮箱会被原样放进桶表常驻（匿名即可撑爆内存）。
const MAX_EMAIL_BYTES: usize = 254;

/// 刷新令牌的最大字节数：`token::is_well_formed` 之前先挡明显伪造的超长串。
const MAX_REFRESH_TOKEN_BYTES: usize = 512;

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
    // security 复核 F1：**先卡长度再进限流**（顺序不能反，否则长邮箱已经进了桶表）
    if credentials.email.len() > MAX_EMAIL_BYTES {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            format!("邮箱长度不能超过 {MAX_EMAIL_BYTES} 字节"),
            &req_id.0,
        ));
    }
    // 红队 H1：未鉴权端点先过限流（速率闸 + 哈希并发闸），否则匿名并发能把内存打爆
    state
        .limiter
        .check_rate(AuthRoute::Register, "global")
        .map_err(|error| error.with_request_id(&req_id.0))?;
    let _hash_slot = state
        .limiter
        .acquire_hash_slot(HASH_SLOT_WAIT)
        .await
        .map_err(|error| error.with_request_id(&req_id.0))?;
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
    // security 复核 F1：同上——先卡长度，再进限流（这是内存放大器的入口）
    if credentials.email.len() > MAX_EMAIL_BYTES {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            format!("邮箱长度不能超过 {MAX_EMAIL_BYTES} 字节"),
            &req_id.0,
        ));
    }
    // 红队 H1：按**规范化邮箱**限速（同一账号被撞库时能立刻收口），再抢一个哈希名额
    state
        .limiter
        .check_rate(AuthRoute::Login, &credentials.email.trim().to_lowercase())
        .map_err(|error| error.with_request_id(&req_id.0))?;
    let _hash_slot = state
        .limiter
        .acquire_hash_slot(HASH_SLOT_WAIT)
        .await
        .map_err(|error| error.with_request_id(&req_id.0))?;
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
    // security 复核 F1：刷新键是固定串（不会撑桶表），但超长令牌没有意义，
    // 先挡掉避免把 1 MiB 的假令牌送进哈希与数据库绑定。
    if request.refresh_token.len() > MAX_REFRESH_TOKEN_BYTES {
        return Err(AppError::new(
            ErrorCode::Unauthorized,
            "刷新令牌格式不正确",
            &req_id.0,
        ));
    }
    // 刷新不哈希（只查库），所以只过速率闸、不占哈希名额
    state
        .limiter
        .check_rate(AuthRoute::Refresh, "global")
        .map_err(|error| error.with_request_id(&req_id.0))?;
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
