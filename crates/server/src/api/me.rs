//! 当前账号信息。
//!
//! 给客户端与集成测试一个"我的 token 到底属于谁"的确认点；
//! `device_id` 在 W2 恒为 `null`（注册/登录还没有设备维度）。

use axum::{extract::Extension, response::Response};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{envelope, request_id::ReqId};
use crate::auth::Principal;

#[derive(Debug, Serialize)]
struct MeBody {
    user_id: Uuid,
    device_id: Option<Uuid>,
}

/// `GET /api/v1/me`（需 Bearer）→ 200 `{user_id, device_id}`
pub async fn me(
    Extension(req_id): Extension<ReqId>,
    Extension(principal): Extension<Principal>,
) -> Response {
    envelope::ok(
        MeBody {
            user_id: principal.user_id,
            device_id: principal.device_id,
        },
        &req_id.0,
    )
}
