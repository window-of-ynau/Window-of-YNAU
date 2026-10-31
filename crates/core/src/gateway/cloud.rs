//! 套餐：经我们服务端转发（ADR-03）。端侧只持时效令牌，**不持上游 Key**。
//!
//! 请求就是完整的 `ChatRequest`（`docs/03` §3.2）；响应是我们自己的 SSE，
//! 事件集合见 [`super::cloud_sse`]。
//!
//! 客户端仍要防"服务端老版本 / 中间层切断"：上游没发终止事件就由装配器
//! `finish()` 补 `Done{Stop}`，绝不让 UI 卡在"永远等不到结束"的状态。

use std::time::Duration;

use yunong_shared::{Channel, ChatRequest, Chunk, ErrorEnvelope};

use super::cloud_sse::parse_cloud_event;
use super::direct::finish_of;
use super::{
    ensure_event_stream, provider_error_message, pump_sse, remote_error, resolved_usage,
    retry_after, send_with_first_event_timeout, status_code, AssemblerPlan, Flow, GatewayFuture,
    ModelGateway, StreamOutcome, Timeouts,
};
use crate::error::CoreError;

/// 我们服务端的对话流端点（`docs/03` §3.2）。
pub const CHAT_STREAM_PATH: &str = "/api/v1/chat/stream";

/// 套餐通道代理。
#[derive(Debug, Clone)]
pub struct CloudProxy {
    pub base_url: String,
    /// 时效令牌（`docs/03` §2）；过期后由上层用 refresh 换新
    pub access_token: String,
    timeouts: Timeouts,
}

impl CloudProxy {
    pub fn new(base_url: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            access_token: access_token.into(),
            timeouts: Timeouts::default(),
        }
    }

    /// 覆盖三段超时（服务端 15s 心跳，默认空闲 30s 足够宽松）。
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// 实际请求的 URL。
    pub fn endpoint(&self) -> String {
        format!("{}{CHAT_STREAM_PATH}", self.base_url.trim_end_matches('/'))
    }
}

impl ModelGateway for CloudProxy {
    fn chat_stream<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_chunk: &'a mut (dyn FnMut(Chunk) -> Flow + Send),
    ) -> GatewayFuture<'a> {
        Box::pin(async move {
            let payload = serde_json::to_vec(request)
                .map_err(|err| CoreError::Invalid(format!("请求体序列化失败：{err}")))?;

            let client = reqwest::Client::builder()
                .connect_timeout(self.timeouts.connect)
                .build()
                .map_err(|err| CoreError::Network(format!("构造 HTTP 客户端失败：{err}")))?;

            let http_request = client
                .post(self.endpoint())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .header(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", self.access_token),
                )
                .body(payload);
            let response = send_with_first_event_timeout(http_request, self.timeouts).await?;

            let status = response.status().as_u16();
            if !response.status().is_success() {
                let wait = retry_after(response.headers());
                let body = response.text().await.unwrap_or_default();
                return Err(cloud_error(status, &body, wait));
            }
            ensure_event_stream(response.headers())?;

            let plan = AssemblerPlan {
                channel: Channel::Plan,
                model: request.model.clone(),
                fallback_request_id: format!("plan_{}", uuid::Uuid::new_v4().simple()),
            };
            let pumped = pump_sse(
                response,
                self.timeouts,
                plan,
                |frame| parse_cloud_event(frame).into_iter().collect(),
                on_chunk,
            )
            .await?;

            Ok(StreamOutcome {
                usage: resolved_usage(pumped.usage, request, &pumped.output_text),
                finish: finish_of(&pumped),
                cancelled: pumped.cancelled,
                // 用服务端 `meta.request_id`（支持断线重查 `GET /chat/{request_id}`）
                request_id: pumped.request_id,
            })
        })
    }

    fn goes_through_cloud(&self) -> bool {
        true
    }
}

/// 服务端错误体 → [`CoreError::Remote`]。
///
/// 优先用我们自己的 `ErrorEnvelope`（`code` 与云端信封一一对应，401/402/429 语义
/// 不会在传输层被压扁）；解析不出来时按 HTTP 状态码兜底。
fn cloud_error(status: u16, body: &str, retry: Option<Duration>) -> CoreError {
    if let Ok(envelope) = serde_json::from_str::<ErrorEnvelope>(body) {
        return remote_error(status, envelope.error.code, envelope.error.message, retry);
    }
    remote_error(
        status,
        status_code(status),
        provider_error_message(body),
        retry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use yunong_shared::ErrorCode;

    #[test]
    fn cloud_endpoint_joins_path() {
        let proxy = CloudProxy::new("https://api.yunong.example/", "tok");
        assert_eq!(
            proxy.endpoint(),
            "https://api.yunong.example/api/v1/chat/stream"
        );
    }

    #[test]
    fn error_envelope_code_wins_over_status() {
        // 429 但信封里写的是 CONCURRENCY_LIMIT：语义以信封为准
        let body = r#"{"ok":false,"error":{"code":"CONCURRENCY_LIMIT","message":"同一 turn 已有在途请求","request_id":"req_1"}}"#;
        match cloud_error(429, body, Some(Duration::from_secs(3))) {
            CoreError::Remote {
                code,
                message,
                retry_after,
            } => {
                assert_eq!(code, ErrorCode::ConcurrencyLimit);
                assert!(message.contains("在途请求"), "{message}");
                assert_eq!(retry_after, Some(Duration::from_secs(3)));
            }
            other => panic!("应为 Remote：{other:?}"),
        }
    }

    #[test]
    fn status_code_fallback_without_envelope() {
        for (status, expected) in [
            (401, ErrorCode::Unauthorized),
            (402, ErrorCode::QuotaExceeded),
            (429, ErrorCode::RateLimited),
            (504, ErrorCode::UpstreamTimeout),
            (500, ErrorCode::UpstreamError),
        ] {
            match cloud_error(status, "<html>bad gateway</html>", None) {
                CoreError::Remote { code, .. } => assert_eq!(code, expected, "status={status}"),
                other => panic!("应为 Remote：{other:?}"),
            }
        }
    }

    #[test]
    fn error_message_is_truncated() {
        let long = "x".repeat(5000);
        let body = format!(
            r#"{{"ok":false,"error":{{"code":"INTERNAL","message":"{long}","request_id":"r"}}}}"#
        );
        match cloud_error(500, &body, None) {
            CoreError::Remote { message, .. } => {
                // 200 字符 + "HTTP 500：" 前缀 + 省略号
                assert!(message.chars().count() < 260, "{}", message.chars().count());
            }
            other => panic!("应为 Remote：{other:?}"),
        }
    }
}
