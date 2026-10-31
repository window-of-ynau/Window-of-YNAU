//! 契约级集成测试：测的是**对外行为**，不是内部实现。
//!
//! 这里每一条都对应 `docs/03-接口文档.md` / `docs/04-ADR.md`（ADR-11）里的约定；
//! 如果哪天接口变了而文档没变，这些测试会先炸。
//!
//! 全部用例都跑在**无数据库模式**下（`AppState::for_tests()` → 内存鉴权后端），
//! 所以不装 PostgreSQL 也能 `cargo test`。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use yunong_server::{app, state::AppState};

const EMAIL: &str = "student@ynau.edu.cn";
const PASSWORD: &str = "correct-horse-battery";

struct Res {
    status: StatusCode,
    body: Value,
    request_id: Option<String>,
}

/// 一个用例一个 `AppState`（每次构造都是干净的内存后端），用例之间不串数据。
struct TestApp {
    state: AppState,
}

impl TestApp {
    fn new() -> Self {
        Self {
            state: AppState::for_tests(),
        }
    }

    async fn call(&self, req: Request<Body>) -> Res {
        let res = app(self.state.clone())
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

    /// 注册一个账号并返回 `data`。
    async fn register(&self, email: &str, password: &str) -> Value {
        let res = self
            .call(json_request(
                "POST",
                "/api/v1/auth/register",
                Some(json!({ "email": email, "password": password })),
            ))
            .await;
        assert_eq!(
            res.status,
            StatusCode::CREATED,
            "注册应成功：{:?}",
            res.body
        );
        assert_eq!(res.body["ok"], true);
        res.body["data"].clone()
    }

    /// 注册并拿到访问令牌（保护类用例只需要这一个）。
    async fn access_token(&self, email: &str) -> String {
        let data = self.register(email, PASSWORD).await;
        data["access_token"]
            .as_str()
            .expect("access_token 必须是字符串")
            .to_owned()
    }
}

fn json_request(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    match body {
        Some(value) => builder
            .body(Body::from(value.to_string()))
            .expect("构造请求"),
        None => builder.body(Body::empty()).expect("构造请求"),
    }
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("构造请求")
}

fn with_bearer(mut req: Request<Body>, token: &str) -> Request<Body> {
    req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().expect("头值合法"),
    );
    req
}

fn with_request_id(mut req: Request<Body>, request_id: &str) -> Request<Body> {
    req.headers_mut().insert(
        "x-request-id",
        request_id.parse().expect("请求 ID 头值合法"),
    );
    req
}

fn chat_body(round: u8, estimated_input_tokens: u32) -> Value {
    json!({
        "session_id": "s_1",
        "turn_id": "t_1",
        "round": round,
        "model": "deepseek-chat",
        "protocol": "openai_completions",
        "messages": [{ "role": "user", "content": "下周三下午有课吗" }],
        "tools": [],
        "max_output_tokens": 512,
        "stream": true,
        "estimated_input_tokens": estimated_input_tokens
    })
}

/// 断言错误信封：状态码、错误码、以及与响应头一致的 `request_id`。
fn assert_error(res: &Res, status: StatusCode, code: &str) {
    assert_eq!(res.status, status, "响应体：{:?}", res.body);
    assert_eq!(res.body["ok"], false);
    assert_eq!(res.body["error"]["code"], code);
    let body_request_id = res.body["error"]["request_id"]
        .as_str()
        .unwrap_or_else(|| panic!("错误信封必须有 request_id：{:?}", res.body));
    assert_eq!(
        Some(body_request_id.to_owned()),
        res.request_id,
        "错误信封的 request_id 必须等于响应头 x-request-id"
    );
}

/// 43 号字符都是令牌：64 个小写 hex。
fn assert_token(value: &Value, field: &str) {
    let token = value
        .as_str()
        .unwrap_or_else(|| panic!("{field} 必须是字符串"));
    assert_eq!(token.len(), 64, "{field} 应是 64 位 hex：{token}");
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "{field} 应是小写 hex：{token}"
    );
}

// ============ 探针：保持公开 ============

#[tokio::test]
async fn healthz_is_public_and_healthy() {
    let app = TestApp::new();
    let res = app.call(get("/healthz")).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["ok"], true);
    assert_eq!(res.body["data"]["status"], "alive");
}

#[tokio::test]
async fn readyz_is_public_without_database() {
    let app = TestApp::new();
    let res = app.call(get("/readyz")).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["ok"], true);
    assert_eq!(res.body["data"]["db"], "not_configured");
}

// ============ 注册 ============

#[tokio::test]
async fn register_returns_tokens_and_user_id() {
    let app = TestApp::new();
    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/register",
            Some(json!({ "email": EMAIL, "password": PASSWORD })),
        ))
        .await;

    assert_eq!(res.status, StatusCode::CREATED);
    assert_eq!(res.body["ok"], true);
    assert_eq!(
        res.body["request_id"].as_str(),
        res.request_id.as_deref(),
        "成功信封也要带与响应头一致的 request_id"
    );

    let data = &res.body["data"];
    let user_id = data["user_id"].as_str().expect("user_id 必须是字符串");
    Uuid::parse_str(user_id).expect("user_id 必须是合法 UUID");
    assert_token(&data["access_token"], "access_token");
    assert_token(&data["refresh_token"], "refresh_token");
    assert_eq!(data["expires_in"].as_i64(), Some(15 * 60));
}

#[tokio::test]
async fn register_rejects_duplicate_email() {
    let app = TestApp::new();
    app.register(EMAIL, PASSWORD).await;

    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/register",
            Some(json!({ "email": EMAIL, "password": PASSWORD })),
        ))
        .await;
    assert_error(&res, StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR");
}

#[tokio::test]
async fn register_rejects_short_password() {
    let app = TestApp::new();
    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/register",
            Some(json!({ "email": EMAIL, "password": "1234567" })),
        ))
        .await;
    assert_error(&res, StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR");
}

#[tokio::test]
async fn register_rejects_invalid_email_and_malformed_body() {
    let app = TestApp::new();

    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/register",
            Some(json!({ "email": "no-at-sign", "password": PASSWORD })),
        ))
        .await;
    assert_error(&res, StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR");

    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/register",
            Some(json!({ "email": EMAIL })),
        ))
        .await;
    assert_error(&res, StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR");
}

#[tokio::test]
async fn caller_request_id_is_echoed_on_success() {
    let app = TestApp::new();
    let res = app
        .call(with_request_id(
            json_request(
                "POST",
                "/api/v1/auth/register",
                Some(json!({ "email": EMAIL, "password": PASSWORD })),
            ),
            "req_from_client_123",
        ))
        .await;

    assert_eq!(res.status, StatusCode::CREATED);
    assert_eq!(res.request_id.as_deref(), Some("req_from_client_123"));
    assert_eq!(res.body["request_id"], "req_from_client_123");
}

// ============ 登录 ============

#[tokio::test]
async fn login_returns_tokens_for_the_same_user() {
    let app = TestApp::new();
    let registered = app.register(EMAIL, PASSWORD).await;

    // 大小写不同也应能登录（服务端按小写规范化）
    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/login",
            Some(json!({ "email": "STUDENT@ynau.edu.cn", "password": PASSWORD })),
        ))
        .await;

    assert_eq!(res.status, StatusCode::OK, "响应体：{:?}", res.body);
    assert_eq!(res.body["ok"], true);
    let data = &res.body["data"];
    assert_eq!(data["user_id"], registered["user_id"]);
    assert_token(&data["access_token"], "access_token");
    assert_token(&data["refresh_token"], "refresh_token");
    assert_eq!(data["expires_in"].as_i64(), Some(15 * 60));
}

#[tokio::test]
async fn login_failures_are_indistinguishable() {
    let app = TestApp::new();
    app.register(EMAIL, PASSWORD).await;

    let wrong_password = app
        .call(json_request(
            "POST",
            "/api/v1/auth/login",
            Some(json!({ "email": EMAIL, "password": "wrong-password" })),
        ))
        .await;
    let unknown_email = app
        .call(json_request(
            "POST",
            "/api/v1/auth/login",
            Some(json!({ "email": "nobody@ynau.edu.cn", "password": PASSWORD })),
        ))
        .await;

    assert_error(&wrong_password, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    assert_error(&unknown_email, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    assert_eq!(
        wrong_password.body["error"]["message"], unknown_email.body["error"]["message"],
        "两种失败必须给同一句话，避免账号枚举"
    );

    // 自定义请求 ID 时，错误信封与响应头仍然一致
    let res = app
        .call(with_request_id(
            json_request(
                "POST",
                "/api/v1/auth/login",
                Some(json!({ "email": EMAIL, "password": "wrong-password" })),
            ),
            "req_login_fail",
        ))
        .await;
    assert_error(&res, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    assert_eq!(res.request_id.as_deref(), Some("req_login_fail"));
}

// ============ 刷新（轮换） ============

#[tokio::test]
async fn refresh_rotates_tokens_and_kills_the_old_one() {
    let app = TestApp::new();
    let registered = app.register(EMAIL, PASSWORD).await;
    let old_refresh = registered["refresh_token"]
        .as_str()
        .expect("refresh_token")
        .to_owned();

    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/refresh",
            Some(json!({ "refresh_token": old_refresh })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "响应体：{:?}", res.body);
    let rotated = res.body["data"].clone();
    assert_token(&rotated["access_token"], "access_token");
    assert_token(&rotated["refresh_token"], "refresh_token");
    assert_ne!(rotated["refresh_token"], registered["refresh_token"]);
    assert_ne!(rotated["access_token"], registered["access_token"]);

    // 新访问令牌可用
    let new_access = rotated["access_token"].as_str().expect("access_token");
    let me = app.call(with_bearer(get("/api/v1/me"), new_access)).await;
    assert_eq!(me.status, StatusCode::OK, "响应体：{:?}", me.body);
    assert_eq!(me.body["data"]["user_id"], registered["user_id"]);

    // 旧刷新令牌必须失效（轮换）
    let replay = app
        .call(json_request(
            "POST",
            "/api/v1/auth/refresh",
            Some(json!({ "refresh_token": old_refresh })),
        ))
        .await;
    assert_error(&replay, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
}

#[tokio::test]
async fn refresh_with_unknown_or_malformed_token_is_401() {
    let app = TestApp::new();

    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/refresh",
            Some(json!({ "refresh_token": "0".repeat(64) })),
        ))
        .await;
    assert_error(&res, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/refresh",
            Some(json!({ "refresh_token": "not-a-token" })),
        ))
        .await;
    assert_error(&res, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
}

// ============ 登出 ============

#[tokio::test]
async fn logout_revokes_access_and_refresh_tokens() {
    let app = TestApp::new();
    let registered = app.register(EMAIL, PASSWORD).await;
    let access = registered["access_token"]
        .as_str()
        .expect("access_token")
        .to_owned();
    let refresh = registered["refresh_token"]
        .as_str()
        .expect("refresh_token")
        .to_owned();

    let res = app
        .call(with_bearer(
            json_request("POST", "/api/v1/auth/logout", None),
            &access,
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "响应体：{:?}", res.body);
    assert_eq!(res.body["ok"], true);
    assert_eq!(res.body["data"]["ok"], true);

    // 访问令牌立刻失效
    let me = app.call(with_bearer(get("/api/v1/me"), &access)).await;
    assert_error(&me, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    // 刷新令牌一并吊销
    let res = app
        .call(json_request(
            "POST",
            "/api/v1/auth/refresh",
            Some(json!({ "refresh_token": refresh })),
        ))
        .await;
    assert_error(&res, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
}

// ============ /me 与保护范围 ============

#[tokio::test]
async fn me_returns_user_id_and_device() {
    let app = TestApp::new();
    let registered = app.register(EMAIL, PASSWORD).await;
    let access = registered["access_token"].as_str().expect("access_token");

    let res = app.call(with_bearer(get("/api/v1/me"), access)).await;
    assert_eq!(res.status, StatusCode::OK, "响应体：{:?}", res.body);
    assert_eq!(res.body["ok"], true);
    assert_eq!(res.body["data"]["user_id"], registered["user_id"]);
    assert!(
        res.body["data"]["device_id"].is_null(),
        "W2 还没有设备维度：device_id 应为 null"
    );
    assert_eq!(res.body["request_id"].as_str(), res.request_id.as_deref());
}

#[tokio::test]
async fn me_requires_bearer_token() {
    let app = TestApp::new();

    let res = app.call(get("/api/v1/me")).await;
    assert_error(&res, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    // 前缀不对
    let mut req = get("/api/v1/me");
    req.headers_mut().insert(
        header::AUTHORIZATION,
        "Basic dXNlcjpwYXNz".parse().expect("头值合法"),
    );
    let res = app.call(req).await;
    assert_error(&res, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");

    // 格式对但不存在
    let res = app
        .call(with_bearer(get("/api/v1/me"), &"0".repeat(64)))
        .await;
    assert_error(&res, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
}

// ============ chat：鉴权通过后仍返回 501 ============

#[tokio::test]
async fn chat_without_token_is_401_not_500() {
    let app = TestApp::new();
    let res = app
        .call(json_request(
            "POST",
            "/api/v1/chat/stream",
            Some(chat_body(1, 100)),
        ))
        .await;
    assert_error(&res, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
}

#[tokio::test]
async fn chat_with_valid_token_reaches_the_not_implemented_gate() {
    let app = TestApp::new();
    let access = app.access_token(EMAIL).await;

    let res = app
        .call(with_bearer(
            json_request("POST", "/api/v1/chat/stream", Some(chat_body(1, 100))),
            &access,
        ))
        .await;
    assert_error(&res, StatusCode::NOT_IMPLEMENTED, "NOT_IMPLEMENTED");
}

#[tokio::test]
async fn chat_malformed_json_uses_our_error_envelope() {
    let app = TestApp::new();
    let access = app.access_token(EMAIL).await;

    let res = app
        .call(with_bearer(
            json_request("POST", "/api/v1/chat/stream", None),
            &access,
        ))
        .await;
    assert_error(&res, StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR");
}

#[tokio::test]
async fn chat_round_out_of_range_is_rejected() {
    let app = TestApp::new();
    let access = app.access_token(EMAIL).await;

    let res = app
        .call(with_bearer(
            json_request(
                "POST",
                "/api/v1/chat/stream",
                Some(chat_body(yunong_shared::MAX_ROUNDS + 1, 100)),
            ),
            &access,
        ))
        .await;
    assert_error(&res, StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR");
}

#[tokio::test]
async fn chat_oversized_input_maps_to_413() {
    let app = TestApp::new();
    let access = app.access_token(EMAIL).await;

    let res = app
        .call(with_bearer(
            json_request(
                "POST",
                "/api/v1/chat/stream",
                Some(chat_body(1, yunong_shared::MAX_INPUT_TOKENS + 1)),
            ),
            &access,
        ))
        .await;
    assert_error(&res, StatusCode::PAYLOAD_TOO_LARGE, "MESSAGE_TOO_LARGE");
}

// ============ 其它 ============

#[tokio::test]
async fn unknown_route_is_404_not_401() {
    let res = TestApp::new().call(get("/api/v1/nope")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}
