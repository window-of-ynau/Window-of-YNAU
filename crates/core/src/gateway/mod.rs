//! 模型网关：两条通道一个抽象（ADR-03 / ADR-04）。
//!
//! ```text
//!                   ┌─ DirectProvider  ──▶ 厂商（BYOK，Key 不出端）
//! ModelGateway ─────┤
//!                   └─ CloudProxy      ──▶ 我们的服务端（套餐，服务端代理）
//! ```
//!
//! # 归约：两条通道产出同一条 `Chunk` 序列
//!
//! 本模块只负责"把 HTTP/SSE 变成 `Chunk`"。**`Meta` 注入、`Done` 去重、`usage` 合并
//! 一律交给 [`ChunkAssembler`](yunong_shared::stream::ChunkAssembler)**（ADR-16）——
//! 端侧网关与服务端代理各写一份必然分叉。
//!
//! # 三段超时（语义分开，**不要**用全局 `.timeout()`）
//!
//! | 阶段 | 默认 | 说明 |
//! |---|---|---|
//! | 建连 | 10s | 含 TLS 握手 |
//! | 首事件 | 90s | 推理模型可能 30~60s 才吐第一个 token，卡太死会把长思考全杀掉 |
//! | 事件间空闲 | 30s | 每收到字节就重置；服务端 15s 心跳，30s 无字节判定死链路 |
//!
//! **不设总时长上限**：`reqwest::Client::builder().timeout()` 是"整个请求含响应体"的
//! 总时长，用在 SSE 上会把长回答拦腰切断。"超过 30s 未完成就降级"是上层调度策略，
//! 不该由传输层决定。
//!
//! # 取消
//!
//! 回调返回 [`Flow::Stop`]，或调用方直接 drop 掉 [`GatewayFuture`]，都会立刻丢弃响应体 →
//! TCP 关闭 → 上游请求取消（不再计费）。这两条都有集成测试守着。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use yunong_shared::stream::ChunkAssembler;
use yunong_shared::{Channel, ChatRequest, Chunk, ErrorCode, FinishReason, Usage};

use crate::context::estimate_tokens;
use crate::error::{CoreError, CoreResult};

mod cloud;
mod cloud_sse;
mod direct;
#[cfg(test)]
mod fake_server;
pub mod sse;
#[cfg(test)]
mod tests;
mod wire;

pub use cloud::CloudProxy;
pub use cloud_sse::parse_cloud_event;
pub use direct::{ApiKeySource, DirectProvider};
pub use sse::{SseDecoder, SseFrame};

/// 错误信息最多保留多少字符。
///
/// 上游错误体可能回显部分 prompt，整段进日志/界面既脏又险；只留前 200 字。
pub(crate) const MAX_ERROR_CHARS: usize = 200;

/// 回调的继续信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// 继续读
    Continue,
    /// 立刻停：丢弃响应体，上游请求被取消
    Stop,
}

/// 一次流式调用的终值（`Chunk` 已经通过回调发出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOutcome {
    /// 装配后的用量终值（= `ChunkAssembler::usage()`）；上游没给时是端侧估算值。
    /// **记账只读这个**，不要逐事件累加。
    pub usage: Usage,
    /// 终止原因；取消或上游没给 `Done` 时是 [`FinishReason::Stop`]，
    /// 上游报错是 [`FinishReason::Error`]。
    pub finish: FinishReason,
    /// 是否被调用方提前取消（回调返回 `Stop` 或被 drop）
    pub cancelled: bool,
    /// 套餐通道的 `meta.request_id`（BYOK 通道是 `None`）
    pub request_id: Option<String>,
}

/// 手工装箱的 Future：让 [`ModelGateway`] 保持 **object-safe**
/// （`Box<dyn ModelGateway>` 可用），又不引入 `async-trait`。
pub type GatewayFuture<'a> = Pin<Box<dyn Future<Output = CoreResult<StreamOutcome>> + Send + 'a>>;

/// 上层只依赖这个 trait，不关心走哪条通道。
pub trait ModelGateway {
    /// 发起一次流式调用：每产出一个**契约合法**的 `Chunk` 就回调一次。
    ///
    /// 回调返回 [`Flow::Stop`] 时立刻取消（见模块注释）。
    fn chat_stream<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_chunk: &'a mut (dyn FnMut(Chunk) -> Flow + Send),
    ) -> GatewayFuture<'a>;

    /// 这条通道是否经过我们的服务器。
    fn goes_through_cloud(&self) -> bool;
}

/// 收集整条流（测试、bench、非流式调用方用）。
///
/// 返回 `(chunks, outcome)`；`chunks` 已经是契约合法序列（`Meta` 首、`Done` 末尾唯一）。
pub async fn collect_chunks<G: ModelGateway + ?Sized>(
    gateway: &G,
    request: &ChatRequest,
) -> CoreResult<(Vec<Chunk>, StreamOutcome)> {
    let mut chunks = Vec::new();
    let outcome = {
        let mut sink = |chunk: Chunk| {
            chunks.push(chunk);
            Flow::Continue
        };
        gateway.chat_stream(request, &mut sink).await?
    };
    Ok((chunks, outcome))
}

/// 三段超时（见模块注释）。测试可以按需调小。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// 建立连接（含 TLS 握手）
    pub connect: Duration,
    /// 连上之后到**第一个** SSE 事件的等待上限
    pub first_event: Duration,
    /// 事件之间的空闲上限（每收到字节重置）
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            first_event: Duration::from_secs(90),
            idle: Duration::from_secs(30),
        }
    }
}

// ---------------------------------------------------------------------------
// 两条通道共用的 HTTP 细节
// ---------------------------------------------------------------------------

/// reqwest 的传输层错误 → 我们的错误（超时与网络要分开，重试策略不同）。
pub(crate) fn map_transport(err: reqwest::Error) -> CoreError {
    if err.is_timeout() {
        CoreError::Timeout(format!("HTTP 超时：{err}"))
    } else {
        CoreError::Network(err.to_string())
    }
}

/// `Retry-After` 头的秒数形式（HTTP-date 形式不解析，返回 `None`）。
pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// 响应体不可解析时，按 HTTP 状态码兜底的错误码。
pub(crate) fn status_code(status: u16) -> ErrorCode {
    match status {
        401 => ErrorCode::Unauthorized,
        403 => ErrorCode::Forbidden,
        402 => ErrorCode::QuotaExceeded,
        429 => ErrorCode::RateLimited,
        504 => ErrorCode::UpstreamTimeout,
        _ => ErrorCode::UpstreamError,
    }
}

/// 非 2xx → [`CoreError::Remote`]。
///
/// `message` 会被截断到 [`MAX_ERROR_CHARS`]：上游错误体可能含 prompt 回显。
pub(crate) fn remote_error(
    status: u16,
    code: ErrorCode,
    message: impl Into<String>,
    retry: Option<Duration>,
) -> CoreError {
    let message = message.into();
    let message = if message.trim().is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}：{}", truncate(&message, MAX_ERROR_CHARS))
    };
    CoreError::Remote {
        code,
        message,
        retry_after: retry,
    }
}

/// 从供应商错误体里挖人类可读的 message。
///
/// 三家形状：`{"error":{"message":..}}`、`{"error":".."}`、Anthropic 的
/// `{"type":"error","error":{"type":..,"message":..}}`。
pub(crate) fn provider_error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.trim().to_owned();
    };
    if let Some(error) = value.get("error") {
        if let Some(text) = error.as_str() {
            return text.to_owned();
        }
        if let Some(text) = error.get("message").and_then(serde_json::Value::as_str) {
            return text.to_owned();
        }
    }
    if let Some(text) = value.get("message").and_then(serde_json::Value::as_str) {
        return text.to_owned();
    }
    body.trim().to_owned()
}

/// 响应头必须是 `text/event-stream`（明确拒绝 HTML——中间层/登录页的典型形状）。
///
/// 上游偶尔不发 `Content-Type`：这种情况放行，交给解析层用内容判断。
pub(crate) fn ensure_event_stream(headers: &reqwest::header::HeaderMap) -> CoreResult<()> {
    let Some(value) = headers.get(reqwest::header::CONTENT_TYPE) else {
        return Ok(());
    };
    let raw = value.to_str().unwrap_or_default().to_ascii_lowercase();
    if raw.contains("text/event-stream") {
        return Ok(());
    }
    Err(CoreError::Remote {
        code: ErrorCode::UpstreamError,
        message: truncate(
            &format!("期望 text/event-stream，实际是 `{raw}`（可能是代理缓冲/登录页）"),
            MAX_ERROR_CHARS,
        ),
        retry_after: None,
    })
}

/// 按字符数截断（不按字节，避免切坏中文/emoji），并补一个省略号。
///
/// 结果长度**不超过** `max_chars`（省略号也计入），这样 `MAX_ERROR_CHARS` 是硬上限。
pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars - 1).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 上游没给 `usage` 时的兜底估算：输入用请求里已有的估算值 + 实际产出文本，并标 `estimated`。
///
/// 什么都估算不出来（空输入空输出）时给 0，**不要伪造数字**。
pub(crate) fn fallback_usage(request: &ChatRequest, output: &str) -> Usage {
    let input = if request.estimated_input_tokens > 0 {
        request.estimated_input_tokens
    } else {
        let mut joined = String::new();
        for message in &request.messages {
            joined.push_str(&message.content);
            for call in &message.tool_calls {
                joined.push_str(&call.arguments.to_string());
            }
        }
        estimate_tokens(&joined)
    };
    Usage {
        tokens_in: input,
        tokens_out: estimate_tokens(output),
        estimated: true,
    }
}

/// 上游给了 usage 就用它；一个字段都没给时才走端侧估算兜底。
pub(crate) fn resolved_usage(usage: Usage, request: &ChatRequest, output: &str) -> Usage {
    if usage.tokens_in == 0 && usage.tokens_out == 0 {
        fallback_usage(request, output)
    } else {
        usage
    }
}

// ---------------------------------------------------------------------------
// 两条通道共用的读循环
// ---------------------------------------------------------------------------

/// 装配器的初始化参数（两条通道共用同一个读循环）。
pub(crate) struct AssemblerPlan {
    /// 这条流属于哪条通道（会写进对外的 `Meta`）
    pub channel: Channel,
    /// 对外的模型名
    pub model: String,
    /// 上游没给 `request_id` 时的兜底 id
    pub fallback_request_id: String,
}

/// 一轮 SSE 读取的结果。
#[derive(Debug, Default)]
pub(crate) struct Pumped {
    /// 累计的可见文本（上游没给 usage 时用来估算输出 token）
    pub output_text: String,
    /// 被调用方取消
    pub cancelled: bool,
    /// 已发出的终止原因
    pub finish: Option<FinishReason>,
    /// 上游用 `error` 事件终止时的错误码/信息
    pub error: Option<(ErrorCode, String)>,
    /// 套餐通道 `meta.request_id`（BYOK 没有）
    pub request_id: Option<String>,
    /// 装配后的用量终值
    pub usage: Usage,
}

/// 发出请求并把**首事件超时**盖在 `send()` 上。
///
/// `reqwest::send()` 的实际语义是"等到响应有第一块 body 数据"（不是"等到响应头"）：
/// 上游一直不说话时 `send()` 会一直挂着。所以首事件上限必须包住 `send()`，
/// 否则"等第一个 token"根本不受超时约束。
pub(crate) async fn send_with_first_event_timeout(
    request: reqwest::RequestBuilder,
    timeouts: Timeouts,
) -> CoreResult<reqwest::Response> {
    match tokio::time::timeout(timeouts.first_event, request.send()).await {
        Ok(result) => result.map_err(map_transport),
        Err(_) => Err(CoreError::Timeout(format!(
            "上游 {:.1}s 内没有返回任何数据（连上后等首帧超时）",
            timeouts.first_event.as_secs_f64()
        ))),
    }
}

/// 把一条 `text/event-stream` 响应泵成 `Chunk`。
///
/// - 首事件用 `timeouts.first_event` 单独计，之后每收到一段字节就把 `timeouts.idle` 重置；
/// - `map_frame` 把一帧解释成 0..n 个 `Chunk`（BYOK = `protocols::parse_events`，
///   套餐 = [`parse_cloud_event`]）；
/// - 上游的 `Meta` **不透传**：只用来取 `request_id`，对外 `Meta` 由装配器注入；
/// - 回调返回 [`Flow::Stop`] → 立刻返回（响应体被 drop → 连接关闭 → 上游取消）；
/// - 上游正常关闭但没给终止事件 → `ChunkAssembler::finish()` 补 `Done{Stop}`。
pub(crate) async fn pump_sse(
    mut response: reqwest::Response,
    timeouts: Timeouts,
    plan: AssemblerPlan,
    mut map_frame: impl FnMut(&SseFrame) -> Vec<Chunk>,
    on_chunk: &mut (dyn FnMut(Chunk) -> Flow + Send),
) -> CoreResult<Pumped> {
    let mut pumped = Pumped::default();
    let mut assembler: Option<ChunkAssembler> = None;
    let mut decoder = SseDecoder::new();
    let mut first = true;

    loop {
        let wait = if first {
            timeouts.first_event
        } else {
            timeouts.idle
        };
        let next = match tokio::time::timeout(wait, response.chunk()).await {
            Ok(result) => result.map_err(map_transport)?,
            Err(_) if first => {
                return Err(CoreError::Timeout(format!(
                    "上游 {:.1}s 内没有返回第一个 SSE 事件",
                    wait.as_secs_f64()
                )))
            }
            Err(_) => {
                return Err(CoreError::Timeout(format!(
                    "上游 SSE 空闲超过 {:.1}s",
                    wait.as_secs_f64()
                )))
            }
        };
        let (frames, eof) = match next {
            Some(bytes) => {
                first = false;
                (decoder.push(&bytes), false)
            }
            // 上游正常关闭：把"没以空行收尾"的最后一帧也吐出来
            None => (decoder.finish(), true),
        };

        for frame in frames {
            for chunk in map_frame(&frame) {
                // 上游 Meta：只取 request_id，不透传（装配器自己发，且必首唯一）
                if let Chunk::Meta { request_id, .. } = &chunk {
                    if !request_id.is_empty() && pumped.request_id.is_none() {
                        pumped.request_id = Some(request_id.clone());
                    }
                    continue;
                }
                if let Chunk::Delta { text } = &chunk {
                    pumped.output_text.push_str(text);
                }
                let asm = assembler.get_or_insert_with(|| {
                    ChunkAssembler::new(
                        plan.channel,
                        pumped
                            .request_id
                            .clone()
                            .unwrap_or_else(|| plan.fallback_request_id.clone()),
                        plan.model.clone(),
                    )
                });
                for out in asm.feed(vec![chunk]) {
                    record(&mut pumped, &out);
                    if on_chunk(out) == Flow::Stop {
                        pumped.cancelled = true;
                        pumped.usage = asm.usage();
                        return Ok(pumped);
                    }
                }
            }
        }
        if eof {
            break;
        }
    }

    // 正常关闭：没收到终止事件就补 Done{Stop}（服务端老版本 / 中间层切断都能兜住）
    let mut asm = assembler.unwrap_or_else(|| {
        ChunkAssembler::new(
            plan.channel,
            pumped
                .request_id
                .clone()
                .unwrap_or_else(|| plan.fallback_request_id.clone()),
            plan.model.clone(),
        )
    });
    for out in asm.finish() {
        record(&mut pumped, &out);
        if on_chunk(out) == Flow::Stop {
            pumped.cancelled = true;
            break;
        }
    }
    pumped.usage = asm.usage();
    Ok(pumped)
}

fn record(pumped: &mut Pumped, chunk: &Chunk) {
    match chunk {
        Chunk::Done { finish } => pumped.finish = Some(*finish),
        Chunk::Error { code, message } => pumped.error = Some((*code, message.clone())),
        _ => {}
    }
}
