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

use crate::api::{client_ip::ClientIp, envelope, error::AppError, request_id::ReqId};
use crate::auth::{token, Principal};
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

/// 邮箱的基本形态校验（**在限流之前**）。
///
/// R2-F9：限流键与账号键必须用同一套归一化。账号侧是 `to_ascii_lowercase`（`users (lower(email))`
/// 索引是 ASCII 语义），所以这里也只做 ASCII 小写；顺带拒绝非 ASCII 邮箱，
/// 避免 `Ä@x.com` 与 `ä@x.com` 这类"看起来是同一个账号、实际是两个桶"的歧义。
fn check_email_shape(email: &str, request_id: &str) -> Result<(), AppError> {
    if email.len() > MAX_EMAIL_BYTES {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            format!("邮箱长度不能超过 {MAX_EMAIL_BYTES} 字节"),
            request_id,
        ));
    }
    if !email.is_ascii() {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            "邮箱只支持 ASCII 字符".to_owned(),
            request_id,
        ));
    }
    Ok(())
}

/// 登录限流的键：与账号键同口径（ASCII 小写、去首尾空白）。
fn email_rate_key(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

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
    Extension(client): Extension<ClientIp>,
    body: Bytes,
) -> Result<Response, AppError> {
    let credentials: CredentialsRequest = parse_body(&body, &req_id)?;
    // security 复核 F1：**先卡长度再进限流**（顺序不能反，否则长邮箱已经进了桶表）
    check_email_shape(&credentials.email, &req_id.0)?;
    // 红队 H1 + R2-F6：注册按**客户端 IP** 分桶（原来是全局桶：一个班 40 人同时注册会 30 个 429，
    // 而匿名者 10 次/分就能把全站注册掐死）。NAT 环境下同出口的多个人共用一个桶，
    // 这是有意的取舍：宁可让同一间学校的人互相挤一点，也不能让一个人掐死全站。
    state
        .limiter
        .check_rate(AuthRoute::Register, &format!("ip:{}", client.0))
        .map_err(|error| error.with_request_id(&req_id.0))?;
    let _hash_slot = state
        .limiter
        .acquire_hash_slot(AuthRoute::Register, HASH_SLOT_WAIT)
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
    check_email_shape(&credentials.email, &req_id.0)?;
    // 红队 H1 + R2-F9：按**ASCII 小写规范化邮箱**限速（与账号键同口径），再抢一个哈希名额
    state
        .limiter
        .check_rate(AuthRoute::Login, &email_rate_key(&credentials.email))
        .map_err(|error| error.with_request_id(&req_id.0))?;
    let _hash_slot = state
        .limiter
        .acquire_hash_slot(AuthRoute::Login, HASH_SLOT_WAIT)
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
    // 刷新不哈希（只查库），所以只过速率闸、不占哈希名额。
    //
    // R2-F6：按**令牌摘要**分桶（原来是全局桶：匿名者 60 次/分就能永久掐死全站刷新；
    // 而 1000 在线 / 15min TTL ≈ 67 refresh/min 本就超过 60 的全局上限）。
    // 用摘要而不是明文，是因为键会进内存表，不该常驻凭据明文。
    let refresh_key = format!("refresh:{}", &token::hash(&request.refresh_token)[..16]);
    state
        .limiter
        .check_rate(AuthRoute::Refresh, &refresh_key)
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
