//! 工具调用循环（C1）：无状态多轮 + 轮次上限 + tool-pair 完整性 + 工具超时与截断。
//!
//! # 定位（ADR-05）
//!
//! "无状态多轮"：每一轮都是一次**完整的新请求**（messages 追加），没有 WebSocket、没有预取。
//! 本模块只做编排：网络仍走 [`crate::gateway::ModelGateway`]，工具永远在**端侧进程内**执行
//! （ADR-01），服务端只转发。
//!
//! # 与 pi 的关系：只借鉴设计，不复制代码
//!
//! 设计参考见 `.internal/research/pi-evaluation.md` §2.2 / §6：
//!
//! - **截断即失败**：`finish_reason=length` 时这一轮的参数是半个 JSON，整批工具**不执行**；
//! - **abort 也补 `tool_result`**：取消/中断时给每个已收到的 `tool_call` 补一个失败结果，
//!   否则 tool-call / tool-result 不配对，下一次请求会被上游拒绝；
//! - **全 terminate 才收尾**：一批工具都是"终结型"时不再发起下一轮；
//! - **轮次上限必须自己加**：pi 的 agent-core 没有上限（上限只在它的 durable 包里）。
//!
//! **没有**照搬 pi 的 shell 工具 / 目录级 trust / 流式 JSON 宽松解析（见该报告 §5"绝对不要抄"）。
//!
//! # 状态机
//!
//! ```text
//!           ┌──────────────────┐
//!  开始 ───▶│ 组装第 N 轮请求     │◀─────────────────────────────────────┐
//!           └────────┬─────────┘                                      │
//!                    ▼                                                │
//!         ┌──────────────────────┐                                    │
//!         │ 流式读：Delta / ToolCall │──调用方 Flow::Stop──▶ 取消收尾   │
//!         └──────────┬───────────┘      （补全 tool_result，返回 cancelled）
//!                    ▼                                                │
//!      finish=Length 且有 tool_call ──▶ Err(Truncated)（整批不执行）    │
//!                    │                                                │
//!             无 tool_call ──▶ Ok(finish)                             │
//!                    │                                                │
//!             有 tool_call ──▶ 执行（单工具超时 / 输出尾部截断）         │
//!                    │          └─▶ 追加 assistant(tool_calls) + tool 消息
//!                    ▼                                                │
//!            全部 terminate？── 是 ──▶ Ok(Stop)                        │
//!                    └── 否 ──▶ 第 N+1 轮 ──────────────────────────────┘
//!                        （N+1 > max_rounds ──▶ Err(RoundLimit)）
//! ```
//!
//! # 已知简化点（C4 再补）
//!
//! 工具参数**不**做增量解析：按"收齐再解析"处理。上游把参数拆成多个
//! [`Chunk::ToolCall`] 片段时，这里按 `index` 累加字符串，收尾时整体 `serde_json::from_str`。
//! 解析不了的工具**不执行**，但仍然补一条失败 `tool_result`（配对优先）。
//!
//! 工具**串行**执行（pi 会并行）：端侧工具都是本地 SQLite 查询，串行的总耗时仍远小于一次
//! 网络往返；并行会引入 SQLite 并发写与"超时叠加"的复杂度，等真需要时再做。
//!
//! # 超时的代价（必须知道）
//!
//! 工具处理器是同步函数，Rust 里没有"中断正在跑的同步代码"的安全原语。超时实现是
//! **把句柄移进工作线程 + `recv_timeout`**：超时后线程被丢弃（detached），它的结果不会再用，
//! 但**它自己的副作用不会被回滚**。所以登记的工具必须是**纯计算/只读**（查课表、查记忆、
//! 算日程）；写操作要么幂等，要么由调用方在超时后再核对一次。

use std::collections::BTreeMap;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use serde_json::{Map, Value};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, ErrorCode, FinishReason, ToolCallRef, Usage};

use crate::error::{CoreError, CoreResult};
use crate::gateway::{Flow, ModelGateway};
use crate::tools::{ToolOutcome, ToolRegistry};

/// 默认轮次上限（**BYOK 直连**通道）。
///
/// pi 的 agent-core 没有上限（一次坏循环能烧光额度），所以这个数字是**我们自己定**的：
/// 24 轮 ≈ 23 次工具往返，覆盖"查课表 → 查记忆 → 算日程 → 复核"这类多跳任务，
/// 又不至于让一次绕圈把整晚的免费额度吃掉。超限**报错**而不是静默停。
///
/// **套餐通道不同**：服务端按 ADR-05 只接受 `round ≤ MAX_ROUNDS`（4 轮，防模型转圈烧钱，
/// 烧的是我们的钱）。套餐调用方必须把 [`LoopConfig::wire_max_rounds`] 设成
/// `Some(MAX_ROUNDS)`，循环会在第 5 轮**报错收尾**——绝不把轮次号压回 4：
/// 抽样记账的幂等键是 `(turn_id, round)`，压轮次会让第 5..N 轮反复覆盖第 4 轮的用量行。
pub const DEFAULT_MAX_ROUNDS: u8 = 24;

/// 单个工具的执行上限。
///
/// 端侧工具都是本地计算（SQLite 查询、日历换算），30s 已经远超正常耗时；
/// 它真正防的是"某个 handler 死循环"把整条回复挂住。
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// 单条工具输出保留的最大行数（尾部）。
pub const DEFAULT_MAX_OUTPUT_LINES: usize = 2_000;

/// 单条工具输出保留的最大字节数（尾部，64 KiB）。
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// 循环参数（默认值见各常量；测试与设置项可以按需改）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopConfig {
    /// 本地循环预算：最多发起几轮请求（默认 24）。
    pub max_rounds: u8,
    /// **通道上限**：走我们自己的套餐代理时，服务端契约只接受 `round ≤ MAX_ROUNDS`
    /// （ADR-05：最多 4 轮，防模型转圈烧钱），传 `Some(MAX_ROUNDS)`；
    /// BYOK 直连不经过我们的服务端，传 `None`（只受 `max_rounds` 约束）。
    ///
    /// **绝不把 `round` 压到上限**：服务端抽样记账的幂等键是 `(turn_id, round)`
    /// （ADR-05 约束 4），压轮次会让第 5..N 轮全部 upsert 到同一行、覆盖第 4 轮的用量，
    /// 记账系统性少计。所以到顶就**报错收尾**，不静默复用轮次号。
    pub wire_max_rounds: Option<u8>,
    pub tool_timeout: Duration,
    pub max_output_lines: usize,
    pub max_output_bytes: usize,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_rounds: DEFAULT_MAX_ROUNDS,
            // 默认按 BYOK 直连（不经过我们服务端）；套餐调用方必须显式设成 Some(MAX_ROUNDS)
            wire_max_rounds: None,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            max_output_lines: DEFAULT_MAX_OUTPUT_LINES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl LoopConfig {
    fn validate(&self) -> CoreResult<()> {
        if self.max_rounds == 0 {
            return Err(CoreError::Invalid("max_rounds 必须 ≥ 1".into()));
        }
        if self.wire_max_rounds == Some(0) {
            return Err(CoreError::Invalid("wire_max_rounds 必须 ≥ 1".into()));
        }
        if self.tool_timeout.is_zero() {
            return Err(CoreError::Invalid("tool_timeout 必须 > 0".into()));
        }
        if self.max_output_lines == 0 || self.max_output_bytes == 0 {
            return Err(CoreError::Invalid("工具输出的行数/字节上限必须 > 0".into()));
        }
        Ok(())
    }
}

/// 循环对外的归一化事件（**内部 API**，不进 wire 契约）。
///
/// 只在现有 [`Chunk`] 之上补工具调用的三段生命周期
/// （对齐 pi 的 `toolcall_start` / `toolcall_delta` / `toolcall_end`），
/// 不引入 pi 的 20+ 协议事件全集。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopEvent {
    /// 第 N 轮开始（每次发起请求前）
    RoundStarted { round: u8 },
    /// 可见文本增量（原样透传 `Chunk::Delta`）
    Delta { text: String },
    /// 工具调用开始（同一 `index` 只发一次）
    ToolCallStarted {
        index: u32,
        id: String,
        name: String,
    },
    /// 工具参数分片（未解析的原始字符串片段）
    ToolCallDelta { index: u32, fragment: String },
    /// 工具调用完成（参数已聚合）
    ToolCallCompleted {
        index: u32,
        id: String,
        name: String,
        arguments: Value,
    },
    /// 工具结果（`content` 已按上限截断）
    ToolResult {
        index: u32,
        id: String,
        name: String,
        ok: bool,
        truncated: bool,
        content: String,
    },
    /// 第 N 轮结束（`finish` 是上游给的结束原因）
    RoundFinished {
        round: u8,
        finish: FinishReason,
        usage: Usage,
    },
}

/// 一次工具循环的终值。
#[derive(Debug, Clone, PartialEq)]
pub struct LoopOutcome {
    pub finish: FinishReason,
    /// 实际发起的轮次（1 = 没用到工具）
    pub rounds: u8,
    /// 各轮 usage 之和（每轮都付一次输入token，所以是求和不是取 max）
    pub usage: Usage,
    /// 调用方中途取消（`Flow::Stop`）
    pub cancelled: bool,
    /// 各轮可见文本的拼接（UI 一般直接消费 `Delta` 事件）
    pub text: String,
    /// 本次循环**新增**的消息（assistant 工具调用 / tool 结果 / 最终回答），
    /// 可直接 append 到会话上下文
    pub messages: Vec<ChatMessage>,
}

/// 跑一次工具循环。
///
/// `request` 是**第一轮**的请求（system/user 已装配好，含 `tools` 声明）；
/// 后续轮次由本函数追加消息后重新发起。返回值里的 `messages` 只包含**新增**的消息。
pub async fn run<G: ModelGateway + ?Sized>(
    gateway: &G,
    registry: &ToolRegistry,
    request: ChatRequest,
    config: LoopConfig,
    on_event: &mut (dyn FnMut(LoopEvent) -> Flow + Send),
) -> CoreResult<LoopOutcome> {
    config.validate()?;

    let base_messages = request.messages.clone();
    let mut added: Vec<ChatMessage> = Vec::new();
    let mut total_usage = Usage::default();
    let mut all_text = String::new();
    let mut round: u8 = 0;

    let outcome = loop {
        if round >= config.max_rounds {
            return Err(CoreError::RoundLimit(format!(
                "工具循环达到 {} 轮上限仍未收敛（可能是问题过于复杂，或模型在原地绕圈）",
                config.max_rounds
            )));
        }
        // 套餐通道：服务端契约只接受 `round ≤ MAX_ROUNDS`（ADR-05：最多 4 轮）。
        // 到顶就**报错收尾**，不能把 `round` 压回上限——抽样记账的幂等键是
        // `(turn_id, round)`（ADR-05 约束 4），压轮次会让第 5..N 轮反复 upsert
        // 同一行、覆盖第 4 轮的用量，记账系统性少计，而我们是按轮计费的。
        if let Some(cap) = config.wire_max_rounds {
            if round >= cap {
                return Err(CoreError::RoundLimit(format!(
                    "套餐通道按 ADR-05 最多 {cap} 轮（已用满仍要调用工具）：改用 BYOK 直连，或把问题拆小"
                )));
            }
        }
        round += 1;
        // 调用方在"还没发请求"时就取消：直接收尾，省掉一次网络请求
        if on_event(LoopEvent::RoundStarted { round }) == Flow::Stop {
            break LoopOutcome {
                finish: FinishReason::Stop,
                rounds: round - 1,
                usage: total_usage,
                cancelled: true,
                text: all_text,
                messages: added,
            };
        }

        // ---- 组装本轮请求（无状态：每轮都是完整上下文） ----
        let mut round_request = request.clone();
        // 上报**真实**轮次：套餐通道的 `wire_max_rounds` 在上面的出口已经拦住超限，
        // 所以这里不会出现"第 5 轮报 4"这种会破坏 `(turn_id, round)` 幂等的写法。
        round_request.round = round;
        round_request.messages = base_messages.clone();
        round_request.messages.extend(added.iter().cloned());

        // ---- 流式读一轮 ----
        let mut turn_text = String::new();
        let mut finish: Option<FinishReason> = None;
        let mut upstream_error: Option<(ErrorCode, String)> = None;
        let mut calls = CallBuffer::default();
        let mut stop_requested = false;

        let stream_outcome = {
            let mut sink = |chunk: Chunk| -> Flow {
                match chunk {
                    Chunk::Delta { text } => {
                        turn_text.push_str(&text);
                        all_text.push_str(&text);
                        if on_event(LoopEvent::Delta { text }) == Flow::Stop {
                            stop_requested = true;
                            return Flow::Stop;
                        }
                        Flow::Continue
                    }
                    Chunk::ToolCall {
                        id,
                        name,
                        arguments,
                        index,
                    } => {
                        let mut flow = Flow::Continue;
                        for event in calls.absorb(id, name, arguments, index) {
                            if on_event(event) == Flow::Stop {
                                stop_requested = true;
                                flow = Flow::Stop;
                            }
                        }
                        flow
                    }
                    Chunk::Done { finish: reason } => {
                        finish = Some(reason);
                        Flow::Continue
                    }
                    Chunk::Error { code, message } => {
                        upstream_error = Some((code, message));
                        Flow::Continue
                    }
                    // Meta / Ping / Usage 由 gateway 的装配器处理，这里不重复记账
                    _ => Flow::Continue,
                }
            };
            gateway.chat_stream(&round_request, &mut sink).await?
        };

        total_usage.tokens_in = total_usage
            .tokens_in
            .saturating_add(stream_outcome.usage.tokens_in);
        total_usage.tokens_out = total_usage
            .tokens_out
            .saturating_add(stream_outcome.usage.tokens_out);
        total_usage.estimated |= stream_outcome.usage.estimated;

        let finish = finish.unwrap_or(stream_outcome.finish);
        if on_event(LoopEvent::RoundFinished {
            round,
            finish,
            usage: stream_outcome.usage,
        }) == Flow::Stop
        {
            stop_requested = true;
        }

        // ---- 取消：补全 tool-pair 后收尾 ----
        if stop_requested || stream_outcome.cancelled {
            let (final_calls, completed) = calls.into_calls();
            for event in completed {
                let _ = on_event(event);
            }
            append_cancelled(&mut added, &turn_text, &final_calls, on_event);
            break LoopOutcome {
                finish: FinishReason::Stop,
                rounds: round,
                usage: total_usage,
                cancelled: true,
                text: all_text,
                messages: added,
            };
        }

        // ---- 上游错误：原样上抛（不吞掉，也不当成"答完了"） ----
        if let Some((code, message)) = upstream_error {
            return Err(CoreError::Remote {
                code,
                message: truncate_chars(&message, 200),
                retry_after: None,
            });
        }
        // 网关以 error 收尾但没给错误 chunk（传输层判死等）：同样不能当成功返回，
        // 否则调用方只看 `Ok` 就会把一段空回答当答案。
        if finish == FinishReason::Error {
            return Err(CoreError::Remote {
                code: ErrorCode::UpstreamError,
                message: "上游以 error 结束，但没有给出错误详情".into(),
                retry_after: None,
            });
        }

        let (final_calls, completed) = calls.into_calls();
        for event in completed {
            if on_event(event) == Flow::Stop {
                stop_requested = true;
            }
        }

        // 参数分片发完时调用方要求停：不执行工具，但配对必须补齐
        if stop_requested {
            append_cancelled(&mut added, &turn_text, &final_calls, on_event);
            break LoopOutcome {
                finish: FinishReason::Stop,
                rounds: round,
                usage: total_usage,
                cancelled: true,
                text: all_text,
                messages: added,
            };
        }

        // ---- 截断：整批工具不执行 ----
        if finish == FinishReason::Length && !final_calls.is_empty() {
            return Err(CoreError::Truncated(format!(
                "本轮 {} 个工具调用因 max_tokens 截断而作废：参数只是半个 JSON，执行它等于执行一段错误指令",
                final_calls.len()
            )));
        }

        // ---- 没有工具调用：正常收尾 ----
        if final_calls.is_empty() {
            if !turn_text.is_empty() {
                added.push(ChatMessage::assistant(turn_text.clone()));
            }
            break LoopOutcome {
                finish,
                rounds: round,
                usage: total_usage,
                cancelled: stop_requested,
                text: all_text,
                messages: added,
            };
        }

        // ---- 执行工具（每个都补一条 tool_result，包括失败/超时/未知） ----
        let executed = execute_calls(registry, &final_calls, &config);
        let calls_ref: Vec<ToolCallRef> = executed
            .iter()
            .map(|item| {
                ToolCallRef::new(
                    item.call.id.clone(),
                    item.call.name.clone(),
                    item.call.arguments.clone(),
                )
            })
            .collect();
        added.push(ChatMessage::assistant_calls(turn_text.clone(), calls_ref));

        let mut all_terminate = true;
        for item in &executed {
            added.push(ChatMessage::tool_result(
                item.call.id.clone(),
                item.call.name.clone(),
                item.outcome.content.clone(),
            ));
            all_terminate &= item.outcome.terminate;
            if on_event(LoopEvent::ToolResult {
                index: item.call.index,
                id: item.call.id.clone(),
                name: item.call.name.clone(),
                ok: item.outcome.ok,
                truncated: item.truncated,
                content: item.outcome.content.clone(),
            }) == Flow::Stop
            {
                stop_requested = true;
            }
        }

        // 结果已经全部配对写入，这时收尾不会再断配对
        if stop_requested {
            break LoopOutcome {
                finish: FinishReason::Stop,
                rounds: round,
                usage: total_usage,
                cancelled: true,
                text: all_text,
                messages: added,
            };
        }

        // 整批都是"终结型"工具：不再发起下一轮
        if all_terminate {
            break LoopOutcome {
                finish: FinishReason::Stop,
                rounds: round,
                usage: total_usage,
                cancelled: false,
                text: all_text,
                messages: added,
            };
        }
    };

    Ok(outcome)
}

/// 取消时补全 tool-pair：每个已知的 `tool_call` 都必须有一条 `tool_result`。
fn append_cancelled(
    added: &mut Vec<ChatMessage>,
    turn_text: &str,
    calls: &[FinalCall],
    on_event: &mut (dyn FnMut(LoopEvent) -> Flow + Send),
) {
    if calls.is_empty() {
        if !turn_text.is_empty() {
            added.push(ChatMessage::assistant(turn_text.to_owned()));
        }
        return;
    }
    let calls_ref: Vec<ToolCallRef> = calls
        .iter()
        .map(|call| ToolCallRef::new(call.id.clone(), call.name.clone(), call.arguments.clone()))
        .collect();
    added.push(ChatMessage::assistant_calls(
        turn_text.to_owned(),
        calls_ref,
    ));
    for call in calls {
        let outcome = ToolOutcome::failure("已取消，未执行");
        added.push(ChatMessage::tool_result(
            call.id.clone(),
            call.name.clone(),
            outcome.content.clone(),
        ));
        let _ = on_event(LoopEvent::ToolResult {
            index: call.index,
            id: call.id.clone(),
            name: call.name.clone(),
            ok: false,
            truncated: false,
            content: outcome.content,
        });
    }
}

/// 一次工具执行的最终结果（含是否被截断）。
struct ExecutedCall {
    call: FinalCall,
    outcome: ToolOutcome,
    truncated: bool,
}

fn execute_calls(
    registry: &ToolRegistry,
    calls: &[FinalCall],
    config: &LoopConfig,
) -> Vec<ExecutedCall> {
    calls
        .iter()
        .map(|call| {
            let outcome = match &call.parse_error {
                // 参数不合法：不执行，但依然回填结果（配对优先）
                Some(reason) => ToolOutcome::failure(format!("未执行：{reason}")),
                None => run_tool(registry, call, config.tool_timeout),
            };
            let (content, truncated) = truncate_output(
                outcome.content,
                config.max_output_lines,
                config.max_output_bytes,
            );
            ExecutedCall {
                call: call.clone(),
                outcome: ToolOutcome { content, ..outcome },
                truncated,
            }
        })
        .collect()
}

/// 执行单个工具，带超时。
///
/// 同步 handler 没有可中断的原语，所以把 `Arc` 句柄移进工作线程、主线程 `recv_timeout`；
/// 超时后线程被丢弃（结果不再使用，副作用不回滚，见模块文档"超时的代价"）。
fn run_tool(registry: &ToolRegistry, call: &FinalCall, timeout: Duration) -> ToolOutcome {
    let Some(handler) = registry.handler(&call.name) else {
        return ToolOutcome::failure(format!("未知工具：{}", call.name));
    };
    let arguments = call.arguments.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("yunong-tool".into())
        .spawn(move || {
            let outcome = match handler(&arguments) {
                Ok(outcome) => outcome,
                Err(error) => ToolOutcome::failure(format!("工具执行失败：{error}")),
            };
            // 接收端可能已因超时走人，发送失败是正常情况
            let _ = sender.send(outcome);
        });
    if let Err(error) = spawned {
        return ToolOutcome::failure(format!("无法启动工具线程：{error}"));
    }
    match receiver.recv_timeout(timeout) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Timeout) => ToolOutcome::failure(format!(
            "工具执行超时（{}s），已放弃本次结果",
            timeout.as_secs_f32()
        )),
        Err(RecvTimeoutError::Disconnected) => ToolOutcome::failure("工具线程异常退出"),
    }
}

/// 保留**尾部**（最近的输出最有用），并加一行"已截断"标记。
///
/// 行数与字节双阈值：任一超限都截断。按字符边界切字节，不会切坏 UTF-8。
fn truncate_output(content: String, max_lines: usize, max_bytes: usize) -> (String, bool) {
    let total_lines = content.lines().count();
    let total_bytes = content.len();
    let too_many_lines = total_lines > max_lines;
    let too_many_bytes = total_bytes > max_bytes;
    if !too_many_lines && !too_many_bytes {
        return (content, false);
    }

    let mut tail = if too_many_lines {
        let skip = total_lines - max_lines;
        content.lines().skip(skip).collect::<Vec<_>>().join("\n")
    } else {
        content
    };
    if tail.len() > max_bytes {
        let mut cut = tail.len() - max_bytes;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = tail[cut..].to_owned();
    }
    let marker = format!(
        "…[输出已截断：共 {total_lines} 行 / {total_bytes} 字节，仅保留尾部最多 {max_lines} 行 / {max_bytes} 字节]\n"
    );
    (format!("{marker}{tail}"), true)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
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

/// 已收齐的一次调用（参数可能是 `{}` 或解析失败）。
#[derive(Debug, Clone, PartialEq)]
struct FinalCall {
    index: u32,
    id: String,
    name: String,
    /// 传给工具的参数：**永远是 JSON 对象**（解析失败时是 `{}`，原因见 `parse_error`）
    arguments: Value,
    /// 参数不合法时非 `None`：不执行，但依然回填 tool_result
    parse_error: Option<String>,
}

#[derive(Debug, Default)]
struct PendingCall {
    id: String,
    name: String,
    /// 参数分片（未解析的原始字符串）
    fragment: String,
    /// 上游一次性给了完整对象
    complete: Option<Value>,
    /// `ToolCallCompleted` 是否已经发过
    completed_emitted: bool,
}

/// 按 `index` 归位工具调用的参数分片。
///
/// 三协议都会把一次调用拆成多帧：Completions 给 `tool_calls[i].index`、
/// Anthropic 给 `content_block_start.index`、Responses 给 `output_index`（C9）。
/// 没有序号时按"续上一个"处理（上游极少这样发）。
#[derive(Debug, Default)]
struct CallBuffer {
    slots: BTreeMap<u32, PendingCall>,
    last_index: Option<u32>,
}

impl CallBuffer {
    fn absorb(
        &mut self,
        id: String,
        name: String,
        arguments: Value,
        index: Option<u32>,
    ) -> Vec<LoopEvent> {
        let index = index.or(self.last_index).unwrap_or(0);
        self.last_index = Some(index);
        let first_sight = !self.slots.contains_key(&index);
        let slot = self.slots.entry(index).or_default();
        if !id.is_empty() {
            slot.id = id;
        }
        if !name.is_empty() {
            slot.name = name;
        }

        let mut events = Vec::new();
        if first_sight {
            events.push(LoopEvent::ToolCallStarted {
                index,
                id: slot.id.clone(),
                name: slot.name.clone(),
            });
        }
        match arguments {
            Value::String(fragment) => {
                slot.fragment.push_str(&fragment);
                events.push(LoopEvent::ToolCallDelta { index, fragment });
            }
            // 起始帧的占位空对象（Anthropic 形状）：真实参数在后面的 delta 里
            Value::Object(map) if map.is_empty() && slot.complete.is_none() => {}
            other => {
                if !slot.completed_emitted {
                    slot.complete = Some(other.clone());
                    slot.completed_emitted = true;
                    events.push(LoopEvent::ToolCallCompleted {
                        index,
                        id: slot.id.clone(),
                        name: slot.name.clone(),
                        arguments: other,
                    });
                }
            }
        }
        events
    }

    /// 收尾：把每个槽位归一成可执行的调用，并补发还没发过的 `ToolCallCompleted`。
    fn into_calls(self) -> (Vec<FinalCall>, Vec<LoopEvent>) {
        let mut calls = Vec::with_capacity(self.slots.len());
        let mut events = Vec::new();
        for (index, slot) in self.slots {
            let PendingCall {
                id,
                name,
                fragment,
                complete,
                completed_emitted,
            } = slot;
            // id 缺失会导致 tool_result 对不上：给一个确定性兜底 id
            let id = if id.trim().is_empty() {
                format!("call_{index}")
            } else {
                id
            };
            let (arguments, parse_error) = match complete {
                Some(value) => normalize_arguments(value),
                None if fragment.trim().is_empty() => (Value::Object(Map::new()), None),
                None => match serde_json::from_str::<Value>(&fragment) {
                    Ok(value) => normalize_arguments(value),
                    Err(error) => (
                        Value::Object(Map::new()),
                        Some(format!("参数不是合法 JSON（{error}）")),
                    ),
                },
            };
            if !completed_emitted {
                events.push(LoopEvent::ToolCallCompleted {
                    index,
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
            }
            calls.push(FinalCall {
                index,
                id,
                name,
                arguments,
                parse_error,
            });
        }
        (calls, events)
    }
}

/// 参数必须是 JSON 对象（Anthropic 的 `input` 只收对象；Completions 收字符串但语义相同）。
/// 非对象一律降级成 `{}` 并记原因——**不**把半个 JSON 塞给上游（那会让整轮 400）。
fn normalize_arguments(value: Value) -> (Value, Option<String>) {
    if value.is_object() {
        (value, None)
    } else {
        (
            Value::Object(Map::new()),
            Some("参数不是 JSON 对象".to_owned()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use yunong_shared::{ErrorCode, Protocol, Role, ToolSpec};

    use crate::gateway::{GatewayFuture, StreamOutcome};

    // ------------------------------------------------------------------
    // 测试替身：脚本化的网关 + 可计数的工具
    // ------------------------------------------------------------------

    /// 按脚本逐轮吐出 chunk 的假网关（**不联网**）。
    #[derive(Default)]
    struct Script {
        turns: Mutex<VecDeque<Vec<Chunk>>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl Script {
        fn new(turns: Vec<Vec<Chunk>>) -> Arc<Self> {
            Arc::new(Self {
                turns: Mutex::new(turns.into()),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl ModelGateway for Script {
        fn chat_stream<'a>(
            &'a self,
            request: &'a ChatRequest,
            on_chunk: &'a mut (dyn FnMut(Chunk) -> Flow + Send),
        ) -> GatewayFuture<'a> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request.clone());
                let turn = self
                    .turns
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| vec![done(FinishReason::Stop)]);
                let mut usage = Usage::default();
                let mut finish = FinishReason::Stop;
                let mut cancelled = false;
                for chunk in turn {
                    match &chunk {
                        Chunk::Usage {
                            tokens_in,
                            tokens_out,
                            estimated,
                        } => {
                            usage.tokens_in = usage.tokens_in.max(*tokens_in);
                            usage.tokens_out = usage.tokens_out.max(*tokens_out);
                            usage.estimated |= *estimated;
                        }
                        Chunk::Done { finish: reason } => finish = *reason,
                        _ => {}
                    }
                    if on_chunk(chunk) == Flow::Stop {
                        cancelled = true;
                        break;
                    }
                }
                Ok(StreamOutcome {
                    usage,
                    finish,
                    cancelled,
                    request_id: None,
                })
            })
        }

        fn goes_through_cloud(&self) -> bool {
            false
        }
    }

    fn delta(text: &str) -> Chunk {
        Chunk::Delta { text: text.into() }
    }

    fn done(finish: FinishReason) -> Chunk {
        Chunk::Done { finish }
    }

    fn tool_call(index: u32, id: &str, name: &str, arguments: Value) -> Chunk {
        Chunk::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
            index: Some(index),
        }
    }

    fn usage_chunk(tokens_in: u32, tokens_out: u32) -> Chunk {
        Chunk::Usage {
            tokens_in,
            tokens_out,
            estimated: false,
        }
    }

    fn base_request() -> ChatRequest {
        ChatRequest {
            session_id: "s_1".into(),
            turn_id: "t_1".into(),
            round: 1,
            model: "deepseek-chat".into(),
            protocol: Protocol::OpenaiCompletions,
            messages: vec![
                ChatMessage::system("你是云农之窗助手"),
                ChatMessage::user("下周三下午有课吗"),
            ],
            tools: vec![ToolSpec {
                name: "get_courses".into(),
                description: "查询课表".into(),
                parameters: serde_json::json!({ "type": "object" }),
            }],
            max_output_tokens: 512,
            stream: true,
            estimated_input_tokens: 100,
        }
    }

    /// 注册一个会数调用次数的 `get_courses`。
    fn registry(calls: Arc<AtomicUsize>) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register("get_courses", move |args| {
            calls.fetch_add(1, Ordering::SeqCst);
            let weekday = args.get("weekday").and_then(Value::as_u64).unwrap_or(0);
            Ok(format!("周{weekday}：无课"))
        });
        registry
    }

    /// 记录事件、始终继续。
    fn recorder() -> (
        Arc<Mutex<Vec<LoopEvent>>>,
        impl FnMut(LoopEvent) -> Flow + Send,
    ) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        (log, move |event: LoopEvent| {
            sink.lock().unwrap().push(event);
            Flow::Continue
        })
    }

    // ------------------------------------------------------------------
    // 基本流程
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn zero_tool_calls_finishes_in_one_round() {
        let gateway = Script::new(vec![vec![delta("下周三没课"), done(FinishReason::Stop)]]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("应当成功");

        assert_eq!(outcome.rounds, 1);
        assert_eq!(outcome.finish, FinishReason::Stop);
        assert!(!outcome.cancelled);
        assert_eq!(outcome.text, "下周三没课");
        assert_eq!(outcome.messages.len(), 1, "最终回答要进 transcript");
        assert_eq!(outcome.messages[0].content, "下周三没课");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn single_tool_round_then_final_answer() {
        let gateway = Script::new(vec![
            vec![
                delta("我先查一下"),
                tool_call(0, "c1", "get_courses", serde_json::json!({ "weekday": 3 })),
                done(FinishReason::ToolCalls),
            ],
            vec![delta("周三下午没课"), done(FinishReason::Stop)],
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("应当成功");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.rounds, 2);
        assert_eq!(outcome.text, "我先查一下周三下午没课");

        // 第二轮请求必须带上 assistant(tool_calls) + tool 结果（无状态多轮）
        let requests = gateway.requests();
        assert_eq!(requests.len(), 2);
        let second = &requests[1];
        assert_eq!(second.round, 2);
        assert_eq!(second.messages.len(), 4);
        assert_eq!(second.messages[2].role, Role::Assistant);
        assert_eq!(second.messages[2].tool_calls.len(), 1);
        assert_eq!(second.messages[2].tool_calls[0].id, "c1");
        assert_eq!(second.messages[3].role, Role::Tool);
        assert_eq!(second.messages[3].tool_call_id.as_deref(), Some("c1"));
        assert!(second.messages[3].content.contains("周3"));

        // 新增消息：assistant 调用 + tool 结果 + 最终回答
        assert_eq!(outcome.messages.len(), 3);
    }

    #[tokio::test]
    async fn multiple_tool_rounds_are_supported() {
        let gateway = Script::new(vec![
            vec![
                tool_call(0, "c1", "get_courses", serde_json::json!({ "weekday": 3 })),
                done(FinishReason::ToolCalls),
            ],
            vec![
                tool_call(0, "c2", "get_courses", serde_json::json!({ "weekday": 4 })),
                done(FinishReason::ToolCalls),
            ],
            vec![delta("查完了"), done(FinishReason::Stop)],
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("应当成功");

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(outcome.rounds, 3);
        // 第三轮请求里应该有 5 条消息（system,user,2×(assistant+tool)）
        let requests = gateway.requests();
        assert_eq!(requests[2].messages.len(), 6);
    }

    #[tokio::test]
    async fn round_limit_is_reported_not_silently_stopped() {
        let gateway = Script::new(vec![
            vec![
                tool_call(0, "c1", "get_courses", serde_json::json!({})),
                done(FinishReason::ToolCalls),
            ],
            vec![
                tool_call(0, "c2", "get_courses", serde_json::json!({})),
                done(FinishReason::ToolCalls),
            ],
            vec![
                tool_call(0, "c3", "get_courses", serde_json::json!({})),
                done(FinishReason::ToolCalls),
            ],
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();
        let config = LoopConfig {
            max_rounds: 2,
            ..LoopConfig::default()
        };

        let error = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            config,
            &mut sink,
        )
        .await
        .expect_err("超过上限必须报错");

        assert!(matches!(error, CoreError::RoundLimit(_)), "{error:?}");
        assert!(error.to_string().contains('2'), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "只应跑到上限为止");
        assert_eq!(gateway.requests().len(), 2);
    }

    // ------------------------------------------------------------------
    // 截断 / 取消 / 完整性
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn length_stop_skips_the_whole_tool_batch() {
        let gateway = Script::new(vec![
            vec![
                tool_call(0, "c1", "get_courses", serde_json::json!({ "weekday": 3 })),
                tool_call(1, "c2", "get_courses", serde_json::json!({ "weekday": 4 })),
                done(FinishReason::Length),
            ],
            // 这条不该被消费：截断后不允许再发下一轮
            vec![delta("不该出现"), done(FinishReason::Stop)],
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();

        let error = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect_err("截断时必须报错");

        assert!(matches!(error, CoreError::Truncated(_)), "{error:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "整批工具都不能执行");
        assert_eq!(gateway.requests().len(), 1, "截断后不应再发请求");
    }

    #[tokio::test]
    async fn abort_keeps_tool_call_and_result_paired() {
        let gateway = Script::new(vec![vec![
            tool_call(0, "c1", "get_courses", serde_json::json!({ "weekday": 3 })),
            tool_call(1, "c2", "get_courses", serde_json::json!({ "weekday": 4 })),
            done(FinishReason::ToolCalls),
        ]]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));

        // 收到第二个工具调用开始就取消（此时两个调用都已知道）
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let mut sink = move |event: LoopEvent| -> Flow {
            if matches!(event, LoopEvent::ToolCallStarted { .. })
                && counter.fetch_add(1, Ordering::SeqCst) == 1
            {
                return Flow::Stop;
            }
            Flow::Continue
        };

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("取消不是错误");

        assert!(outcome.cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "取消后不应执行工具");
        assert_eq!(outcome.messages.len(), 3, "1 条 assistant + 2 条 tool 结果");
        let assistant = &outcome.messages[0];
        assert_eq!(assistant.role, Role::Assistant);
        assert_eq!(assistant.tool_calls.len(), 2);
        for (index, call) in assistant.tool_calls.iter().enumerate() {
            let result = &outcome.messages[index + 1];
            assert_eq!(result.role, Role::Tool);
            assert_eq!(
                result.tool_call_id.as_deref(),
                Some(call.id.as_str()),
                "每个 tool_call 都要有配对的 tool_result"
            );
            assert!(result.content.contains("取消"), "{}", result.content);
        }
    }

    #[tokio::test]
    async fn abort_before_the_request_is_made_skips_the_network_call() {
        let gateway = Script::new(vec![vec![delta("不该出现"), done(FinishReason::Stop)]]);
        let registry = ToolRegistry::new();
        let mut sink = |event: LoopEvent| -> Flow {
            if matches!(event, LoopEvent::RoundStarted { .. }) {
                Flow::Stop
            } else {
                Flow::Continue
            }
        };

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("取消不是错误");

        assert!(outcome.cancelled);
        assert_eq!(outcome.rounds, 0);
        assert!(outcome.messages.is_empty());
        assert_eq!(gateway.requests().len(), 0, "取消后不应再发请求");
    }

    // ------------------------------------------------------------------
    // 工具超时 / 未知工具 / 截断
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn tool_timeout_produces_an_error_result_and_the_loop_finishes() {
        let gateway = Script::new(vec![
            vec![
                tool_call(0, "c1", "slow", serde_json::json!({})),
                done(FinishReason::ToolCalls),
            ],
            vec![delta("工具超时了，先这样"), done(FinishReason::Stop)],
        ]);
        let mut registry = ToolRegistry::new();
        registry.register("slow", |_| {
            std::thread::sleep(Duration::from_millis(300));
            Ok("终于好了".into())
        });
        let (log, mut sink) = recorder();
        let config = LoopConfig {
            tool_timeout: Duration::from_millis(20),
            ..LoopConfig::default()
        };

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            config,
            &mut sink,
        )
        .await
        .expect("超时不能把整条流挂死");

        assert_eq!(outcome.rounds, 2);
        let results: Vec<_> = log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                LoopEvent::ToolResult {
                    ok,
                    content,
                    truncated,
                    ..
                } => Some((*ok, content.clone(), *truncated)),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 1);
        assert!(!results[0].0);
        assert!(results[0].1.contains("超时"), "{}", results[0].1);
        assert!(!results[0].2);
        // 配对仍在：tool 消息带同样的 id
        let tool_message = outcome
            .messages
            .iter()
            .find(|message| message.role == Role::Tool)
            .expect("必须有 tool 结果");
        assert_eq!(tool_message.tool_call_id.as_deref(), Some("c1"));
    }

    #[tokio::test]
    async fn unknown_tool_is_a_failure_result_with_pairing_intact() {
        let gateway = Script::new(vec![
            vec![
                tool_call(0, "c1", "shell", serde_json::json!({ "cmd": "rm -rf /" })),
                done(FinishReason::ToolCalls),
            ],
            vec![delta("这个能力不存在"), done(FinishReason::Stop)],
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("未知工具不该让整轮失败");

        let tool_message = &outcome.messages[1];
        assert_eq!(tool_message.role, Role::Tool);
        assert!(
            tool_message.content.contains("未知工具"),
            "{}",
            tool_message.content
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn long_output_is_truncated_by_line_and_byte_thresholds() {
        // 行数超限
        let many_lines = (1..=5)
            .map(|index| format!("第 {index} 行"))
            .collect::<Vec<_>>()
            .join("\n");
        let (text, truncated) = truncate_output(many_lines.clone(), 2, 1024);
        assert!(truncated);
        assert!(text.contains("输出已截断"), "{text}");
        assert!(text.ends_with("第 4 行\n第 5 行"), "只保留尾部：{text}");
        assert!(!text.contains("第 1 行"));

        // 字节超限（中文按字符边界切，不会切坏 UTF-8）
        let long = "一".repeat(50); // 150 字节
        let (text, truncated) = truncate_output(long.clone(), 2000, 30);
        assert!(truncated);
        assert!(text.contains("输出已截断"));
        assert!(text.ends_with(&"一".repeat(10)), "{text}");

        // 都没超限：原样返回
        let (text, truncated) = truncate_output("短输出".into(), 2000, 1024);
        assert!(!truncated);
        assert_eq!(text, "短输出");
    }

    #[tokio::test]
    async fn tool_result_event_carries_the_truncation_flag() {
        let gateway = Script::new(vec![
            vec![
                tool_call(0, "c1", "chatty", serde_json::json!({})),
                done(FinishReason::ToolCalls),
            ],
            vec![delta("好"), done(FinishReason::Stop)],
        ]);
        let mut registry = ToolRegistry::new();
        registry.register("chatty", |_| {
            Ok((1..=50)
                .map(|index| format!("第 {index} 行"))
                .collect::<Vec<_>>()
                .join("\n"))
        });
        let (log, mut sink) = recorder();
        let config = LoopConfig {
            max_output_lines: 3,
            ..LoopConfig::default()
        };

        let _ = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            config,
            &mut sink,
        )
        .await
        .expect("应当成功");

        let result = log
            .lock()
            .unwrap()
            .iter()
            .find_map(|event| match event {
                LoopEvent::ToolResult {
                    truncated, content, ..
                } => Some((*truncated, content.clone())),
                _ => None,
            })
            .expect("必须有 ToolResult 事件");
        assert!(result.0, "截断必须标记出来");
        assert!(result.1.ends_with("第 49 行\n第 50 行"), "{}", result.1);
    }

    // ------------------------------------------------------------------
    // 参数聚合与非法参数
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn streamed_argument_fragments_are_aggregated() {
        let gateway = Script::new(vec![
            vec![
                // 起始帧：id/name + 占位空对象
                tool_call(0, "c1", "get_courses", serde_json::json!({})),
                // 分片：{"weekday": + 3}
                tool_call(0, "", "", Value::String(r#"{"weekday":"#.into())),
                tool_call(0, "", "", Value::String("3}".into())),
                done(FinishReason::ToolCalls),
            ],
            vec![delta("周三没课"), done(FinishReason::Stop)],
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (log, mut sink) = recorder();

        let _ = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("应当成功");

        assert_eq!(calls.load(Ordering::SeqCst), 1, "聚合后应执行一次");
        let events = log.lock().unwrap().clone();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, LoopEvent::ToolCallStarted { .. }))
                .count(),
            1,
            "started 只能发一次：{events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, LoopEvent::ToolCallDelta { .. }))
                .count(),
            2,
            "两片 delta：{events:?}"
        );
        let completed: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                LoopEvent::ToolCallCompleted { arguments, .. } => Some(arguments.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(completed.len(), 1, "completed 只能发一次：{events:?}");
        assert_eq!(completed[0], serde_json::json!({ "weekday": 3 }));
    }

    #[tokio::test]
    async fn unparsable_arguments_are_not_executed_but_still_paired() {
        let gateway = Script::new(vec![
            vec![
                tool_call(0, "c1", "get_courses", Value::String("{oops".into())),
                done(FinishReason::ToolCalls),
            ],
            vec![delta("参数坏了"), done(FinishReason::Stop)],
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("参数坏不该让整轮失败");

        assert_eq!(calls.load(Ordering::SeqCst), 0, "参数不合法不能执行工具");
        assert_eq!(outcome.messages[0].tool_calls.len(), 1);
        assert_eq!(
            outcome.messages[0].tool_calls[0].arguments,
            serde_json::json!({}),
            "非法参数在上游请求里必须降级成对象（否则 Anthropic 直接 400）"
        );
        let tool_message = &outcome.messages[1];
        assert_eq!(tool_message.tool_call_id.as_deref(), Some("c1"));
        assert!(
            tool_message.content.contains("未执行"),
            "{}",
            tool_message.content
        );
    }

    // ------------------------------------------------------------------
    // 契约细节
    // ------------------------------------------------------------------

    /// 一轮都不收敛的脚本：每轮都请求工具。
    fn always_tool_turns(count: usize) -> Vec<Vec<Chunk>> {
        (0..count)
            .map(|index| {
                vec![
                    tool_call(
                        0,
                        &format!("c{index}"),
                        "get_courses",
                        serde_json::json!({ "weekday": 1 }),
                    ),
                    done(FinishReason::ToolCalls),
                ]
            })
            .collect()
    }

    #[tokio::test]
    async fn proxy_channel_stops_at_the_contract_round_cap_without_reusing_round_numbers() {
        // 套餐通道：服务端只接受 round ≤ MAX_ROUNDS（ADR-05，4 轮）。
        // 第 5 轮必须**报错收尾**，且上报的轮次序列必须是 [1,2,3,4]——
        // 绝不能是 [1,2,3,4,4]（那会让 (turn_id, round) 幂等键覆盖第 4 轮的用量行）。
        let gateway = Script::new(always_tool_turns(6));
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();
        let config = LoopConfig {
            max_rounds: 6,
            wire_max_rounds: Some(yunong_shared::MAX_ROUNDS),
            ..LoopConfig::default()
        };

        let error = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            config,
            &mut sink,
        )
        .await
        .expect_err("套餐通道第 5 轮必须报错收尾");
        assert!(matches!(error, CoreError::RoundLimit(_)), "{error:?}");
        assert!(
            error.to_string().contains("套餐"),
            "错误信息要说清是哪条通道的上限：{error}"
        );

        let rounds: Vec<u8> = gateway
            .requests()
            .iter()
            .map(|request| request.round)
            .collect();
        assert_eq!(
            rounds,
            vec![1, 2, 3, 4],
            "绝不能复用 round=4 上报第 5 轮（会破坏抽样记账的幂等键）"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 4, "只执行前 4 轮的工具");
    }

    #[tokio::test]
    async fn byok_channel_reports_real_round_numbers_up_to_the_local_budget() {
        // BYOK 直连不经我们服务端：只受本地预算约束，上报真实轮次（不夹取）。
        let gateway = Script::new(always_tool_turns(6));
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();
        let config = LoopConfig {
            max_rounds: 6,
            wire_max_rounds: None,
            ..LoopConfig::default()
        };

        let error = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            config,
            &mut sink,
        )
        .await
        .expect_err("本地预算用满 → 报错");
        assert!(matches!(error, CoreError::RoundLimit(_)), "{error:?}");

        let rounds: Vec<u8> = gateway
            .requests()
            .iter()
            .map(|request| request.round)
            .collect();
        assert_eq!(rounds, vec![1, 2, 3, 4, 5, 6], "BYOK 上报真实轮次");
    }

    #[tokio::test]
    async fn usage_is_summed_across_rounds() {
        let gateway = Script::new(vec![
            vec![
                usage_chunk(10, 5),
                tool_call(0, "c1", "get_courses", serde_json::json!({})),
                done(FinishReason::ToolCalls),
            ],
            vec![usage_chunk(12, 3), delta("好了"), done(FinishReason::Stop)],
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("应当成功");

        assert_eq!(outcome.usage.tokens_in, 22);
        assert_eq!(outcome.usage.tokens_out, 8);
        assert!(!outcome.usage.estimated);
    }

    #[tokio::test]
    async fn upstream_error_is_propagated_as_remote_error() {
        let gateway = Script::new(vec![vec![Chunk::Error {
            code: ErrorCode::RateLimited,
            message: "slow down".into(),
        }]]);
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry(Arc::clone(&calls));
        let (_log, mut sink) = recorder();

        let error = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect_err("上游错误要上抛");

        assert!(
            matches!(error, CoreError::Remote { code, .. } if code == ErrorCode::RateLimited),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn error_finish_without_detail_is_not_treated_as_success() {
        let gateway = Script::new(vec![vec![done(FinishReason::Error)]]);
        let registry = ToolRegistry::new();
        let (_log, mut sink) = recorder();

        let error = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect_err("以 error 收尾却不能当成功");

        assert!(
            matches!(
                error,
                CoreError::Remote {
                    code: ErrorCode::UpstreamError,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn config_is_validated() {
        let gateway = Script::new(vec![]);
        let registry = ToolRegistry::new();
        let (_log, mut sink) = recorder();
        for config in [
            LoopConfig {
                max_rounds: 0,
                ..LoopConfig::default()
            },
            LoopConfig {
                wire_max_rounds: Some(0),
                ..LoopConfig::default()
            },
            LoopConfig {
                tool_timeout: Duration::ZERO,
                ..LoopConfig::default()
            },
            LoopConfig {
                max_output_lines: 0,
                ..LoopConfig::default()
            },
            LoopConfig {
                max_output_bytes: 0,
                ..LoopConfig::default()
            },
        ] {
            let error = run(
                gateway.as_ref(),
                &registry,
                base_request(),
                config,
                &mut sink,
            )
            .await
            .expect_err("非法配置必须拒绝");
            assert!(matches!(error, CoreError::Invalid(_)), "{error:?}");
        }
    }

    #[tokio::test]
    async fn all_terminate_tools_finish_without_another_round() {
        // 脚本只给一轮：如果循环还要发第二轮，假网关会回一个 Done{Stop} 空轮，
        // 断言 requests().len() == 1 就能证明它没有继续。
        let gateway = Script::new(vec![vec![
            tool_call(0, "c1", "finish", serde_json::json!({})),
            done(FinishReason::ToolCalls),
        ]]);
        let mut registry = ToolRegistry::new();
        registry.register_outcome("finish", |_| {
            Ok(ToolOutcome::success("回答完毕").with_terminate())
        });
        let (_log, mut sink) = recorder();

        let outcome = run(
            gateway.as_ref(),
            &registry,
            base_request(),
            LoopConfig::default(),
            &mut sink,
        )
        .await
        .expect("应当成功");

        assert_eq!(outcome.rounds, 1, "全 terminate 时不应再发起下一轮");
        assert_eq!(outcome.finish, FinishReason::Stop);
        assert_eq!(gateway.requests().len(), 1);
        assert_eq!(outcome.messages.len(), 2, "assistant + tool 结果");
    }
}
