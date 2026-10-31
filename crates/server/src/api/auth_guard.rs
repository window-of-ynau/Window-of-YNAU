//! Bearer 鉴权中间件：`/api/v1/**` 的保护层。
//!
//! 挂载位置见 `lib.rs`：注册/登录/刷新是公开的，登出与其余业务接口都要过这里。
//! 失败一律返回**错误信封**（不是 500、也不是纯文本），
//! `request_id` 从请求扩展里取，保证与响应头一致。

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap},
    middleware::Next,
    response::{IntoResponse, Response},
};
use yunong_shared::ErrorCode;

use crate::api::{error::AppError, request_id::ReqId};
use crate::auth::BEARER_PREFIX;
use crate::state::AppState;

/// 校验 `Authorization: Bearer <access_token>`，通过后把 [`crate::auth::Principal`] 放进扩展。
pub async fn require_bearer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    // 请求 ID 中间件在更外层，正常路径一定拿得到；真没有也不能让信封和响应头不一致。
    let req_id = request
        .extensions()
        .get::<ReqId>()
        .cloned()
        .unwrap_or_default();

    // 先转成 String：借着头去 await 会和后面的 `extensions_mut` 打架。
    let Some(token) = bearer_token(request.headers()).map(str::to_owned) else {
        return AppError::new(
            ErrorCode::Unauthorized,
            "缺少 Authorization: Bearer <access_token>",
            &req_id.0,
        )
        .into_response();
    };

    match state.auth.authenticate(&token).await {
        Ok(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Err(err) => AppError::from_auth(err, &req_id.0).into_response(),
    }
}

/// 从请求头里取 Bearer 令牌；缺头、前缀不对、空令牌都算缺失。
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix(BEARER_PREFIX)?.trim();
    (!token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, value.parse().expect("头值应合法"));
        headers
    }

    #[test]
    fn parses_bearer_token() {
        assert_eq!(bearer_token(&headers("Bearer abc123")), Some("abc123"));
        assert_eq!(bearer_token(&headers("Bearer  abc123 ")), Some("abc123"));
    }

    #[test]
    fn rejects_wrong_or_empty_scheme() {
        assert_eq!(bearer_token(&HeaderMap::new()), None);
        assert_eq!(bearer_token(&headers("Basic abc123")), None);
        assert_eq!(bearer_token(&headers("Bearer")), None);
        assert_eq!(bearer_token(&headers("Bearer   ")), None);
    }
}
