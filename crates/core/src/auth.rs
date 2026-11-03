//! 套餐通道的"用存储里的 refresh 换新 access"（FFI：`yunong_auth_refresh_from_store`）。
//!
//! # 为什么要有这个模块
//!
//! `yunong_get_secret` 按设计**只返回 `{set, masked}`**（凭据明文永不出 Rust）。
//! 于是 App 重启后 Dart 手里没有 refresh 明文，自己刷新不了——每次启动都要重新登录。
//! 这个模块把"读 refresh → 调刷新接口 → 只把新 access 交给 Dart"整段留在 Rust 里。
//!
//! # 失败分级（这是本模块最要紧的语义）
//!
//! | 情况 | 本地令牌 | 返回码 | 理由 |
//! |---|---|---|---|
//! | 401 / 403（refresh 被拒） | **删除** access + refresh | `UNAUTHORIZED` | 令牌确实失效了，留着只会反复失败 |
//! | 网络不可达 / 超时 / 5xx | **原样保留** | `NETWORK` | 断一次网就把用户登出是最糟的体验 |
//! | 429 / 其它非 2xx | **原样保留** | `NETWORK` | 限流不是"登录失效"，重试即可 |
//! | 存储里没有 refresh | 不动 | `UNAUTHORIZED` | 本来就没登录过（或降级存储重启后丢了） |
//!
//! # 令牌 key 名
//!
//! 与 Dart 侧（`app/lib/services/auth_service.dart:78-79`）**完全一致**：
//! [`ACCESS_KEY`] = `auth_access_token`、[`REFRESH_KEY`] = `auth_refresh_token`。
//! 改名等于让老用户的令牌失联，不要动。
//!
//! # 轮换
//!
//! 服务端刷新时**旧 refresh 立刻失效**（`crates/server/src/api/auth.rs` 的
//! `/api/v1/auth/refresh`），所以响应里若带了新 refresh，必须**由 Rust 直接写回**
//! 存储——否则下一次刷新就会 401。

use std::time::Duration;

use serde_json::json;
use yunong_shared::ErrorCode;

use crate::secret::KeyStore;

/// access 令牌的存储 key（与 Dart 侧 `auth_service.dart` 一致）。
pub const ACCESS_KEY: &str = "auth_access_token";

/// refresh 令牌的存储 key（与 Dart 侧 `auth_service.dart` 一致）。
pub const REFRESH_KEY: &str = "auth_refresh_token";

/// 刷新端点路径（服务端 `crates/server/src/api/auth.rs`）。
pub const REFRESH_PATH: &str = "/api/v1/auth/refresh";

/// 刷新失败的原因（决定 FFI 返回码与"要不要清存储"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthRefreshError {
    /// 存储里根本没有 refresh：没登录过，或降级存储重启后丢了
    MissingRefresh,
    /// refresh 被服务端拒绝（401/403）→ 调用方必须已经清掉本地令牌
    Unauthorized(String),
    /// 网络不可达 / 超时 / 服务端暂时不可用 → **本地令牌保留**
    Network(String),
    /// 本地安全存储读写失败
    Storage(String),
    /// `cloud_base_url` 不像个 URL（没写 scheme 之类）
    InvalidBaseUrl(String),
}

impl AuthRefreshError {
    /// FFI 返回码。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingRefresh | Self::Unauthorized(_) => "UNAUTHORIZED",
            Self::Network(_) => "NETWORK",
            Self::Storage(_) => "STORAGE",
            Self::InvalidBaseUrl(_) => "BAD_INPUT",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::MissingRefresh => "登录已失效，请重新登录".to_owned(),
            Self::Unauthorized(message) | Self::Network(message) | Self::Storage(message) => {
                message.clone()
            }
            Self::InvalidBaseUrl(message) => message.clone(),
        }
    }

    /// 是否应该把本地令牌清掉（**只有确定失效时**才清）。
    pub const fn clears_store(&self) -> bool {
        matches!(self, Self::MissingRefresh | Self::Unauthorized(_))
    }

    /// 对应的共享 `ErrorCode`（给日志/上层复用）。
    pub const fn shared_code(&self) -> ErrorCode {
        match self {
            Self::MissingRefresh | Self::Unauthorized(_) => ErrorCode::Unauthorized,
            Self::Network(_) => ErrorCode::UpstreamError,
            Self::Storage(_) => ErrorCode::Internal,
            Self::InvalidBaseUrl(_) => ErrorCode::ValidationError,
        }
    }
}

/// 刷新成功后的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refreshed {
    /// 新的 access（回给 Dart）
    pub access_token: String,
    /// 有效期（秒）；服务端没给就是 0
    pub expires_in: i64,
    /// 服务端是否轮换了 refresh（轮换后已写回存储）
    pub rotated: bool,
}

/// 刷新的超时设置（同步 FFI 调用会阻塞调用线程，所以别太长）。
#[derive(Debug, Clone, Copy)]
pub struct RefreshTimeouts {
    pub connect: Duration,
    pub total: Duration,
}

impl Default for RefreshTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            total: Duration::from_secs(10),
        }
    }
}

/// 把 `cloud_base_url` 拼成刷新端点。
///
/// 用户/配置填的 base 可能带尾斜杠，也可能"已经带了 `/api/v1`"（设置页里很常见），
/// 两种都吃掉，避免拼出 `/api/v1/api/v1/auth/refresh`。
pub fn endpoint(base_url: &str) -> Result<String, AuthRefreshError> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(AuthRefreshError::InvalidBaseUrl(
            "cloud_base_url 不能为空".to_owned(),
        ));
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(AuthRefreshError::InvalidBaseUrl(format!(
            "cloud_base_url 必须带 http:// 或 https://：{trimmed}"
        )));
    }
    let base = trimmed.trim_end_matches('/');
    let base = base.strip_suffix("/api/v1").unwrap_or(base);
    Ok(format!("{base}{REFRESH_PATH}"))
}

/// 用存储里的 refresh 换新 access（async 版，测试与内部复用）。
///
/// **成功**：新 access 写回存储；若服务端轮换了 refresh，新 refresh 也直接写回。
/// **401/403**：先删存储再返回 `Unauthorized`（调用方看到错误时令牌已经没了）。
pub async fn refresh_with(
    store: &dyn KeyStore,
    client: &reqwest::Client,
    base_url: &str,
) -> Result<Refreshed, AuthRefreshError> {
    let url = endpoint(base_url)?;
    let refresh = store
        .load_raw(REFRESH_KEY)
        .map_err(|err| AuthRefreshError::Storage(format!("读 refresh 失败：{err}")))?
        .filter(|value| !value.trim().is_empty())
        .ok_or(AuthRefreshError::MissingRefresh)?;

    let response = client
        .post(&url)
        // reqwest 没开 `json` 特性（不加依赖特性），这里自己序列化并声明 Content-Type
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::to_string(&json!({ "refresh_token": refresh }))
                .expect("凭据序列化不会失败"),
        )
        .send()
        .await
        .map_err(|err| {
            // 连不上 / 超时 / TLS 失败：一律 **不清** 本地令牌
            if err.is_timeout() {
                AuthRefreshError::Network("刷新超时，请检查网络后重试".to_owned())
            } else {
                AuthRefreshError::Network(format!("网络不可用：{err}"))
            }
        })?;

    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        // 令牌确实失效：清掉 access + refresh（清不掉也要如实返回 UNAUTHORIZED）
        let _ = store.delete(REFRESH_KEY);
        let _ = store.delete(ACCESS_KEY);
        return Err(AuthRefreshError::Unauthorized(
            "登录已失效，请重新登录".to_owned(),
        ));
    }
    if !(200..300).contains(&status) {
        // 429/5xx 之类：**保留** 令牌，让用户"重试"而不是"重新登录"
        return Err(AuthRefreshError::Network(format!(
            "服务端暂时不可用（HTTP {status}），请稍后重试"
        )));
    }

    let body = response
        .text()
        .await
        .map_err(|err| AuthRefreshError::Network(format!("读刷新响应失败：{err}")))?;
    let parsed = parse_session(&body)?;

    // 写回存储：写失败不阻断本次登录（access 已在内存里可用），但下次重启会丢
    let _ = store.set(ACCESS_KEY, &parsed.access_token);
    if let Some(refresh) = &parsed.refresh_token {
        let _ = store.set(REFRESH_KEY, refresh);
    }
    Ok(Refreshed {
        access_token: parsed.access_token,
        expires_in: parsed.expires_in,
        rotated: parsed.refresh_token.is_some(),
    })
}

/// 同步版：自己建一个 current-thread runtime 再 `block_on`（给 C ABI 用）。
///
/// **不能在已有 tokio runtime 的线程里调用**（tokio 不允许 runtime 里再 block_on 一个
/// runtime，会 panic）。C ABI 的调用方是 Dart 的原生线程，天然满足；测试里要放在
/// `spawn_blocking` 或普通线程里跑。
pub fn refresh_blocking(
    store: &dyn KeyStore,
    base_url: &str,
    timeouts: RefreshTimeouts,
) -> Result<Refreshed, AuthRefreshError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| AuthRefreshError::Storage(format!("建运行时失败：{err}")))?;
    let client = reqwest::Client::builder()
        .connect_timeout(timeouts.connect)
        .timeout(timeouts.total)
        .build()
        .map_err(|err| AuthRefreshError::Storage(format!("建 HTTP 客户端失败：{err}")))?;
    runtime.block_on(refresh_with(store, &client, base_url))
}

#[derive(Debug)]
struct ParsedSession {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
}

/// 解析刷新响应：既接受服务端的信封 `{"ok":true,"data":{…}}`，
/// 也接受展平的 `{access_token,…}`（网关/代理改写过的响应）。
fn parse_session(body: &str) -> Result<ParsedSession, AuthRefreshError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        AuthRefreshError::Network(format!(
            "刷新响应不是 JSON（前 80 字节：{}）",
            body.chars().take(80).collect::<String>()
        ))
    })?;
    if value.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        // HTTP 200 但信封报错：按服务端不可用处理（不清令牌）
        let message = value
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("刷新失败");
        return Err(AuthRefreshError::Network(message.to_owned()));
    }
    let data = value.get("data").unwrap_or(&value);
    let access_token = data
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| AuthRefreshError::Network("刷新响应里没有 access_token".to_owned()))?
        .to_owned();
    let expires_in = data.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(0);
    let refresh_token = data
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|token| !token.trim().is_empty())
        .map(str::to_owned);
    Ok(ParsedSession {
        access_token,
        expires_in,
        refresh_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::fake_server::FakeServer;
    use crate::secret::MemoryKeyStore;

    fn store_with(refresh: Option<&str>, access: Option<&str>) -> MemoryKeyStore {
        let store = MemoryKeyStore::new("测试用内存存储");
        if let Some(refresh) = refresh {
            store.set(REFRESH_KEY, refresh).unwrap();
        }
        if let Some(access) = access {
            store.set(ACCESS_KEY, access).unwrap();
        }
        store
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
    }

    const OK_BODY: &str = r#"{"ok":true,"data":{"user_id":"u1","access_token":"new-access-1","expires_in":3600,"refresh_token":"new-refresh-1"}}"#;

    #[tokio::test]
    async fn success_returns_new_access_and_rotates_refresh() {
        let server = FakeServer::reply(200, "application/json", OK_BODY).await;
        let store = store_with(Some("old-refresh"), Some("old-access"));

        let refreshed = refresh_with(&store, &client(), &server.base_url())
            .await
            .expect("刷新应当成功");
        assert_eq!(refreshed.access_token, "new-access-1");
        assert_eq!(refreshed.expires_in, 3600);
        assert!(refreshed.rotated, "服务端轮换了 refresh");

        // 请求确实带上了旧 refresh
        let captured = server.captured();
        assert_eq!(captured.path(), REFRESH_PATH);
        assert_eq!(captured.json()["refresh_token"], "old-refresh");

        // 存储被更新成新的 access + 轮换后的 refresh
        assert_eq!(
            store.load_raw(ACCESS_KEY).unwrap().as_deref(),
            Some("new-access-1")
        );
        assert_eq!(
            store.load_raw(REFRESH_KEY).unwrap().as_deref(),
            Some("new-refresh-1"),
            "轮换后的 refresh 必须写回，否则下次刷新就 401"
        );
    }

    #[tokio::test]
    async fn unauthorized_clears_both_tokens() {
        let body = r#"{"ok":false,"code":"UNAUTHORIZED","message":"刷新令牌无效"}"#;
        let server = FakeServer::reply(401, "application/json", body).await;
        let store = store_with(Some("dead-refresh"), Some("dead-access"));

        let error = refresh_with(&store, &client(), &server.base_url())
            .await
            .expect_err("401 必须报错");
        assert_eq!(error.code(), "UNAUTHORIZED");
        assert!(error.clears_store());
        assert_eq!(error.message(), "登录已失效，请重新登录");
        // 两个令牌都被清掉：UI 回未登录态
        assert_eq!(store.load_raw(REFRESH_KEY).unwrap(), None);
        assert_eq!(store.load_raw(ACCESS_KEY).unwrap(), None);
    }

    #[tokio::test]
    async fn forbidden_also_clears() {
        let server = FakeServer::reply(403, "application/json", "{}").await;
        let store = store_with(Some("dead-refresh"), Some("dead-access"));
        let error = refresh_with(&store, &client(), &server.base_url())
            .await
            .expect_err("403 也必须算失效");
        assert_eq!(error.code(), "UNAUTHORIZED");
        assert_eq!(store.load_raw(REFRESH_KEY).unwrap(), None);
    }

    #[tokio::test]
    async fn network_failure_keeps_tokens() {
        // 指向一个没人监听的端口：连接被拒
        let store = store_with(Some("good-refresh"), Some("good-access"));
        let error = refresh_with(&store, &client(), "http://127.0.0.1:1")
            .await
            .expect_err("连不上必须报错");
        assert_eq!(error.code(), "NETWORK", "{error:?}");
        assert!(!error.clears_store(), "网络失败不能清令牌");
        // 令牌原样保留
        assert_eq!(
            store.load_raw(REFRESH_KEY).unwrap().as_deref(),
            Some("good-refresh")
        );
        assert_eq!(
            store.load_raw(ACCESS_KEY).unwrap().as_deref(),
            Some("good-access")
        );
    }

    #[tokio::test]
    async fn rate_limited_response_keeps_tokens() {
        let server = FakeServer::reply(429, "application/json", "{}").await;
        let store = store_with(Some("good-refresh"), None);
        let error = refresh_with(&store, &client(), &server.base_url())
            .await
            .expect_err("429 应当报错");
        assert_eq!(error.code(), "NETWORK");
        assert!(!error.clears_store(), "限流不是登录失效");
        assert_eq!(
            store.load_raw(REFRESH_KEY).unwrap().as_deref(),
            Some("good-refresh")
        );
    }

    #[tokio::test]
    async fn missing_refresh_is_unauthorized_without_network_call() {
        let store = store_with(None, Some("stale-access"));
        // 用一个不可能成功的地址：真发了请求就会是 NETWORK
        let error = refresh_with(&store, &client(), "http://127.0.0.1:1")
            .await
            .expect_err("没有 refresh 应当直接报错");
        assert_eq!(
            error.code(),
            "UNAUTHORIZED",
            "没登录过就是未登录，不是网络问题"
        );
    }

    #[test]
    fn endpoint_normalizes_base_url() {
        assert_eq!(
            endpoint("https://api.example.com").unwrap(),
            "https://api.example.com/api/v1/auth/refresh"
        );
        // 尾斜杠
        assert_eq!(
            endpoint("https://api.example.com/").unwrap(),
            "https://api.example.com/api/v1/auth/refresh"
        );
        // 已经带了 /api/v1（设置页里最常见）
        assert_eq!(
            endpoint("https://api.example.com/api/v1").unwrap(),
            "https://api.example.com/api/v1/auth/refresh"
        );
        assert_eq!(
            endpoint("https://api.example.com/api/v1/").unwrap(),
            "https://api.example.com/api/v1/auth/refresh"
        );
    }

    #[test]
    fn endpoint_rejects_empty_and_scheme_less() {
        assert_eq!(endpoint("").unwrap_err().code(), "BAD_INPUT");
        assert_eq!(endpoint("   ").unwrap_err().code(), "BAD_INPUT");
        assert_eq!(endpoint("api.example.com").unwrap_err().code(), "BAD_INPUT");
    }

    #[test]
    fn parse_accepts_envelope_and_flat_bodies() {
        let envelope = parse_session(OK_BODY).unwrap();
        assert_eq!(envelope.access_token, "new-access-1");
        assert_eq!(envelope.refresh_token.as_deref(), Some("new-refresh-1"));

        let flat = parse_session(r#"{"access_token":"a2","expires_in":60}"#).unwrap();
        assert_eq!(flat.access_token, "a2");
        assert_eq!(flat.expires_in, 60);
        assert!(flat.refresh_token.is_none(), "服务端没轮换就不该编一个");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse_session("<html>").unwrap_err().code(), "NETWORK");
        assert_eq!(parse_session("{}").unwrap_err().code(), "NETWORK");
        assert_eq!(
            parse_session(r#"{"ok":false,"message":"刷新失败"}"#)
                .unwrap_err()
                .message(),
            "刷新失败"
        );
    }
}
