//! 对话代理（套餐通道）：把上游的流式事件转成我们自己的 SSE（ADR-03 / ADR-05）。
//!
//! 一次请求的完整链路：
//!
//! ```text
//! Bearer 鉴权（auth_guard）
//!   └─► 契约校验 / 单轮上限（本文件）
//!         └─► 并发闸门（按 turn_id：全局 50 + 单用户 1）→ 超限 429 + Retry-After
//!               └─► 上游开流（超时→504 / 4xx·5xx→502，都走统一信封）
//!                     └─► ChunkAssembler（Meta 必首、Done/Error 必末且唯一、usage 合并）
//!                           └─► SSE 事件流
//!                                 └─► 抽样记账 / 审计（fire-and-forget，失败不影响流）
//! ```
//!
//! 客户端断开时，响应体被丢弃 → 生产任务写 channel 失败 → 立刻 `drop` 上游连接，
//! 上游看到对端关闭就停止生成（**不再烧 token**）；闸门名额与记账都在任务收尾时处理。

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::{
    body::{Body, Bytes},
    extract::{Extension, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_core::Stream;
use tokio::sync::mpsc;
use uuid::Uuid;
use yunong_shared::{ApiError, Channel, ChatRequest, Chunk, ChunkAssembler, ErrorCode, Usage};

use crate::accounting::{AuditEntry, UsageSample};
use crate::api::error::AppError;
use crate::api::request_id::ReqId;
use crate::auth::Principal;
use crate::gate::{GateRejection, TurnPermit};
use crate::state::AppState;
use crate::upstream::{self, Timeouts, Upstream};

/// SSE 通道缓冲：给客户端一点抖动空间，同时保留背压（满了就暂停读上游）。
const SSE_CHANNEL_CAPACITY: usize = 8;

/// `Chunk` 的 SSE 事件名（`docs/03` §10 的事件表）。
fn event_name(chunk: &Chunk) -> &'static str {
    match chunk {
        Chunk::Meta { .. } => "meta",
        Chunk::Delta { .. } => "delta",
        Chunk::ToolCall { .. } => "tool_call",
        Chunk::ToolResult { .. } => "tool_result",
        Chunk::Usage { .. } => "usage",
        Chunk::Done { .. } => "done",
        Chunk::Error { .. } => "error",
        Chunk::Ping => "ping",
    }
}

/// 一条事件编码成 wire 字节：`event: <name>\ndata: <json>\n\n`。
fn encode_event(chunk: &Chunk) -> Bytes {
    // Chunk 全是简单结构体，序列化失败只可能是实现写错；退化成空对象也不能把连接搞断
    let data = serde_json::to_string(chunk).unwrap_or_else(|_| "{}".to_owned());
    Bytes::from(format!("event: {}\ndata: {data}\n\n", event_name(chunk)))
}

/// `POST /api/v1/chat/stream`（需 Bearer）：套餐通道的流式代理。
pub async fn chat_stream(
    State(state): State<AppState>,
    Extension(req_id): Extension<ReqId>,
    Extension(principal): Extension<Principal>,
    body: Bytes,
) -> Result<Response, AppError> {
    // 手写解析而不是用 `Json<ChatRequest>`：axum 默认的 422 响应体不是我们的错误信封。
    let request: ChatRequest = serde_json::from_slice(&body).map_err(|error| {
        AppError::new(
            ErrorCode::ValidationError,
            format!("请求体不是合法的 ChatRequest：{error}"),
            &req_id.0,
        )
    })?;
    request
        .validate()
        .map_err(|(code, message)| AppError::new(code, message, &req_id.0))?;

    // 服务端再守一道：配置可收紧上限，不信任客户端自报。
    if request.estimated_input_tokens > state.config.max_input_tokens {
        return Err(AppError::new(
            ErrorCode::MessageTooLarge,
            format!(
                "输入约 {} tokens，超过服务端上限 {}",
                request.estimated_input_tokens, state.config.max_input_tokens
            ),
            &req_id.0,
        ));
    }

    // 1) 并发闸门：按 turn 计（不是按 HTTP 请求计）
    let permit = match state
        .gate
        .acquire(principal.user_id, &request.turn_id)
        .await
    {
        Ok(permit) => permit,
        Err(rejection) => {
            state.accounting.record_audit_detached(AuditEntry::deny(
                principal.user_id,
                req_id.0.clone(),
                request.turn_id.clone(),
                gate_reason(rejection),
            ));
            tracing::info!(
                request_id = %req_id.0,
                turn_id = %request.turn_id,
                rejection = ?rejection,
                "并发闸门拒绝"
            );
            return Ok(too_many_requests(rejection, &req_id.0));
        }
    };

    // 2) 上游开流：连不上/超时/非 2xx 都在这里变成 502/504 信封
    let Some(provider) = state.config.upstream.provider(request.protocol) else {
        return Err(AppError::new(
            ErrorCode::UpstreamError,
            format!(
                "未配置 {:?} 的上游（UPSTREAM_BASE_URL / UPSTREAM_API_KEY）",
                request.protocol
            ),
            &req_id.0,
        ));
    };
    let timeouts = Timeouts {
        connect: Duration::from_secs(state.config.upstream_connect_timeout_secs),
        read: Duration::from_secs(state.config.upstream_read_timeout_secs),
    };
    let upstream = match upstream::open(request.protocol, provider, &request, timeouts).await {
        Ok(upstream) => upstream,
        Err(error) => {
            tracing::warn!(
                request_id = %req_id.0,
                error = %error,
                code = error.code().as_str(),
                "上游开流失败"
            );
            return Err(AppError::new(
                error.code(),
                error.message().to_owned(),
                &req_id.0,
            ));
        }
    };

    // 3) 审计放行 + 起流
    state.accounting.record_audit_detached(AuditEntry::allow(
        principal.user_id,
        req_id.0.clone(),
        request.turn_id.clone(),
    ));
    tracing::info!(
        request_id = %req_id.0,
        session_id = %request.session_id,
        turn_id = %request.turn_id,
        round = request.round,
        model = %request.model,
        protocol = ?request.protocol,
        "开始转发"
    );

    let heartbeat = Duration::from_secs(state.config.sse_heartbeat_secs);
    let body = Body::from_stream(sse_stream(
        state,
        permit,
        upstream,
        request,
        principal.user_id,
        req_id.0.clone(),
        heartbeat,
    ));

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        // 别让中间层缓存或缓冲：SSE 要逐条下发
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .expect("静态响应头一定合法"))
}

/// 429 + `Retry-After`，错误体仍是统一信封。
fn too_many_requests(rejection: GateRejection, request_id: &str) -> Response {
    let envelope =
        ApiError::new(ErrorCode::ConcurrencyLimit, rejection.message()).into_envelope(request_id);
    let retry_after = rejection.retry_after_secs().to_string();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after.as_str())],
        Json(envelope),
    )
        .into_response()
}

fn gate_reason(rejection: GateRejection) -> &'static str {
    match rejection {
        GateRejection::TurnInFlight => "turn_in_flight",
        GateRejection::UserInFlight => "user_in_flight",
        GateRejection::GlobalBusy => "global_busy",
    }
}

/// 生产任务：读上游 → 装配 → 写 channel；任务结束（含客户端断开）时取消上游并记账。
fn sse_stream(
    state: AppState,
    permit: TurnPermit,
    mut upstream: Upstream,
    request: ChatRequest,
    user_id: Uuid,
    request_id: String,
    heartbeat: Duration,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let (sender, receiver) = mpsc::channel::<Result<Bytes, Infallible>>(SSE_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        let model = request.model.clone();
        let mut assembler = ChunkAssembler::new(Channel::Plan, request_id.clone(), model.clone());

        'stream: loop {
            // 心跳也不能越过装配器：Meta 必须仍然是最先出现的事件
            let next = if heartbeat.is_zero() {
                upstream.next_chunks().await
            } else {
                match tokio::time::timeout(heartbeat, upstream.next_chunks()).await {
                    Ok(next) => next,
                    Err(_) => {
                        if !forward(&sender, assembler.feed(vec![Chunk::Ping])).await {
                            break 'stream;
                        }
                        continue;
                    }
                }
            };

            match next {
                Some(Ok(chunks)) => {
                    if !forward(&sender, assembler.feed(chunks)).await {
                        break 'stream; // 客户端断开
                    }
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        request_id = %request_id,
                        error = %error,
                        "上游流中断，转成 SSE error 事件"
                    );
                    // Error 是终止事件：装配器不会再补 Done（ADR-16）
                    let chunk = Chunk::Error {
                        code: error.code(),
                        message: error.message().to_owned(),
                    };
                    forward(&sender, assembler.feed(vec![chunk])).await;
                    break 'stream;
                }
                None => {
                    // 上游正常关闭但没给结束帧时，装配器补一个 Done
                    forward(&sender, assembler.finish()).await;
                    break 'stream;
                }
            }
        }

        // 取消上游：客户端断开时，这一步让厂商立刻停止生成（不继续烧 token）
        drop(upstream);

        // 抽样记账：**只写一行**，用装配器合并后的终值（按字段 max 合并）
        let usage: Usage = assembler.usage();
        if state.accounting.should_sample(&request.turn_id) {
            state.accounting.record_sample_detached(UsageSample {
                user_id,
                req_id: request_id.clone(),
                model,
                protocol: request.protocol,
                channel: Channel::Plan,
                round: request.round,
                usage,
            });
        }

        // permit 在这里释放：闸门名额与"单用户 1 个 turn"一起归还
        drop(permit);
        drop(sender);
    });

    SseBody { receiver }
}

/// 逐条写进 channel；返回 `false` 表示客户端已经断开（收端被 drop）。
async fn forward(sender: &mpsc::Sender<Result<Bytes, Infallible>>, chunks: Vec<Chunk>) -> bool {
    for chunk in chunks {
        if sender.send(Ok(encode_event(&chunk))).await.is_err() {
            return false;
        }
    }
    true
}

/// 响应体：把 channel 里的 SSE 字节转成 `Stream`（`Body::from_stream` 要的正是这个）。
struct SseBody {
    receiver: mpsc::Receiver<Result<Bytes, Infallible>>,
}

impl Stream for SseBody {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_match_the_sse_contract() {
        assert_eq!(
            event_name(&Chunk::Meta {
                request_id: "req_1".into(),
                model: "m".into(),
                channel: Channel::Plan
            }),
            "meta"
        );
        assert_eq!(event_name(&Chunk::Delta { text: "x".into() }), "delta");
        assert_eq!(
            event_name(&Chunk::Usage {
                tokens_in: 1,
                tokens_out: 2,
                estimated: false
            }),
            "usage"
        );
        assert_eq!(
            event_name(&Chunk::Done {
                finish: yunong_shared::FinishReason::Stop
            }),
            "done"
        );
        assert_eq!(
            event_name(&Chunk::Error {
                code: ErrorCode::UpstreamError,
                message: "x".into()
            }),
            "error"
        );
        assert_eq!(event_name(&Chunk::Ping), "ping");
    }

    #[test]
    fn wire_format_is_event_plus_data_plus_blank_line() {
        let bytes = encode_event(&Chunk::Delta {
            text: "你好".into(),
        });
        let text = String::from_utf8(bytes.to_vec()).expect("UTF-8");
        assert!(text.starts_with("event: delta\ndata: "), "{text}");
        assert!(text.ends_with("\n\n"), "事件之间必须空行分隔：{text:?}");
        assert!(
            text.contains("\"type\":\"delta\""),
            "wire 形状带 type 标签：{text}"
        );
    }

    #[test]
    fn every_gate_rejection_has_a_reason_code() {
        for rejection in [
            GateRejection::TurnInFlight,
            GateRejection::UserInFlight,
            GateRejection::GlobalBusy,
        ] {
            assert!(!gate_reason(rejection).is_empty());
        }
    }
}
