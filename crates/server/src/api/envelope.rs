//! 成功信封：`{"ok": true, "data": {...}, "request_id": "req_..."}`。
//!
//! 与 `shared::ErrorEnvelope` 对称：客户端只看 `ok` 就能分支，
//! `request_id` 始终等于响应头 `x-request-id`，便于对日志。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// 统一成功响应体。
#[derive(Debug, Clone, Serialize)]
pub struct SuccessEnvelope<T> {
    pub ok: bool,
    pub data: T,
    pub request_id: String,
}

/// 200 + 成功信封。
pub fn ok<T: Serialize>(data: T, request_id: &str) -> Response {
    respond(StatusCode::OK, data, request_id)
}

/// 201 + 成功信封（注册用）。
pub fn created<T: Serialize>(data: T, request_id: &str) -> Response {
    respond(StatusCode::CREATED, data, request_id)
}

fn respond<T: Serialize>(status: StatusCode, data: T, request_id: &str) -> Response {
    (
        status,
        Json(SuccessEnvelope {
            ok: true,
            data,
            request_id: request_id.to_owned(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shape_is_stable() {
        let envelope = SuccessEnvelope {
            ok: true,
            data: serde_json::json!({ "ok": true }),
            request_id: "req_1".to_owned(),
        };
        let value = serde_json::to_value(&envelope).expect("信封应能序列化");
        assert_eq!(value["ok"], true);
        assert_eq!(value["data"]["ok"], true);
        assert_eq!(value["request_id"], "req_1");
    }

    #[test]
    fn status_codes_are_as_documented() {
        let body = |_: ()| serde_json::json!({});
        assert_eq!(ok(body(()), "req_1").status(), StatusCode::OK);
        assert_eq!(created(body(()), "req_1").status(), StatusCode::CREATED);
    }
}
