//! BYOK：端侧直连厂商（ADR-04）。
//!
//! Key 只在**发请求前取一次**，用完即弃，且永不进日志（[`ApiKeySource`] 的 `Debug` 会打码）。
//!
//! 请求/响应都是薄适配：请求体由 `crate::protocols::build_body_with` 组，
//! 响应流由 `crate::protocols::parse_events` 解释成 `Chunk`，
//! 再去重/合并交给 `ChunkAssembler`。

use std::sync::Arc;

use yunong_shared::{Channel, ChatRequest, Chunk, FinishReason, Protocol};

use super::wire;
use super::{
    ensure_event_stream, provider_error_message, pump_sse, remote_error, resolved_usage,
    retry_after, send_with_first_event_timeout, status_code, AssemblerPlan, Flow, GatewayFuture,
    ModelGateway, StreamOutcome, Timeouts,
};
use crate::error::{CoreError, CoreResult};
use crate::protocols::{build_body_with, BuildOptions};

/// API Key 的来源。
///
/// 生产走 keystore：`ApiKeySource::Provider(Arc::new(move || store.get(SecretKind::ApiKey)))`，
/// 每次发请求前取一次，Key **不常驻**在 [`DirectProvider`] 的字段里。
/// 测试与桌面开发可以直接给字符串。
#[derive(Clone)]
pub enum ApiKeySource {
    /// 直接给值（测试/桌面；不要用于生产）
    Inline(String),
    /// 每次请求前回调取（keystore / DPAPI / Keychain）
    Provider(Arc<dyn Fn() -> CoreResult<String> + Send + Sync>),
}

impl ApiKeySource {
    fn resolve(&self) -> CoreResult<String> {
        let raw = match self {
            Self::Inline(key) => key.clone(),
            Self::Provider(read) => read()?,
        };
        let key = raw.trim().to_owned();
        if key.is_empty() {
            return Err(CoreError::SecretMissing("ApiKey"));
        }
        Ok(key)
    }
}

impl std::fmt::Debug for ApiKeySource {
    /// **绝不打印 Key**：只打出来源与打码后的前几位。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inline(key) => write!(f, "Inline({}***)", mask(key)),
            Self::Provider(_) => f.write_str("Provider(<fn>)"),
        }
    }
}

fn mask(key: &str) -> String {
    let trimmed = key.trim();
    match trimmed.char_indices().nth(4) {
        Some((idx, _)) => trimmed[..idx].to_owned(),
        None => String::new(),
    }
}

/// BYOK：端侧直连厂商。
#[derive(Debug, Clone)]
pub struct DirectProvider {
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    key: Option<ApiKeySource>,
    report_usage: bool,
    timeouts: Timeouts,
}

impl DirectProvider {
    /// 不带 Key 的构造：请求时会报 `SecretMissing`，方便先把 UI 壳搭起来。
    pub fn new(protocol: Protocol, base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            protocol,
            base_url: base_url.into(),
            model: model.into(),
            key: None,
            report_usage: true,
            timeouts: Timeouts::default(),
        }
    }

    /// 直接给 Key（测试/桌面）。
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(ApiKeySource::Inline(key.into()));
        self
    }

    /// 每次请求前从 keystore 取 Key（生产的唯一姿势）。
    pub fn with_api_key_provider(
        mut self,
        read: impl Fn() -> CoreResult<String> + Send + Sync + 'static,
    ) -> Self {
        self.key = Some(ApiKeySource::Provider(Arc::new(read)));
        self
    }

    /// 是否让上游在流里返回 usage（Completions 的 `stream_options.include_usage`）。
    ///
    /// 默认 `true`：不开的话 OpenAI 流里根本没有 usage，记账只能靠估算。
    /// 上游是**第三方兼容网关**、因为未知字段返回 400 时关掉它
    /// （不要做"400 后自动去字段重试"：隐式重试会让计费翻倍且难排查）。
    pub fn with_usage_reporting(mut self, enabled: bool) -> Self {
        self.report_usage = enabled;
        self
    }

    /// 覆盖三段超时（默认 10s / 90s / 30s，见模块文档）。
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// 实际请求的 URL（含协议路径）。
    pub fn endpoint(&self) -> String {
        wire::endpoint(&self.base_url, self.protocol)
    }

    fn api_key(&self) -> CoreResult<String> {
        self.key
            .as_ref()
            .ok_or(CoreError::SecretMissing("ApiKey"))?
            .resolve()
    }
}

impl ModelGateway for DirectProvider {
    fn chat_stream<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_chunk: &'a mut (dyn FnMut(Chunk) -> Flow + Send),
    ) -> GatewayFuture<'a> {
        Box::pin(async move {
            let key = self.api_key()?;
            let body = build_body_with(
                self.protocol,
                request,
                &BuildOptions {
                    report_usage: self.report_usage,
                },
            );
            let payload = serde_json::to_vec(&body)
                .map_err(|err| CoreError::Invalid(format!("请求体序列化失败：{err}")))?;

            let client = reqwest::Client::builder()
                .connect_timeout(self.timeouts.connect)
                .build()
                .map_err(|err| CoreError::Network(format!("构造 HTTP 客户端失败：{err}")))?;

            let mut builder = client
                .post(self.endpoint())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .body(payload);
            for (name, value) in wire::auth_headers(self.protocol, &key) {
                builder = builder.header(name, value);
            }

            let response = send_with_first_event_timeout(builder, self.timeouts).await?;
            let status = response.status().as_u16();
            if !response.status().is_success() {
                let wait = retry_after(response.headers());
                let body = response.text().await.unwrap_or_default();
                return Err(remote_error(
                    status,
                    status_code(status),
                    provider_error_message(&body),
                    wait,
                ));
            }
            ensure_event_stream(response.headers())?;

            let protocol = self.protocol;
            let plan = AssemblerPlan {
                channel: Channel::Byok,
                model: request.model.clone(),
                fallback_request_id: format!("byok_{}", uuid::Uuid::new_v4().simple()),
            };
            let pumped = pump_sse(
                response,
                self.timeouts,
                plan,
                move |frame| crate::protocols::parse_events(protocol, &frame.data),
                on_chunk,
            )
            .await?;

            Ok(StreamOutcome {
                usage: resolved_usage(pumped.usage, request, &pumped.output_text),
                finish: finish_of(&pumped),
                cancelled: pumped.cancelled,
                // BYOK 不经过我们的服务器，没有 request_id 概念
                request_id: None,
            })
        })
    }

    fn goes_through_cloud(&self) -> bool {
        false
    }
}

/// 统一收尾原因：上游给了 `Done` 用它，报错用 `Error`，否则 `Stop`。
pub(super) fn finish_of(pumped: &super::Pumped) -> FinishReason {
    if let Some(finish) = pumped.finish {
        finish
    } else if pumped.error.is_some() {
        FinishReason::Error
    } else {
        FinishReason::Stop
    }
}
