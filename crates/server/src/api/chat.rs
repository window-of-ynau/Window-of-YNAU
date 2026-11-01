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
//! 客户端断开时：响应体被丢弃 → `sender.closed()` 立刻就绪（`select!` 直接跳出读循环）
//! → **不再读上游**；`UpstreamStream` 随任务结束被 drop，上游的写入很快会被 TCP 窗口卡住
//! （不再有 token 被我们消费）。闸门名额与抽样记账都在任务收尾时处理。
//!
//! 说明：`reqwest` 在响应被 drop 后停止读取，但**不会主动发 FIN**——连接要等上游自己
//! 超时或写完才回收。对成本的影响是"厂商的写被背压住"，不是"立刻断开"。

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
use crate::upstream::{UpstreamError, UpstreamStream};

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

    // 红队 M4：标识字段的长度上限。**超限 422、不做截断**——静默截断会让审计/抽样里
    // 的 turn_id 与客户端后续轮次对不上（同一次提问被拆成两个 turn），排查时是灾难。
    for (field, value) in [
        ("session_id", request.session_id.as_str()),
        ("turn_id", request.turn_id.as_str()),
        ("model", request.model.as_str()),
    ] {
        if value.chars().count() > MAX_IDENT_CHARS {
            return Err(AppError::new(
                ErrorCode::ValidationError,
                format!("{field} 超过 {MAX_IDENT_CHARS} 个字符上限"),
                &req_id.0,
            ));
        }
    }

    // 红队 M5：输出 token 上限由服务端定，不允许客户端把 `max_output_tokens` 放大。
    if request.max_output_tokens > state.config.max_output_tokens_cap {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            format!(
                "max_output_tokens 不能超过 {}",
                state.config.max_output_tokens_cap
            ),
            &req_id.0,
        ));
    }

    // 服务端再守一道：配置可收紧上限，不信任客户端自报。
    // 红队 H2：**不信客户端自报**——取"客户端估值"与"服务端估值"的较大者。
    // 否则把 estimated_input_tokens 填 1，就能把接近 body 上限的正文送进上游。
    let server_estimate = estimate_input_tokens(&request);
    let admission_tokens = request.estimated_input_tokens.max(server_estimate);
    if admission_tokens > state.config.max_input_tokens {
        return Err(AppError::new(
            ErrorCode::MessageTooLarge,
            format!(
                "输入约 {} tokens，超过服务端上限 {}",
                admission_tokens, state.config.max_input_tokens
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

    // 2) 上游开流：连不上/超时/非 2xx/不是 SSE 都在这里变成 502/504 信封
    let upstream = match state.upstream.open(request.protocol, &request).await {
        Ok(upstream) => upstream,
        Err(error) => {
            // 细节（内网上游地址、缺哪个环境变量、上游响应片段）**只进日志**（评审 F3）
            tracing::warn!(
                request_id = %req_id.0,
                error = %error,
                code = error.code().as_str(),
                "上游开流失败"
            );
            // 审计：decision=error（放行了但执行失败），reason 只放短码，不放厂商原文
            state
                .accounting
                .record_audit_detached(AuditEntry::upstream_error(
                    principal.user_id,
                    req_id.0.clone(),
                    request.turn_id.clone(),
                    error.audit_reason(),
                ));
            return Err(AppError::new(
                error.code(),
                public_upstream_message(error.code()),
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
    let total_timeout = Duration::from_secs(state.config.stream_total_timeout_secs);
    let body = Body::from_stream(sse_stream(StreamArgs {
        state,
        permit,
        upstream,
        request,
        user_id: principal.user_id,
        request_id: req_id.0.clone(),
        heartbeat,
        total_timeout,
    }));

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

/// 标识类字段（session_id / turn_id / model）的字符数上限（红队 M4）。
const MAX_IDENT_CHARS: usize = 128;

/// 估算输出 token 时最多累计多少字符（只要够算比例，不必留全文）。
const MAX_ESTIMATE_CHARS: usize = 64 * 1024;

/// 上游故障对外的**固定文案**（评审 F3 / F9）。
///
/// 内网上游地址、缺哪个环境变量、上游响应片段、厂商错误原文这些细节只进 `tracing`，
/// **绝不进响应体**——客户端只需要"能不能重试"，运维靠日志。
fn public_upstream_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::UpstreamTimeout => "上游响应超时，请稍后重试",
        _ => "上游暂时不可用，请稍后重试",
    }
}

/// 把**来自上游帧**的 `Chunk::Error` 文案换成固定文案（评审 F9）。
///
/// 上游会在流里直接发错误对象（`{"error":{"message":"<厂商原文>"}}`），适配器把它原样
/// 塞进 `Chunk::Error.message`；如果照发，厂商的 org id、配额提示、内网地址就漏到客户端了。
///
/// 在**转发层统一清洗**（而不是试图在类型上区分来源）：
/// - `code` 保留（客户端要靠它决定重试策略）；
/// - 原文 `warn!`（运维在日志里看得见）；
/// - 我们自己在别处产生的 `Error`（装配器/闸门/校验）不经过这里，文案不受影响。
fn sanitize_upstream_errors(chunks: Vec<Chunk>, request_id: &str) -> Vec<Chunk> {
    chunks
        .into_iter()
        .map(|chunk| match chunk {
            Chunk::Error { code, message } => {
                tracing::warn!(
                    request_id = %request_id,
                    code = code.as_str(),
                    upstream_message = %message,
                    "上游在流中报了错：原文只进日志，对外换成固定文案"
                );
                Chunk::Error {
                    code,
                    message: public_upstream_message(code).to_owned(),
                }
            }
            other => other,
        })
        .collect()
}

/// 服务端自己的输入 token 估算（**准入与记账共用这一份**）。
///
/// 口径与端侧一致（`core::context::estimate_tokens`：CJK 1 字 ≈ 1 token，其它 4 字符 ≈ 1）。
/// 除消息正文外也算上工具声明的名字——工具描述同样进上游 prompt。
pub fn estimate_input_tokens(request: &ChatRequest) -> u32 {
    // 口径：**凡是会转发给上游的内容都要算**（security 复核 F2）。
    // 之前漏了工具描述/参数与历史 tool_calls 的参数——实测 700 KiB 的工具描述
    // 能整体绕过 32k 预算，上游实收 717 KB。
    let mut total: u32 = 0;
    for message in &request.messages {
        total = total.saturating_add(yunong_core::context::estimate_tokens(&message.content));
        for call in &message.tool_calls {
            total = total.saturating_add(yunong_core::context::estimate_tokens(&call.name));
            total = total.saturating_add(estimate_json_tokens(&call.arguments));
        }
    }
    for tool in &request.tools {
        total = total.saturating_add(yunong_core::context::estimate_tokens(&tool.name));
        total = total.saturating_add(yunong_core::context::estimate_tokens(&tool.description));
        total = total.saturating_add(estimate_json_tokens(&tool.parameters));
    }
    total
}

/// 估算一个 JSON 值序列化后的 token 数（工具参数 / JSON Schema 都会原样转发）。
fn estimate_json_tokens(value: &serde_json::Value) -> u32 {
    match serde_json::to_string(value) {
        Ok(text) => yunong_core::context::estimate_tokens(&text),
        // 极少数情况（NaN 等）序列化失败：宁可高估也不能漏算
        Err(_) => u32::MAX,
    }
}

/// 上游没给 `usage` 时的兜底估算，**与端侧同口径**
/// （[`yunong_core::context::estimate_tokens`]：CJK 1 字 ≈ 1 token，其它 4 字符 ≈ 1 token）。
///
/// 输入按每条消息分别估算再相加（不拼大字符串）；输出按已下发的增量文本估算。
/// 这是"区分未知与真 0"的最低成本做法：只标 `estimated=true` 不够，0/0 会让成本统计失真。
fn estimate_usage(request: &ChatRequest, emitted_text: &str) -> Usage {
    Usage {
        tokens_in: estimate_input_tokens(request),
        tokens_out: yunong_core::context::estimate_tokens(emitted_text),
        estimated: true,
    }
}

/// 生产任务：读上游 → 装配 → 写 channel；任务结束（含客户端断开）时取消上游并记账。
/// 一条流需要的运行参数（收成结构体，避免一长串位置参数）。
struct StreamArgs {
    state: AppState,
    permit: TurnPermit,
    upstream: UpstreamStream,
    request: ChatRequest,
    user_id: Uuid,
    request_id: String,
    heartbeat: Duration,
    total_timeout: Duration,
}

fn sse_stream(args: StreamArgs) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let StreamArgs {
        state,
        permit,
        upstream,
        request,
        user_id,
        request_id,
        heartbeat,
        total_timeout,
    } = args;
    let mut upstream = upstream;
    let (sender, receiver) = mpsc::channel::<Result<Bytes, Infallible>>(SSE_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        // 总时长守卫：`0` → 永不触发；否则用**绝对截止时间**。
        // 循环里每次重新 await 也没关系：`sleep_until(deadline)` 的截止点是绝对的，
        // 不会因为重新创建而"续命"。
        let total_deadline =
            (!total_timeout.is_zero()).then(|| tokio::time::Instant::now() + total_timeout);
        let mut total_timed_out = false;
        let model = request.model.clone();
        let mut assembler = ChunkAssembler::new(Channel::Plan, request_id.clone(), model.clone());
        // 记账用的两个观察量：上游有没有给过 usage；吐出过多长文本（估算出 token 用）
        let mut saw_usage = false;
        let mut emitted_text = String::new();

        // 客户端断开时收端被 drop，这个 future 立刻完成：我们立刻停止读上游
        // （随后 drop `UpstreamStream`，上游的写入会被 TCP 背压卡住）。
        // 作用域收在这里，是因为 `sender.closed()` 借用了 `sender`，
        // 后面的 `drop(sender)` 必须等这个借用结束。
        {
            let mut client_gone = std::pin::pin!(sender.closed());

            'stream: loop {
                // 心跳也不能越过装配器：Meta 必须仍然是最先出现的事件
                let outcome = tokio::select! {
                    // biased：优先取上游事件，避免"心跳与事件同时就绪"时把事件推迟一轮
                    biased;
                    outcome = read_next(&mut upstream, heartbeat) => outcome,
                    _ = &mut client_gone => break 'stream,
                    // 红队 M5：整条流的总时长上限（不是空闲上限）——"缓慢但一直有 token"
                    // 的流会把闸门名额长期占住，50 个账号就能让其他人全 429。
                    _ = wait_until(total_deadline) => {
                        total_timed_out = true;
                        break 'stream;
                    }
                };

                match outcome {
                    ReadOutcome::Ping => {
                        if !forward(&sender, assembler.feed(vec![Chunk::Ping])).await {
                            break 'stream;
                        }
                    }
                    ReadOutcome::Next(Some(Ok(chunks))) => {
                        let produced =
                            assembler.feed(sanitize_upstream_errors(chunks, &request_id));
                        for chunk in &produced {
                            match chunk {
                                Chunk::Usage { .. } => saw_usage = true,
                                Chunk::Delta { text }
                                    if emitted_text.len() < MAX_ESTIMATE_CHARS =>
                                {
                                    emitted_text.push_str(text);
                                }
                                Chunk::Error { code, .. } => {
                                    // 上游在流里报错（厂商错误帧）：同样是"执行失败"，
                                    // 记 decision=error，reason 只放短码（原文已进日志）。
                                    state.accounting.record_audit_detached(
                                        AuditEntry::upstream_error(
                                            user_id,
                                            request_id.clone(),
                                            request.turn_id.clone(),
                                            format!("upstream_error_frame_{}", code.as_str()),
                                        ),
                                    );
                                }
                                _ => {}
                            }
                        }
                        if !forward(&sender, produced).await {
                            break 'stream; // 客户端断开
                        }
                    }
                    ReadOutcome::Next(Some(Err(error))) => {
                        tracing::warn!(
                            request_id = %request_id,
                            error = %error,
                            "上游流中断，转成 SSE error 事件"
                        );
                        // 审计：流中断也是上游失败（decision=error），reason 只放短码
                        state
                            .accounting
                            .record_audit_detached(AuditEntry::upstream_error(
                                user_id,
                                request_id.clone(),
                                request.turn_id.clone(),
                                error.audit_reason(),
                            ));
                        // Error 是终止事件：装配器不会再补 Done（ADR-16）。
                        // 对外只给错误码 + 固定文案，细节留在上面的日志里（评审 F3）。
                        let chunk = Chunk::Error {
                            code: error.code(),
                            message: public_upstream_message(error.code()).to_owned(),
                        };
                        forward(&sender, assembler.feed(vec![chunk])).await;
                        break 'stream;
                    }
                    ReadOutcome::Next(None) => {
                        // 上游正常关闭但没给结束帧时，装配器补一个 Done
                        forward(&sender, assembler.finish()).await;
                        break 'stream;
                    }
                }
            }
        }

        // 红队 M5：总时长到点。按"上游超时"收尾：发终止 error 事件 + 记 error 审计，
        // 客户端看到的是和 504 同样的码与文案（它只需要知道"这条流不会再有内容了"）。
        if total_timed_out {
            tracing::warn!(
                request_id = %request_id,
                turn_id = %request.turn_id,
                seconds = total_timeout.as_secs(),
                "流总时长超限，主动终止"
            );
            state
                .accounting
                .record_audit_detached(AuditEntry::upstream_error(
                    user_id,
                    request_id.clone(),
                    request.turn_id.clone(),
                    "stream_total_timeout",
                ));
            let chunk = Chunk::Error {
                code: ErrorCode::UpstreamTimeout,
                message: public_upstream_message(ErrorCode::UpstreamTimeout).to_owned(),
            };
            let _ = forward(&sender, assembler.feed(vec![chunk])).await;
        }

        // 取消：客户端断开时我们早就不再读上游了，这一步把响应也释放掉。
        // （reqwest 不会主动发 FIN，但 socket 缓冲一旦被灌满，厂商的写就会被卡住。）
        drop(upstream);

        // 抽样记账：**每轮一行**（`(turn_id, round)` 幂等），写装配器合并后的终值。
        // 上游一次 usage 都没给时，用与端侧同口径的估算兜底并置 `estimated=true`（评审 F2）：
        // 否则库里只有 0/0，分不清"真的是 0"还是"没有数据"。
        let usage = if saw_usage {
            assembler.usage()
        } else {
            tracing::debug!(
                request_id = %request_id,
                turn_id = %request.turn_id,
                round = request.round,
                "上游没给 usage，落库用估算值（estimated=true）"
            );
            estimate_usage(&request, &emitted_text)
        };
        if state.accounting.should_sample(user_id, &request.turn_id) {
            state.accounting.record_sample_detached(UsageSample {
                turn_id: request.turn_id.clone(),
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

/// 读一步上游的结果。
enum ReadOutcome {
    /// 一步事件：`None` 表示上游正常结束
    Next(Option<Result<Vec<Chunk>, UpstreamError>>),
    /// 空闲太久：先发一条心跳，再继续等
    Ping,
}

/// 读一步上游事件；心跳间隔到了就先回去发 ping。
///
/// `UpstreamStream::next_chunks` 是取消安全的：被 `select!` 打断时已解出的事件还在
/// 它自己的缓冲里，不会丢事件。
async fn read_next(upstream: &mut UpstreamStream, heartbeat: Duration) -> ReadOutcome {
    if heartbeat.is_zero() {
        return ReadOutcome::Next(upstream.next_chunks().await);
    }
    match tokio::time::timeout(heartbeat, upstream.next_chunks()).await {
        Ok(next) => ReadOutcome::Next(next),
        Err(_) => ReadOutcome::Ping,
    }
}

/// 等到绝对截止时间；`None` 时永远 pending（用于"不限制总时长"）。
async fn wait_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
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
