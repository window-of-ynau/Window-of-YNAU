//! 契约级集成测试：测的是**对外行为**，不是内部实现。
//!
//! 这里每一条都对应 `docs/03-接口文档.md` 里的约定；
//! 如果哪天接口变了而文档没变，这些测试会先炸。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use yunong_server::{app, state::AppState};

struct Res {
    status: StatusCode,
    body: Value,
    request_id: Option<String>,
}

async fn call(req: Request<Body>) -> Res {
    let res = app(AppState::for_tests())
        .oneshot(req)
        .await
        .expect("服务应能处理请求");
    let status = res.status();
    let request_id = res
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("读取响应体")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        body,
        request_id,
    }
}

fn post_chat(json: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/chat/stream")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json.to_owned()))
        .unwrap()
}

fn valid_json(round: u8, estimated: u32) -> String {
    serde_json::json!({
        "session_id": "s_1",
        "turn_id": "t_1",
        "round": round,
        "model": "deepseek-chat",
        "protocol": "openai_completions",
        "messages": [{ "role": "user", "content": "下周三下午有课吗" }],
        "tools": [],
        "max_output_tokens": 512,
        "stream": true,
        "estimated_input_tokens": estimated
    })
    .to_string()
}

#[tokio::test]
async fn healthz_is_public_and_healthy() {
    let res = call(
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["ok"], true);
    assert_eq!(res.body["data"]["status"], "alive");
    // 每个响应都必须带 request id（日志与错误信封靠它对齐）
    assert!(res.request_id.is_some(), "响应缺少 x-request-id");
}

#[tokio::test]
async fn readyz_ok_without_database() {
    let res = call(
        Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["data"]["db"], "not_configured");
}

#[tokio::test]
async fn malformed_json_uses_our_error_envelope() {
    let res = call(post_chat("{ not json")).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.body["ok"], false);
    assert_eq!(res.body["error"]["code"], "VALIDATION_ERROR");
    // 信封里的 request_id 必须与响应头一致
    assert_eq!(
        res.body["error"]["request_id"].as_str().unwrap(),
        res.request_id.as_deref().unwrap()
    );
}

#[tokio::test]
async fn round_out_of_range_is_rejected() {
    for round in [0_u8, 9] {
        let res = call(post_chat(&valid_json(round, 100))).await;
        assert_eq!(
            res.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "round={round}"
        );
        assert_eq!(res.body["error"]["code"], "VALIDATION_ERROR");
    }
}

#[tokio::test]
async fn oversized_input_maps_to_413() {
    let res = call(post_chat(&valid_json(1, 40_000))).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(res.body["error"]["code"], "MESSAGE_TOO_LARGE");
}

#[tokio::test]
async fn valid_request_reaches_the_not_implemented_gate() {
    // W1 的预期行为：契约通过 → 如实告诉调用方"还没实现"，而不是假装成功。
    let res = call(post_chat(&valid_json(1, 1200))).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(res.body["error"]["code"], "NOT_IMPLEMENTED");
}

#[tokio::test]
async fn caller_request_id_is_echoed() {
    let mut req = post_chat(&valid_json(1, 1200));
    req.headers_mut()
        .insert("x-request-id", "trace-from-client".parse().unwrap());
    let res = call(req).await;
    assert_eq!(res.request_id.as_deref(), Some("trace-from-client"));
    assert_eq!(res.body["error"]["request_id"], "trace-from-client");
}

#[tokio::test]
async fn unknown_route_is_404_not_200() {
    let res = call(
        Request::builder()
            .uri("/api/v1/nope")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}
