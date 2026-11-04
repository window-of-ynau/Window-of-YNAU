//! 流式对话的 FFI 支撑层（W7 P0）：配置解析 → 跑一次 `chat_stream` → 每个 Chunk 序列化回调。
//!
//! # 为什么需要这一层
//!
//! `gateway` 是纯 Rust 的流式 trait，Dart 侧要"边生成边显示"，必须走**回调**而不是
//! "调用→返回 JSON"。FFI 函数本体在 `ffi.rs`，这里放可单测的逻辑：
//! - [`ChatRunConfig`]：Dart 传的 JSON 配置（通道 / 协议 / 模型 / Key / 消息）；
//! # ⚠️ 回调的所有权（T82，最容易踩的一条）
//!
//! 每条 chunk 的 JSON 由 Rust `CString::into_raw` 分配，**所有权随回调转给调用方**：
//! 调用方读完必须 `yunong_free_string(ptr)`。原因见 `ffi::ChatChunkCallback`：
//! `NativeCallable.listener` 是**异步投递**，回调返回时数据还没被读，Rust 若在返回时释放
//! 就是悬垂指针（现象：Dart 侧 `FormatException: Missing extension byte`）。
//!
//! - [`run_stream`]：搭运行时、选通道、把 Chunk 交给回调；流中的错误**以 `Chunk::Error` 输出**
//!   （不是异常、不是 FFI 返回值），这样 Dart 侧永远只有一条流；
//! - [`CancelRegistry`]：`yunong_chat_cancel(handle)` 的取消登记表。
//!
//! # 安全约定（与 `ffi.rs` 对齐）
//!
//! 回调里的字符串**只在回调期间有效**，由 Rust 分配与释放；Dart 侧绝对不能 free。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Deserialize;
use yunong_shared::{
    Channel, ChatMessage, ChatRequest, Chunk, ErrorCode, FinishReason, Protocol, Usage,
};

use crate::gateway::ModelGateway;

/// Dart 侧传进来的运行配置。
#[derive(Debug, Clone, Deserialize)]
pub struct ChatRunConfig {
    /// `byok`（直连厂商）或 `cloud`（套餐代理）
    #[serde(default = "default_channel")]
    pub channel: String,
    /// `openai_completions` / `openai_responses` / `anthropic_messages`
    #[serde(default = "default_protocol")]
    pub protocol: String,
    pub model: String,
    /// BYOK 的厂商 base url（缺省按协议给默认值）
    #[serde(default)]
    pub base_url: Option<String>,
    /// BYOK 的 API Key（**仅测试/命令行**用；App 应传 `secret_key` 让明文留在 Rust）
    #[serde(default)]
    pub api_key: Option<String>,
    /// 凭据在平台安全存储里的 key 名（如 `byok_api_key`）：由 Rust 自己取明文，Dart 不见明文
    #[serde(default)]
    pub secret_key: Option<String>,
    /// 应用数据目录（用于定位 secret store；缺省用当前目录）
    #[serde(default)]
    pub data_dir: Option<String>,
    /// 本次请求的「工具选择」：`auto`（默认）/ `none` / `required`（t124）。
    ///
    /// **只对这一次提问生效**，不是持久配置；工具循环的第 2 轮起自动退回 auto。
    #[serde(default)]
    pub tool_choice: Option<String>,
    /// 套餐通道的 access token
    #[serde(default)]
    pub cloud_token: Option<String>,
    #[serde(default)]
    pub cloud_base_url: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    /// 本地数据库路径（挂端侧内置工具用；空 = 不挂工具，行为与之前完全一致）
    #[serde(default)]
    pub db_path: Option<String>,
    /// 单工具超时（毫秒；空 = 默认 10s）——测试与联调需要更短的超时
    #[serde(default)]
    pub tool_timeout_ms: Option<u64>,
}

fn default_channel() -> String {
    "byok".to_owned()
}

fn default_protocol() -> String {
    "openai_completions".to_owned()
}

impl ChatRunConfig {
    /// 解析 + 结构校验（错误码直接给 UI 用：`MISSING_API_KEY` / `MISSING_ACCESS_TOKEN` / `BAD_CONFIG`）。
    pub fn parse(json: &str) -> Result<Self, (ErrorCode, String)> {
        let config: Self = serde_json::from_str(json).map_err(|err| {
            (
                ErrorCode::ValidationError,
                format!("配置不是合法 JSON：{err}"),
            )
        })?;
        if config.model.trim().is_empty() {
            return Err((ErrorCode::ValidationError, "model 不能为空".into()));
        }
        if config.messages.is_empty() {
            return Err((ErrorCode::ValidationError, "messages 不能为空".into()));
        }
        match config.channel.as_str() {
            "byok" => {
                // 只要给了 secret_key，明文由 Rust 从安全存储取，不要求 Dart 传 Key
                if config.secret_key.is_some() {
                    return Ok(config);
                }
                if config
                    .api_key
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
                {
                    return Err((
                        ErrorCode::Unauthorized,
                        "还没填 API Key：请在「设置」里填入你的密钥".into(),
                    ));
                }
            }
            "cloud" => {
                if config
                    .cloud_token
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
                {
                    return Err((
                        ErrorCode::Unauthorized,
                        "还没登录套餐账号：请先在设置里填写套餐访问令牌".into(),
                    ));
                }
            }
            other => {
                return Err((
                    ErrorCode::ValidationError,
                    format!("未知通道：{other}（只能是 byok 或 cloud）"),
                ));
            }
        }
        Ok(config)
    }

    /// 组装契约层的 [`ChatRequest`]。
    pub fn to_request(&self) -> ChatRequest {
        ChatRequest {
            session_id: self
                .session_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            turn_id: self
                .turn_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            round: 1,
            model: self.model.clone(),
            protocol: parse_protocol(&self.protocol),
            messages: self.messages.clone(),
            // t113：**必须把工具广告给上游**，否则模型不知道端侧能查课表/记忆
            //（事故现场：问「第 8 周哪天最累」，模型回"我没有你的课表上下文"）
            tools: if self.db_path.is_some() {
                crate::tools::builtin_specs()
            } else {
                Vec::new()
            },
            max_output_tokens: self.max_output_tokens.unwrap_or(1024),
            stream: true,
            estimated_input_tokens: 0,
        }
    }
}

/// 协议字符串 → 枚举（未知值退回 Completions，最稳）。
pub fn parse_protocol(value: &str) -> Protocol {
    match value {
        "openai_responses" => Protocol::OpenaiResponses,
        "anthropic_messages" => Protocol::AnthropicMessages,
        _ => Protocol::OpenaiCompletions,
    }
}

/// 把内部错误包装成一条 `Chunk::Error` 的 JSON（Dart 侧只处理一种流）。
pub fn error_chunk_json(code: ErrorCode, message: impl Into<String>) -> String {
    let message = message.into();
    // 消息截断：错误串不给 UI 展示原文，但日志/诊断需要上限
    let message: String = message.chars().take(200).collect();
    serde_json::to_string(&Chunk::Error { code, message }).unwrap_or_else(|_| {
        r#"{"type":"error","code":"internal","message":"序列化失败"}"#.to_owned()
    })
}

/// 取消登记表：`handle` 由 FFI 分配，取消后回调返回 false 让流停下。
#[derive(Debug, Default)]
pub struct CancelRegistry {
    next: AtomicU64,
    cancelled: Mutex<HashSet<u64>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 分配一个新 handle（1 起，0 保留给"无效"）。
    pub fn allocate(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// 登记取消；返回是否确实存在（不存在说明流已结束，不算错）。
    pub fn cancel(&self, handle: u64) -> bool {
        let mut guard = match self.cancelled.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert(handle)
    }

    pub fn is_cancelled(&self, handle: u64) -> bool {
        let guard = match self.cancelled.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.contains(&handle)
    }

    /// 流结束后清理登记，避免长期占内存。
    pub fn release(&self, handle: u64) {
        let mut guard = match self.cancelled.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.remove(&handle);
    }
}

/// 把闭包包成 `Send`：gateway 的 `on_chunk` 要求 `Send`，而我们只在**当前线程**的
/// current-thread runtime 里调用它（不会跨线程），所以这个包装是安全的。
struct SendableClosure<F>(F);

// 安全说明：`run_stream` 内部只在创建它的线程上 `block_on`，闭包不会真的跨线程移动。
#[allow(unsafe_code)]
unsafe impl<F> Send for SendableClosure<F> {}

impl<F: FnMut(String) -> bool> SendableClosure<F> {
    /// 用方法调用而不是 `callback.0(...)`：edition 2021 的精确捕获会拆字段，
    /// 那样闭包捕获的是 `F`（非 Send），包装就白做了。
    fn call(&mut self, chunk: String) -> bool {
        (self.0)(chunk)
    }
}

/// 跑一次流：每个 chunk 交给 `on_chunk`（返回 false = 调用方要求取消）。
///
/// 返回的 `Err` **只表示"没能开始"**（配置错误、运行时没建起来）；一旦开始，
/// 流内错误一律以 `Chunk::Error` 交给回调。
///
/// # 工具事件（t103）
///
/// `config.db_path` 非空时会挂上端侧内置工具（见 [`crate::tools::builtin_registry`]），
/// 模型发起的工具调用以 `{"type":"tool_call","call_id","name","args"}`、
/// 端侧执行结果以 `{"type":"tool_result","call_id","ok","summary","code?"}` 出现在同一条流里
/// （形状见 [`crate::tool_events`]）。
///
/// **任何退出路径都不会留下悬空的 `tool_call`**：取消 / 超时 / 轮次上限 / 上游错误
/// 都会补一条失败的 `tool_result`（[`crate::tool_events::PairGuard`] 兜底）。
pub fn run_stream(
    config: ChatRunConfig,
    on_chunk: impl FnMut(String) -> bool,
) -> Result<(), (ErrorCode, String)> {
    let registry = crate::tools::builtin_registry(config.db_path.clone());
    run_stream_with_registry(config, registry, on_chunk)
}

/// 同 [`run_stream`]，但用调用方给的注册表（联调/测试用桩工具）。
pub fn run_stream_with_registry(
    config: ChatRunConfig,
    registry: crate::tools::ToolRegistry,
    on_chunk: impl FnMut(String) -> bool,
) -> Result<(), (ErrorCode, String)> {
    // BYOK + secret_key：明文只在 Rust 内部取一次，绝不经过 Dart
    let api_key = match (config.secret_key.as_deref(), config.api_key.clone()) {
        (Some(name), _) => {
            let base = config.data_dir.clone().unwrap_or_else(|| ".".to_owned());
            let store = crate::secret::default_store(&base);
            match store.load_raw(name) {
                Ok(Some(value)) => value,
                Ok(None) => {
                    return Err((
                        ErrorCode::Unauthorized,
                        "还没设置 API Key：请在设置里填写".to_owned(),
                    ))
                }
                Err(err) => return Err((ErrorCode::Internal, format!("读取凭据失败：{err}"))),
            }
        }
        (None, Some(value)) => value,
        (None, None) => String::new(),
    };
    // t114：provider 维度选协议（responses → messages → completions），**回退不静默**
    let provider_base = if config.channel.as_str() == "cloud" {
        config.cloud_base_url.clone().unwrap_or_default()
    } else {
        config.base_url.clone().unwrap_or_default()
    };
    let protocol_choice = crate::protocol_pick::resolve_raw(&provider_base, &config.protocol);
    // 本次请求的 tool_choice（认不出的值 → auto + warn）
    let (tool_choice, tool_choice_warn) =
        crate::tool_choice::ToolChoice::parse(config.tool_choice.as_deref());
    if let Some(warn) = &tool_choice_warn {
        eprintln!("[yunong][chat] {warn}");
    }
    let effective_protocol = protocol_choice.effective;

    let mut request = config.to_request();
    request.protocol = effective_protocol;
    // 广告的 tools 必须**正好等于**注册了处理器的工具（见 `tools::specs_for`）
    let registered: Vec<String> = registry.names().into_iter().map(str::to_owned).collect();
    if !registered.is_empty() {
        request.tools = crate::tools::specs_for(&registered);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| (ErrorCode::Internal, format!("运行时创建失败：{err}")))?;

    let mut cancelled = false;
    let mut callback = SendableClosure(on_chunk);
    // 统一按 JSON 投递：工具事件不走 `Chunk` 枚举，但共用同一条回调
    // （字符串所有权仍是 `into_raw` + 调用方 free，见 `ffi::ChatCallback::emit`）
    // t116：记录"是否已经向客户端写出过可见内容"——写出过就**不许**回退协议
    //（用户已经看到半截回答，重来一次更糟）
    let streamed = std::sync::atomic::AtomicBool::new(false);
    // 本次提问里模型到底调了几个工具（判 tool_choice=required 有没有被遵守）
    let tool_calls_seen = std::sync::atomic::AtomicUsize::new(0);
    let mut emit = |json: String| -> bool {
        if json.contains("\"type\":\"delta\"") || json.contains("\"type\":\"tool_call\"") {
            streamed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if json.contains("\"type\":\"tool_call\"") {
            tool_calls_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        if callback.call(json) {
            false
        } else {
            cancelled = true;
            true
        }
    };

    // T80：流一启动就发一条 `meta`，**不等上游**（证明"回调通了"）。
    let channel = if config.channel.as_str() == "cloud" {
        Channel::Plan
    } else {
        Channel::Byok
    };
    let early_request_id = format!("{}_{}", channel.as_str(), uuid::Uuid::new_v4().simple());
    // 诊断（t113）：客户端/老板一眼看出"这次有没有挂工具"
    let tool_names: Vec<String> = registry.names().into_iter().map(str::to_owned).collect();
    let meta_cancelled = emit(meta_json(
        &early_request_id,
        &config.model,
        channel,
        &tool_names,
        &protocol_choice.requested_name,
        effective_protocol,
        tool_choice,
    ));
    // 回退必须可见：紧跟 meta 发一条 protocol_fallback（不静默换协议）
    let mut stop_early = meta_cancelled;
    if !stop_early {
        if let Some(reason) = protocol_choice.fallback_reason.clone() {
            stop_early = emit(crate::protocol_pick::fallback_event_json(
                &protocol_choice,
                &reason,
            ));
        }
    }

    let use_tools = !registry.names().is_empty();
    let mut guard = crate::tool_events::PairGuard::new();

    // 最终实际用的协议（运行时回退成功后会变）——写进 done 帧（t118）
    let mut final_protocol = effective_protocol;
    let outcome: Option<crate::error::CoreResult<StreamSummary>> = if stop_early {
        None
    } else {
        // t116：运行时回退——**只**在"协议不被支持"的明确信号上重试一次
        let mut protocol = effective_protocol;
        let mut attempted: Vec<Protocol> = vec![protocol];
        let result: crate::error::CoreResult<StreamSummary>;
        loop {
            let mut attempt_request = request.clone();
            attempt_request.protocol = protocol;
            let summary: crate::error::CoreResult<StreamSummary> = if use_tools {
                // ---- 工具循环路径 ----
                let gateway: Box<dyn crate::gateway::ModelGateway> = match config.channel.as_str() {
                    "cloud" => Box::new(crate::gateway::CloudProxy::new(
                        config.cloud_base_url.clone().unwrap_or_default(),
                        config.cloud_token.clone().unwrap_or_default(),
                    )),
                    _ => Box::new(
                        crate::gateway::DirectProvider::new(
                            protocol,
                            config.base_url.clone().unwrap_or_default(),
                            config.model.clone(),
                        )
                        .with_api_key(api_key.clone())
                        .with_tool_choice(Some(tool_choice)),
                    ),
                };
                let loop_config = crate::tool_loop::LoopConfig {
                    // 套餐通道按 ADR-05 最多 4 轮；BYOK 只受本地 24 轮约束
                    wire_max_rounds: (config.channel.as_str() == "cloud")
                        .then_some(yunong_shared::MAX_ROUNDS),
                    // 单工具超时：默认 10s；请求里可调（测试/联调要短超时）
                    tool_timeout: config
                        .tool_timeout_ms
                        .map(std::time::Duration::from_millis)
                        .unwrap_or_else(|| crate::tool_loop::LoopConfig::default().tool_timeout),
                    ..crate::tool_loop::LoopConfig::default()
                };
                let mut usage = Usage::default();
                let mut finish = FinishReason::Stop;
                let loop_result = {
                    // 作用域：让 `on_event` 在 guard.drain 之前释放（它借用 guard 与 emit）
                    let mut on_event =
                        |event: crate::tool_loop::LoopEvent| -> crate::gateway::Flow {
                            match event {
                                crate::tool_loop::LoopEvent::Delta { text } => {
                                    if emit(chunk_json(&Chunk::Delta { text })) {
                                        crate::gateway::Flow::Stop
                                    } else {
                                        crate::gateway::Flow::Continue
                                    }
                                }
                                crate::tool_loop::LoopEvent::ToolCallCompleted {
                                    index,
                                    id,
                                    name,
                                    arguments,
                                } => {
                                    guard.opened(&id);
                                    let json = crate::tool_events::tool_call_json(
                                        &id,
                                        &name,
                                        &arguments,
                                        Some(index),
                                    );
                                    if emit(json) {
                                        crate::gateway::Flow::Stop
                                    } else {
                                        crate::gateway::Flow::Continue
                                    }
                                }
                                crate::tool_loop::LoopEvent::ToolResult {
                                    id, ok, content, ..
                                } => {
                                    guard.closed(&id);
                                    let code = if ok {
                                        None
                                    } else {
                                        Some(crate::tool_events::FailureCode::from_message(
                                            &content,
                                        ))
                                    };
                                    let json = crate::tool_events::tool_result_json(
                                        &id, ok, &content, code,
                                    );
                                    if emit(json) {
                                        crate::gateway::Flow::Stop
                                    } else {
                                        crate::gateway::Flow::Continue
                                    }
                                }
                                crate::tool_loop::LoopEvent::RoundFinished {
                                    usage: round_usage,
                                    finish: round_finish,
                                    ..
                                } => {
                                    usage.tokens_in =
                                        usage.tokens_in.saturating_add(round_usage.tokens_in);
                                    usage.tokens_out =
                                        usage.tokens_out.saturating_add(round_usage.tokens_out);
                                    usage.estimated |= round_usage.estimated;
                                    finish = round_finish;
                                    crate::gateway::Flow::Continue
                                }
                                _ => crate::gateway::Flow::Continue,
                            }
                        };
                    runtime.block_on(crate::tool_loop::run(
                        &*gateway,
                        &registry,
                        attempt_request.clone(),
                        loop_config,
                        &mut on_event,
                    ))
                };
                match loop_result {
                    Ok(loop_outcome) => {
                        if loop_outcome.usage.tokens_in + loop_outcome.usage.tokens_out > 0 {
                            usage = loop_outcome.usage;
                        }
                        let was_cancelled = loop_outcome.cancelled;
                        Ok(StreamSummary {
                            usage,
                            finish: if was_cancelled {
                                FinishReason::Stop
                            } else {
                                finish
                            },
                            cancelled: was_cancelled,
                            usage_reported: false,
                        })
                    }
                    Err(err) => {
                        // 轮次上限 / 截断 / 上游错误：先把悬空的工具调用闭环，再把错误上抛
                        let code = if matches!(err, crate::error::CoreError::RoundLimit(_)) {
                            crate::tool_events::FailureCode::RoundLimit
                        } else {
                            crate::tool_events::FailureCode::ToolError
                        };
                        let reason = format!("工具未返回结果：{err}");
                        for json in guard.drain(&reason, code) {
                            emit(json);
                        }
                        Err(err)
                    }
                }
            } else {
                // ---- 直连路径（无工具，行为与之前完全一致）----
                match config.channel.as_str() {
                    "cloud" => {
                        let gateway = crate::gateway::CloudProxy::new(
                            config.cloud_base_url.clone().unwrap_or_default(),
                            config.cloud_token.clone().unwrap_or_default(),
                        );
                        runtime.block_on(gateway.chat_stream(&attempt_request, &mut |chunk| {
                            // 首帧 meta 上面已经发过：装配器再送的那条丢掉（meta 必首且唯一）
                            if matches!(chunk, Chunk::Meta { .. }) {
                                return crate::gateway::Flow::Continue;
                            }
                            // done 帧补 `effective_protocol`（t118：最终实际用的协议）
                            if let Chunk::Done { finish } = chunk {
                                let done = done_json_full(
                                    finish,
                                    protocol,
                                    tool_choice_honored(
                                        tool_choice,
                                        tool_calls_seen.load(std::sync::atomic::Ordering::SeqCst),
                                    ),
                                );
                                return if emit(done) {
                                    crate::gateway::Flow::Stop
                                } else {
                                    crate::gateway::Flow::Continue
                                };
                            }
                            if emit(chunk_json(&chunk)) {
                                crate::gateway::Flow::Stop
                            } else {
                                crate::gateway::Flow::Continue
                            }
                        }))
                    }
                    _ => {
                        let gateway = crate::gateway::DirectProvider::new(
                            protocol,
                            config.base_url.clone().unwrap_or_default(),
                            config.model.clone(),
                        )
                        .with_api_key(api_key.clone())
                        .with_tool_choice(Some(tool_choice));
                        runtime.block_on(gateway.chat_stream(&attempt_request, &mut |chunk| {
                            if matches!(chunk, Chunk::Meta { .. }) {
                                return crate::gateway::Flow::Continue;
                            }
                            // done 帧补 `effective_protocol`（t118：最终实际用的协议）
                            if let Chunk::Done { finish } = chunk {
                                let done = done_json_full(
                                    finish,
                                    protocol,
                                    tool_choice_honored(
                                        tool_choice,
                                        tool_calls_seen.load(std::sync::atomic::Ordering::SeqCst),
                                    ),
                                );
                                return if emit(done) {
                                    crate::gateway::Flow::Stop
                                } else {
                                    crate::gateway::Flow::Continue
                                };
                            }
                            if emit(chunk_json(&chunk)) {
                                crate::gateway::Flow::Stop
                            } else {
                                crate::gateway::Flow::Continue
                            }
                        }))
                    }
                }
                .map(|stream_outcome| StreamSummary {
                    usage: stream_outcome.usage,
                    finish: stream_outcome.finish,
                    cancelled: stream_outcome.cancelled,
                    usage_reported: true,
                })
            };
            final_protocol = protocol;
            match summary {
                Ok(summary) => {
                    result = Ok(summary);
                    break;
                }
                Err(err) => {
                    // 运行时回退只对 **BYOK 直连**开放：套餐通道走我们自己的代理，
                    // 换协议重试等于对同一 turn 打两次请求（会重复计费），不做。
                    let byok = config.channel.as_str() != "cloud";
                    let retry = if byok
                        && !streamed.load(std::sync::atomic::Ordering::SeqCst)
                        && attempted.len() < 2
                    {
                        crate::protocol_pick::unsupported_signal(&err).zip(
                            crate::protocol_pick::next_runtime_candidate(
                                &protocol_choice.provider,
                                protocol,
                                &attempted,
                            ),
                        )
                    } else {
                        None
                    };
                    match retry {
                        Some((reason, next)) => {
                            // 只重试一次，且写清"这是重试"（retried / attempt）
                            emit(crate::protocol_pick::retried_event_json(
                                &protocol_choice.requested_name,
                                &protocol_choice.provider,
                                next,
                                &reason,
                                attempted.len() as u32 + 1,
                            ));
                            attempted.push(next);
                            protocol = next;
                            continue;
                        }
                        None => {
                            result = Err(err);
                            break;
                        }
                    }
                }
            }
        }
        Some(result)
    };

    // ---- 收尾：先把悬空的工具调用闭环，再发结束帧 ----
    match outcome {
        // 首帧即取消：没联网，但客户端仍要收到终帧
        None => {
            for json in guard.drain("已取消，未执行", crate::tool_events::FailureCode::Cancelled)
            {
                emit(json);
            }
            emit(done_json_full(
                FinishReason::Stop,
                final_protocol,
                tool_choice_honored(
                    tool_choice,
                    tool_calls_seen.load(std::sync::atomic::Ordering::SeqCst),
                ),
            ));
            Ok(())
        }
        Some(Ok(summary)) => {
            let reason = if summary.cancelled {
                "已取消，未执行"
            } else {
                "流已结束，工具未返回结果"
            };
            for json in guard.drain(reason, crate::tool_events::FailureCode::Cancelled) {
                emit(json);
            }
            if !summary.usage_reported {
                // 工具路径：`usage` 被循环吞进 `RoundFinished`，这里补一帧（抽样记账用）
                if summary.usage.tokens_in + summary.usage.tokens_out > 0 {
                    emit(chunk_json(&Chunk::Usage {
                        tokens_in: summary.usage.tokens_in,
                        tokens_out: summary.usage.tokens_out,
                        estimated: summary.usage.estimated,
                    }));
                }
                emit(done_json_full(
                    summary.finish,
                    final_protocol,
                    tool_choice_honored(
                        tool_choice,
                        tool_calls_seen.load(std::sync::atomic::Ordering::SeqCst),
                    ),
                ));
            } else if summary.cancelled {
                // R5-F7：取消后补终帧，否则客户端停在"加载中"
                emit(done_json());
            }
            Ok(())
        }
        Some(Err(err)) => {
            for json in guard.drain("工具未返回结果", crate::tool_events::FailureCode::ToolError)
            {
                emit(json);
            }
            let (code, message) = map_core_error(err);
            emit(error_chunk_json(code, message));
            Ok(())
        }
    }
}

/// 一条流的归一化结果（直连路径 / 工具循环路径统一）。
struct StreamSummary {
    usage: Usage,
    finish: FinishReason,
    cancelled: bool,
    /// 直连路径的 `usage` 已经由网关作为 `Chunk::Usage` 发过，不要再补一帧
    usage_reported: bool,
}

/// `meta` 帧（首帧）：在契约字段之外**多加几个诊断字段**。
///
/// - `tools_enabled` / `tools`：这次有没有挂端侧工具、挂了哪些（false = 模型根本不知道有课表/记忆可查）；
/// - `requested_protocol` / `effective_protocol`：用户要的协议 vs 实际用的（不同就表示发生了回退）。
///
/// 不加进 `crates/shared::Chunk`（那是共享契约，改它要过 captain）：这里手写 JSON，
/// 老客户端按已知字段解析、多出来的字段自然忽略。
fn meta_json(
    request_id: &str,
    model: &str,
    channel: Channel,
    tools: &[String],
    requested_protocol: &str,
    effective_protocol: Protocol,
    tool_choice: crate::tool_choice::ToolChoice,
) -> String {
    serde_json::json!({
        "type": "meta",
        "request_id": request_id,
        "model": model,
        "channel": channel,
        "tools_enabled": !tools.is_empty(),
        "tools": tools,
        "requested_protocol": requested_protocol,
        "effective_protocol": crate::protocol_pick::protocol_name(effective_protocol),
        // t124：本次请求的「工具选择」策略与实际生效值
        // （messages 协议下 required 只能发 `{"type":"any"}`，DeepSeek 兼容层按 auto 处理，
        //   所以 effective 会是 "any" 而不是 "required"）
        "tool_choice": tool_choice.as_str(),
        "tool_choice_effective": crate::tool_choice::effective_label(effective_protocol, tool_choice),
    })
    .to_string()
}

fn chunk_json(chunk: &Chunk) -> String {
    serde_json::to_string(chunk)
        .unwrap_or_else(|_| error_chunk_json(ErrorCode::Internal, "序列化失败"))
}

/// `done` 帧：在契约字段之外补一个 **`effective_protocol`**（t118）。
///
/// 语义分工：
/// - `meta`（首帧）：出发时的 `requested_protocol` / `effective_protocol`（初判）；
/// - `protocol_fallback`：发生过回退的事实（`retried` / `attempt`）；
/// - `done`（末帧）：**最终实际成功**的协议——运行时回退成功后就以它为准。
///
/// 不改 `crates/shared::Chunk`（共享契约），这里手写 JSON 注入字段，老客户端忽略即可。
fn done_json_with_protocol(finish: FinishReason, protocol: Protocol) -> String {
    done_json_full(finish, protocol, None)
}

/// 这次 `tool_choice=required` 有没有被模型遵守（没强制时返回 `None`）。
///
/// - 只有 `required` 才有意义；`auto`/`none` → `None`（不写字段）；
/// - 模型一次工具都没调用 → `Some(false)`，`done` 里会标注，
///   客户端据此提示"已强制但模型仍然只回了文本"，**不会**因此重试。
fn tool_choice_honored(
    choice: crate::tool_choice::ToolChoice,
    tool_calls_seen: usize,
) -> Option<bool> {
    matches!(choice, crate::tool_choice::ToolChoice::Required).then_some(tool_calls_seen > 0)
}

/// `done` 帧的完整版本：`effective_protocol` + 可选的 `tool_choice_honored`。
fn done_json_full(
    finish: FinishReason,
    protocol: Protocol,
    tool_choice_honored: Option<bool>,
) -> String {
    let base = serde_json::to_value(Chunk::Done { finish }).unwrap_or(serde_json::Value::Null);
    let mut object = base.as_object().cloned().unwrap_or_default();
    object.insert(
        "effective_protocol".to_owned(),
        serde_json::Value::String(crate::protocol_pick::protocol_name(protocol).to_owned()),
    );
    if let Some(honored) = tool_choice_honored {
        object.insert(
            "tool_choice_honored".to_owned(),
            serde_json::Value::Bool(honored),
        );
    }
    serde_json::Value::Object(object).to_string()
}

fn done_json() -> String {
    done_json_with_protocol(FinishReason::Stop, Protocol::OpenaiCompletions)
}

/// `CoreError` → 契约错误码（UI 走同一套文案）。
pub fn map_core_error(err: crate::error::CoreError) -> (ErrorCode, String) {
    use crate::error::CoreError;
    match err {
        CoreError::Network(message) => (ErrorCode::UpstreamError, message),
        CoreError::Timeout(_) => (ErrorCode::UpstreamTimeout, err.to_string()),
        CoreError::Cancelled(_) => (ErrorCode::Internal, "已取消".to_owned()),
        CoreError::Remote {
            code,
            message,
            retry_after,
        } => {
            // `CoreError::Remote.code` 本来就是契约错误码，直接用
            let message = match retry_after {
                Some(duration) => format!("{message}（{}s 后可重试）", duration.as_secs()),
                None => message,
            };
            (code, message)
        }
        other => (ErrorCode::Internal, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yunong_shared::{ChatMessage, Role};

    fn byok_json(api_key: &str) -> String {
        serde_json::json!({
            "channel": "byok",
            "protocol": "openai_completions",
            "model": "deepseek-chat",
            "api_key": api_key,
            "messages": [{ "role": "user", "content": "你好" }]
        })
        .to_string()
    }

    /// R5-F7：取消之后必须补一帧终态（`done`），客户端才能离开"加载中"。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_emits_a_terminal_done_frame() {
        fn delta(text: &str) -> String {
            format!(
                "data: {}\n\n",
                serde_json::json!({
                    "id": "c1",
                    "object": "chat.completion.chunk",
                    "created": 1,
                    "model": "deepseek-chat",
                    "choices": [{
                        "index": 0,
                        "delta": { "content": text },
                        "finish_reason": serde_json::Value::Null
                    }]
                })
            )
        }
        let server = crate::gateway::fake_server::FakeServer::start(
            200,
            "text/event-stream",
            Vec::new(),
            vec![
                crate::gateway::fake_server::Step::Write(delta("你")),
                crate::gateway::fake_server::Step::Write(delta("好")),
                crate::gateway::fake_server::Step::Write("data: [DONE]\n\n".to_owned()),
                crate::gateway::fake_server::Step::Close,
            ],
        )
        .await;
        let mut config = ChatRunConfig::parse(&byok_json("sk-test")).unwrap();
        config.base_url = Some(server.base_url());

        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = std::sync::Arc::clone(&chunks);
        // `run_stream` 自建 runtime，放到阻塞线程里跑
        tokio::task::spawn_blocking(move || {
            run_stream(config, move |json| {
                let mut guard = sink.lock().unwrap();
                guard.push(json);
                // 收到第 1 条（meta）就取消：模拟用户点了"停止"
                guard.len() > 1
            })
            .unwrap();
        })
        .await
        .unwrap();

        let chunks = chunks.lock().unwrap().clone();
        assert!(!chunks.is_empty(), "至少要有 meta");
        let last: serde_json::Value =
            serde_json::from_str(chunks.last().expect("取消后应有终帧")).unwrap();
        assert_eq!(
            last["type"], "done",
            "取消后最后一条必须是终帧，实际：{last}"
        );
        assert_eq!(last["finish"], "stop");
        // 取消之前的 chunk 照常到达
        assert!(chunks.len() >= 2, "{chunks:?}");
        // T80：早发的那条 meta + 装配器那条必须去重，wire 上只能有一条 meta
        let metas = chunks
            .iter()
            .filter(|chunk| {
                serde_json::from_str::<serde_json::Value>(chunk)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("type")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some("meta")
            })
            .count();
        assert_eq!(metas, 1, "meta 必须唯一：{chunks:?}");
    }

    #[test]
    fn parse_rejects_missing_api_key_with_readable_code() {
        let err = ChatRunConfig::parse(&byok_json("")).unwrap_err();
        assert_eq!(err.0, ErrorCode::Unauthorized);
        assert!(err.1.contains("API Key"), "{}", err.1);
    }

    #[test]
    fn parse_rejects_missing_cloud_token() {
        let json = serde_json::json!({
            "channel": "cloud",
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }]
        })
        .to_string();
        let err = ChatRunConfig::parse(&json).unwrap_err();
        assert_eq!(err.0, ErrorCode::Unauthorized);
        assert!(err.1.contains("套餐"), "{}", err.1);
    }

    #[test]
    fn parse_rejects_unknown_channel_and_bad_json() {
        let json = serde_json::json!({
            "channel": "magic",
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }]
        })
        .to_string();
        assert_eq!(
            ChatRunConfig::parse(&json).unwrap_err().0,
            ErrorCode::ValidationError
        );
        assert_eq!(
            ChatRunConfig::parse("not json").unwrap_err().0,
            ErrorCode::ValidationError
        );
    }

    #[test]
    fn to_request_fills_contract_defaults() {
        let config = ChatRunConfig::parse(&byok_json("sk-test")).unwrap();
        let request = config.to_request();
        assert_eq!(request.round, 1);
        assert!(request.stream);
        assert!(!request.session_id.is_empty());
        assert!(!request.turn_id.is_empty());
        assert_eq!(request.model, "deepseek-chat");
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn error_chunk_is_a_stream_event_not_an_exception() {
        let json = error_chunk_json(ErrorCode::Unauthorized, "没填 Key".repeat(200));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "error");
        // 契约里的错误码是 SCREAMING_SNAKE_CASE（`docs/03 §1.3`）
        assert_eq!(value["code"], "UNAUTHORIZED");
        // 消息截断到 200 字符
        assert!(value["message"].as_str().unwrap().chars().count() <= 200);
    }

    #[test]
    fn cancel_registry_lifecycle() {
        let registry = CancelRegistry::new();
        let a = registry.allocate();
        let b = registry.allocate();
        assert_ne!(a, b);
        assert!(!registry.is_cancelled(a));
        assert!(registry.cancel(a));
        assert!(registry.is_cancelled(a));
        assert!(!registry.is_cancelled(b));
        registry.release(a);
        assert!(!registry.is_cancelled(a));
    }

    #[test]
    fn protocol_strings_map_to_known_variants() {
        assert_eq!(
            parse_protocol("openai_responses"),
            Protocol::OpenaiResponses
        );
        assert_eq!(
            parse_protocol("anthropic_messages"),
            Protocol::AnthropicMessages
        );
        assert_eq!(parse_protocol("whatever"), Protocol::OpenaiCompletions);
    }

    #[test]
    fn messages_deserialize_from_contract_json() {
        let config = ChatRunConfig::parse(&byok_json("sk")).unwrap();
        assert_eq!(config.messages[0].role, Role::User);
        assert_eq!(config.messages[0].content, "你好");
        let _ = ChatMessage::user("x");
    }

    // ---------------------------------------------------------------------
    // t103：工具事件透出（tool_call / tool_result，配对永不悬空）
    // ---------------------------------------------------------------------

    use std::io::{Read, Write};

    /// 能按**连接顺序**回放脚本的假上游（工具循环每轮一个连接）。
    struct ScriptedServer {
        port: u16,
        /// 收到的请求体（按连接顺序），用来断言"tools 到底有没有广告给上游"
        bodies: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl ScriptedServer {
        fn start(responses: Vec<String>) -> Self {
            let scripted: Vec<(u16, String)> =
                responses.into_iter().map(|body| (200, body)).collect();
            Self::start_with_status(scripted)
        }

        /// 每个连接按顺序回放 `(状态码, 响应体)`；用来模拟"协议不支持"（404/405）等失败。
        fn start_with_status(responses: Vec<(u16, String)>) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let recorder = std::sync::Arc::clone(&bodies);
            std::thread::spawn(move || {
                for (status, body) in responses {
                    let Ok((mut socket, _)) = listener.accept() else {
                        return;
                    };
                    // 读完请求头 + body，避免上游比客户端先关
                    let mut buffer = [0_u8; 4096];
                    let mut seen = Vec::new();
                    let mut header_end = None;
                    loop {
                        let read = socket.read(&mut buffer).unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        seen.extend_from_slice(&buffer[..read]);
                        if let Some(position) =
                            seen.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            header_end = Some(position + 4);
                            break;
                        }
                    }
                    if let Some(start) = header_end {
                        let head = String::from_utf8_lossy(&seen[..start]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        while seen.len() < start + length {
                            let read = socket.read(&mut buffer).unwrap_or(0);
                            if read == 0 {
                                break;
                            }
                            seen.extend_from_slice(&buffer[..read]);
                        }
                        let body = String::from_utf8_lossy(&seen[start..]).into_owned();
                        recorder.lock().unwrap().push(body);
                    }
                    let reason = if status == 200 { "OK" } else { "ERR" };
                    let content_type = if status == 200 {
                        "text/event-stream"
                    } else {
                        "application/json"
                    };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes());
                    let _ = socket.flush();
                }
            });
            Self { port, bodies }
        }

        fn base_url(&self) -> String {
            format!("http://127.0.0.1:{}/v1", self.port)
        }

        fn request_count(&self) -> usize {
            self.bodies.lock().unwrap().len()
        }
    }

    fn sse(payload: serde_json::Value) -> String {
        format!("data: {payload}\n\n")
    }

    /// 第一轮：模型要求调用 `echo`（参数是对象）；第二轮：说一句话收尾。
    fn tool_round(name: &str, call_id: &str, arguments: &str) -> String {
        let mut body = String::new();
        body.push_str(&sse(serde_json::json!({
            "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "m",
            "choices": [{
                "index": 0,
                "delta": { "tool_calls": [{
                    "index": 0, "id": call_id, "type": "function",
                    "function": { "name": name, "arguments": arguments }
                }]},
                "finish_reason": serde_json::Value::Null
            }]
        })));
        body.push_str(&sse(serde_json::json!({
            "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "m",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }]
        })));
        body.push_str("data: [DONE]\n\n");
        body
    }

    fn final_round(text: &str) -> String {
        format!(
            "{}{}{}",
            sse(serde_json::json!({
                "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "m",
                "choices": [{ "index": 0, "delta": { "content": text },
                              "finish_reason": serde_json::Value::Null }]
            })),
            sse(serde_json::json!({
                "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "m",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 7, "completion_tokens": 3 }
            })),
            "data: [DONE]\n\n"
        )
    }

    fn byok_config(base_url: &str, tool_timeout_ms: Option<u64>) -> ChatRunConfig {
        let mut config = ChatRunConfig::parse(&byok_json("sk-test")).unwrap();
        config.base_url = Some(base_url.to_owned());
        config.tool_timeout_ms = tool_timeout_ms;
        config
    }

    /// 跑一次流，收集所有 chunk。
    fn run_collect(config: ChatRunConfig, registry: crate::tools::ToolRegistry) -> Vec<String> {
        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = std::sync::Arc::clone(&chunks);
        // `run_stream` 自建 runtime，放到普通线程里跑即可（与 FFI 调用方式一致）
        std::thread::spawn(move || {
            run_stream_with_registry(config, registry, move |json| {
                sink.lock().unwrap().push(json);
                true
            })
            .unwrap();
        })
        .join()
        .unwrap();
        let guard = chunks.lock().unwrap();
        guard.clone()
    }

    fn parse_all(chunks: &[String]) -> Vec<serde_json::Value> {
        chunks
            .iter()
            .map(|json| serde_json::from_str(json).unwrap_or_else(|err| panic!("{err}: {json}")))
            .collect()
    }

    fn calls_of(events: &[serde_json::Value]) -> Vec<&serde_json::Value> {
        events
            .iter()
            .filter(|event| event["type"] == "tool_call")
            .collect()
    }

    fn results_of(events: &[serde_json::Value]) -> Vec<&serde_json::Value> {
        events
            .iter()
            .filter(|event| event["type"] == "tool_result")
            .collect()
    }

    /// 正常路径：`tool_call` → `tool_result`，`call_id` 配对、`args` 原样、顺序正确。
    #[test]
    fn tool_call_and_result_are_paired_and_args_are_verbatim() {
        let server = ScriptedServer::start(vec![
            tool_round("echo", "call_1", "{\"week\":8}"),
            final_round("好"),
        ]);
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register("echo", |_args| Ok("5 条结果".to_owned()));
        let chunks = run_collect(byok_config(&server.base_url(), Some(2_000)), registry);
        // `--nocapture` 时把真实 wire JSON 打出来（报告/联调对照用）
        for json in &chunks {
            println!("[wire] {json}");
        }
        let events = parse_all(&chunks);

        let calls = calls_of(&events);
        let results = results_of(&events);
        assert_eq!(calls.len(), 1, "{chunks:#?}");
        assert_eq!(results.len(), 1, "{chunks:#?}");
        // 配对：call 在 result 之前，call_id 相同
        assert_eq!(calls[0]["call_id"], "call_1");
        assert_eq!(results[0]["call_id"], "call_1");
        let call_position = chunks
            .iter()
            .position(|json| json.contains("call_1") && json.contains("tool_call"))
            .unwrap();
        let result_position = chunks
            .iter()
            .position(|json| json.contains("call_1") && json.contains("tool_result"))
            .unwrap();
        assert!(call_position < result_position, "call 必须在 result 之前");
        // 契约形状：args 原样是对象
        assert_eq!(calls[0]["args"], serde_json::json!({ "week": 8 }));
        assert_eq!(calls[0]["name"], "echo");
        // 结果：ok=true + 一句短话，且成功不带 code
        assert_eq!(results[0]["ok"], true);
        assert_eq!(results[0]["summary"], "5 条结果");
        assert!(results[0].get("code").is_none(), "{}", results[0]);
        // 流仍以 done 结尾
        assert_eq!(events.last().unwrap()["type"], "done");
    }

    // ---------------------------------------------------------------------
    // t113：工具定义必须真的广告给上游（否则模型只会说"我没有你的课表上下文"）
    // ---------------------------------------------------------------------

    fn config_with_request(extra: serde_json::Value) -> ChatRunConfig {
        let mut request = serde_json::json!({
            "channel": "byok",
            "protocol": "openai_completions",
            "model": "deepseek-chat",
            "api_key": "sk-test",
            "messages": [{ "role": "user", "content": "第 8 周哪天最累？" }]
        });
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        ChatRunConfig::parse(&request.to_string()).unwrap()
    }

    /// 有 `db_path` → 请求体里必须有两个内置工具的**定义**（中文描述 + schema）。
    #[test]
    fn request_advertises_builtin_tools_when_db_path_is_set() {
        let config = config_with_request(serde_json::json!({ "db_path": "/tmp/app.db" }));
        let request = config.to_request();
        let names: Vec<&str> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert!(names.contains(&"timetable_query"), "{names:?}");
        assert!(names.contains(&"memory_search"), "{names:?}");
        for tool in &request.tools {
            assert!(!tool.description.is_empty(), "{} 没有描述", tool.name);
            assert!(
                tool.description
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{} 的描述应该是中文（模型按语义匹配中文提问）：{}",
                tool.name,
                tool.description
            );
            assert_eq!(
                tool.parameters["type"], "object",
                "{} 的参数必须是 JSON Schema",
                tool.name
            );
            assert!(
                tool.parameters["properties"].is_object(),
                "{} 缺少 properties",
                tool.name
            );
        }
        // 课表工具要能指定周次
        let timetable = request
            .tools
            .iter()
            .find(|tool| tool.name == "timetable_query")
            .unwrap();
        assert!(timetable.parameters["properties"]["week"].is_object());
    }

    /// 没有 `db_path` → 不带任何 tools（老行为，纯聊天）。
    #[test]
    fn request_has_no_tools_without_db_path() {
        let config = config_with_request(serde_json::json!({}));
        assert!(config.to_request().tools.is_empty());
    }

    /// 端到端：上游真的收到了 `tools`（第一轮请求体），并且 meta 帧报了 tools_enabled。
    #[test]
    fn upstream_receives_tools_in_the_request_body() {
        let server = ScriptedServer::start(vec![
            tool_round("timetable_query", "call_t", "{\"week\":8}"),
            final_round("好"),
        ]);
        let db = std::env::temp_dir().join(format!("ynad-t113-{}.db", std::process::id()));
        let registry = crate::tools::builtin_registry(Some(db.to_string_lossy().into_owned()));
        let config = byok_config(&server.base_url(), Some(2_000));
        // 带 db_path 才会挂工具；这里用真实的 builtin 注册表（不是空壳）
        let mut config = config;
        config.db_path = Some("/tmp/app.db".to_owned());
        config.messages = vec![ChatMessage::user("第 8 周哪天最累？")];
        let chunks = run_collect(config, registry);
        let events = parse_all(&chunks);

        // 1) 请求体里必须有 tools，且含两个内置工具名
        let bodies = server.bodies.lock().unwrap().clone();
        assert!(!bodies.is_empty(), "上游没收到请求");
        let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        let tools = first["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("请求体里没有 tools：{first}"));
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"timetable_query"), "{names:?}");
        assert!(names.contains(&"memory_search"), "{names:?}");
        assert_eq!(tools[0]["type"], "function");

        // 2) meta 帧报出 tools_enabled 与工具名
        let meta = events.first().unwrap();
        assert_eq!(meta["type"], "meta");
        assert_eq!(meta["tools_enabled"], true, "{meta}");
        let meta_tools: Vec<&str> = meta["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert!(meta_tools.contains(&"timetable_query"), "{meta}");
    }

    /// 直连路径（没有工具）：meta 里 `tools_enabled=false`，上游请求体里没有 tools。
    #[test]
    fn no_tools_means_meta_says_false_and_body_has_no_tools_field() {
        let server = ScriptedServer::start(vec![final_round("你好")]);
        let config = byok_config(&server.base_url(), None); // 不带 db_path
        let chunks = run_collect(config, crate::tools::ToolRegistry::new());
        let events = parse_all(&chunks);
        assert_eq!(
            events.first().unwrap()["tools_enabled"],
            false,
            "{:?}",
            events[0]
        );
        let bodies = server.bodies.lock().unwrap().clone();
        let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        assert!(
            first.get("tools").is_none(),
            "没有工具时不该带 tools：{first}"
        );
    }

    // ---------------------------------------------------------------------
    // t114：三条协议都要广告工具 + 协议回退可观测
    // ---------------------------------------------------------------------

    /// 三协议按**各自的 wire 格式**广告工具（对象/数组形状不同，但都必须有）。
    #[test]
    fn all_three_protocols_advertise_tools_in_their_own_shape() {
        for protocol in [
            yunong_shared::Protocol::OpenaiCompletions,
            yunong_shared::Protocol::OpenaiResponses,
            yunong_shared::Protocol::AnthropicMessages,
        ] {
            let config = config_with_request(serde_json::json!({
                "db_path": "/tmp/app.db",
                "protocol": crate::protocol_pick::protocol_name(protocol),
            }));
            let mut request = config.to_request();
            request.protocol = protocol;
            let body = crate::protocols::build_body(protocol, &request);
            let tools = body["tools"].as_array().unwrap_or_else(|| {
                panic!(
                    "{} 没有 tools：{body}",
                    crate::protocol_pick::protocol_name(protocol)
                )
            });
            println!(
                "[tools:{}] {}",
                crate::protocol_pick::protocol_name(protocol),
                serde_json::to_string(tools).unwrap()
            );
            assert_eq!(tools.len(), 2, "两个内置工具都要广告");
            let names: Vec<&str> = tools
                .iter()
                .map(|tool| {
                    // completions: {"type":"function","function":{"name":…}}
                    // responses:   {"type":"function","name":…}
                    // messages:    {"name":…,"input_schema":…}
                    tool["function"]["name"]
                        .as_str()
                        .or_else(|| tool["name"].as_str())
                        .unwrap_or_default()
                })
                .collect();
            assert!(names.contains(&"timetable_query"), "{names:?}");
            assert!(names.contains(&"memory_search"), "{names:?}");
        }
    }

    /// 回退必须**可观测**：meta 里 requested/effective 都写出来，且发一条 protocol_fallback。
    #[test]
    fn protocol_fallback_is_visible_in_meta_and_as_an_event() {
        // 静态表里确实不支持的组合：Anthropic 主机 + completions → 回退到 messages
        // （DeepSeek 三种协议现在都支持，t118 起不再用它触发回退）
        let choice =
            crate::protocol_pick::resolve_raw("https://api.anthropic.com", "openai_completions");
        assert!(choice.fallback(), "{choice:?}");
        let event: serde_json::Value =
            serde_json::from_str(&crate::protocol_pick::fallback_event_json(
                &choice,
                choice.fallback_reason.as_deref().unwrap(),
            ))
            .unwrap();
        println!("[fallback] {event}");
        assert_eq!(event["type"], "protocol_fallback");
        assert_eq!(event["requested_protocol"], "openai_completions");
        assert_eq!(event["effective_protocol"], "anthropic_messages");

        // 认不出的协议名：按 completions 处理，但**说明原因**（以前是静默的）
        let typo = crate::protocol_pick::resolve_raw("https://api.deepseek.com", "responses_v2");
        assert_eq!(typo.effective, yunong_shared::Protocol::OpenaiCompletions);
        assert!(typo
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("认不出的协议名"));
    }

    // ---------------------------------------------------------------------
    // t116：运行时回退（只在"协议不支持"的明确信号上，重试一次，不覆盖已写出的流）
    // ---------------------------------------------------------------------

    /// Responses 协议的 SSE 片段（回退目标用）。
    fn responses_body(text: &str) -> String {
        format!(
            "event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"delta\":\"{text}\"}}\n\n\
             event: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\",\"usage\":{{\"input_tokens\":3,\"output_tokens\":1}}}}}}\n\n"
        )
    }

    fn config_with_protocol(base_url: &str, protocol: &str) -> ChatRunConfig {
        let mut config = ChatRunConfig::parse(&byok_json("sk-test")).unwrap();
        config.base_url = Some(base_url.to_owned());
        config.protocol = protocol.to_owned();
        config
    }

    /// ① 第一轮 404 → 回退到下一协议并成功：retried / attempt=2 / delta 正常。
    #[test]
    fn runtime_fallback_retries_once_on_404() {
        let server = ScriptedServer::start_with_status(vec![
            (
                404,
                "{\"error\":{\"message\":\"no such endpoint\"}}".to_owned(),
            ),
            (200, responses_body("下午没课")),
        ]);
        let config = config_with_protocol(&server.base_url(), "openai_completions");
        let chunks = run_collect(config, crate::tools::ToolRegistry::new());
        let events = parse_all(&chunks);
        for event in &events {
            println!("[wire] {event}");
        }
        let fallback = events
            .iter()
            .find(|event| event["type"] == "protocol_fallback")
            .unwrap_or_else(|| panic!("没有 protocol_fallback：{chunks:#?}"));
        assert_eq!(fallback["retried"], true, "{fallback}");
        assert_eq!(fallback["attempt"], 2, "{fallback}");
        assert_eq!(
            fallback["effective_protocol"], "openai_responses",
            "{fallback}"
        );
        assert_eq!(
            fallback["requested_protocol"], "openai_completions",
            "{fallback}"
        );
        // 重试成功后的 delta 正常到达，且以 done 收尾
        assert!(
            events.iter().any(|event| event["type"] == "delta"),
            "{chunks:#?}"
        );
        assert_eq!(events.last().unwrap()["type"], "done", "{chunks:#?}");
        // 恰好两个请求
        assert_eq!(server.request_count(), 2, "应只重试一次");
    }

    /// ② 401 鉴权失败 → **不回退**，直接报错（只发一个请求）。
    #[test]
    fn no_runtime_fallback_on_401() {
        let server = ScriptedServer::start_with_status(vec![(
            401,
            "{\"error\":{\"message\":\"invalid api key\"}}".to_owned(),
        )]);
        let config = config_with_protocol(&server.base_url(), "openai_completions");
        let chunks = run_collect(config, crate::tools::ToolRegistry::new());
        let events = parse_all(&chunks);
        for event in &events {
            println!("[wire] {event}");
        }
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "protocol_fallback"),
            "401 不该回退：{chunks:#?}"
        );
        let error = events
            .iter()
            .find(|event| event["type"] == "error")
            .unwrap_or_else(|| panic!("应收到 error 帧：{chunks:#?}"));
        assert_eq!(error["code"], "UNAUTHORIZED", "{error}");
        assert_eq!(server.request_count(), 1, "401 只该发一个请求");
    }

    /// ③ 已写出 delta 后失败 → **不回退**（用户已经看到半截内容）。
    #[test]
    fn no_runtime_fallback_after_streaming_started() {
        // 第一条响应先给一个 delta，然后**直接关连接**（不发 [DONE]，也不发结束帧）
        let partial = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",",
            "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"半截\"},\"finish_reason\":null}]}\n\n"
        )
        .to_owned();
        let server = ScriptedServer::start_with_status(vec![
            (200, partial),
            (200, responses_body("不该出现")),
        ]);
        let config = config_with_protocol(&server.base_url(), "openai_completions");
        let chunks = run_collect(config, crate::tools::ToolRegistry::new());
        let events = parse_all(&chunks);
        for event in &events {
            println!("[wire] {event}");
        }
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "protocol_fallback"),
            "已发 delta 后不许回退：{chunks:#?}"
        );
        assert!(
            !chunks.iter().any(|json| json.contains("不该出现")),
            "没有第二次请求：{chunks:#?}"
        );
        assert_eq!(server.request_count(), 1, "已开始流式就不再重试");
    }

    /// ④ 两次都 404 → 报错、只重试一次、不无限重试。
    #[test]
    fn runtime_fallback_stops_after_one_retry() {
        let not_found = (404, "{\"error\":{\"message\":\"not found\"}}".to_owned());
        let server = ScriptedServer::start_with_status(vec![not_found.clone(), not_found]);
        let config = config_with_protocol(&server.base_url(), "openai_completions");
        let chunks = run_collect(config, crate::tools::ToolRegistry::new());
        let events = parse_all(&chunks);
        for event in &events {
            println!("[wire] {event}");
        }
        let fallbacks = events
            .iter()
            .filter(|event| event["type"] == "protocol_fallback")
            .count();
        assert_eq!(fallbacks, 1, "只回退一次：{chunks:#?}");
        assert_eq!(server.request_count(), 2, "总共两个请求（原始 + 一次重试）");
        assert!(
            events.iter().any(|event| event["type"] == "error"),
            "最终要报错：{chunks:#?}"
        );
    }

    /// 真实流里：deepseek + responses → 自动回退到 completions，并**发一条 protocol_fallback**。
    ///
    /// 这里 `base_url` 指向 1 号端口（连不上），但 meta 与回退事件在联网**之前**就发出去了，
    /// 所以正好能验证"回退决策 + 可观测性"这段接线。
    #[test]
    fn anthropic_host_with_completions_falls_back_in_a_real_stream() {
        let config = config_with_request(serde_json::json!({
            "db_path": "/tmp/app.db",
            "protocol": "openai_completions",
            "base_url": "http://api.anthropic.com:1/v1",
        }));
        // 收到回退事件就让回调返回 false（模拟客户端叫停），不必真的去连 anthropic
        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = std::sync::Arc::clone(&chunks);
        std::thread::spawn(move || {
            run_stream_with_registry(config, crate::tools::ToolRegistry::new(), move |json| {
                let mut guard = sink.lock().unwrap();
                let stop = json.contains("protocol_fallback");
                guard.push(json);
                !stop
            })
            .unwrap();
        })
        .join()
        .unwrap();
        let chunks = chunks.lock().unwrap().clone();
        let events = parse_all(&chunks);
        let meta = &events[0];
        assert_eq!(meta["type"], "meta", "{meta}");
        assert_eq!(meta["requested_protocol"], "openai_completions", "{meta}");
        assert_eq!(meta["effective_protocol"], "anthropic_messages", "{meta}");
        // 第二条必须是回退事件（紧跟 meta，不许静默）
        let fallback = &events[1];
        assert_eq!(fallback["type"], "protocol_fallback", "{fallback}");
        assert_eq!(fallback["provider"], "api.anthropic.com", "{fallback}");
        assert!(fallback["reason"].as_str().unwrap().contains("不支持"));
    }

    /// t118：deepseek 三种协议都支持——**不再**被静态降级，meta 里 effective 就是 requested。
    #[test]
    fn deepseek_keeps_responses_without_static_fallback() {
        let config = config_with_request(serde_json::json!({
            "db_path": "/tmp/app.db",
            "protocol": "openai_responses",
            "base_url": "http://api.deepseek.com:1/v1",
        }));
        // 只取 meta（第一个 chunk），随后让回调叫停，避免真的联网
        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = std::sync::Arc::clone(&chunks);
        std::thread::spawn(move || {
            run_stream_with_registry(config, crate::tools::ToolRegistry::new(), move |json| {
                sink.lock().unwrap().push(json);
                false
            })
            .unwrap();
        })
        .join()
        .unwrap();
        let events = parse_all(&chunks.lock().unwrap().clone());
        let meta = &events[0];
        assert_eq!(meta["requested_protocol"], "openai_responses", "{meta}");
        assert_eq!(meta["effective_protocol"], "openai_responses", "{meta}");
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "protocol_fallback"),
            "deepseek + responses 不该静态回退：{events:?}"
        );
    }

    // ---------------------------------------------------------------------
    // t124：tool_choice 透传（auto / none / required）
    // ---------------------------------------------------------------------

    fn config_with_tool_choice(base_url: &str, tool_choice: &str) -> ChatRunConfig {
        let mut config = config_with_request(serde_json::json!({
            "db_path": "/tmp/app.db",
            "protocol": "openai_completions",
            "base_url": base_url,
            "tool_choice": tool_choice,
        }));
        config.messages = vec![yunong_shared::ChatMessage {
            role: yunong_shared::Role::User,
            content: "查课表".to_owned(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }];
        config
    }

    /// 一个带 `echo` 处理的 registry（工具循环测试用）。
    fn echo_registry() -> crate::tools::ToolRegistry {
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register("timetable_query", |_args| Ok("第 3 周有课".to_owned()));
        registry
    }

    /// 三协议下 `tool_choice=required` 的实际写法（`build_body_with`）。
    #[test]
    fn tool_choice_is_encoded_per_protocol() {
        use yunong_shared::Protocol;
        for (protocol, expected) in [
            (Protocol::OpenaiCompletions, serde_json::json!("required")),
            (Protocol::OpenaiResponses, serde_json::json!("required")),
            (
                Protocol::AnthropicMessages,
                serde_json::json!({ "type": "any" }),
            ),
        ] {
            let mut request = config_with_request(serde_json::json!({
                "db_path": "/tmp/app.db",
                "protocol": crate::protocol_pick::protocol_name(protocol),
            }))
            .to_request();
            request.tools = crate::tools::specs_for(&["timetable_query".to_owned()]);
            let options = crate::protocols::BuildOptions {
                tool_choice: Some(crate::tool_choice::ToolChoice::Required),
                ..Default::default()
            };
            let body = crate::protocols::build_body_with(protocol, &request, &options);
            println!(
                "[tool_choice:{}] {}",
                crate::protocol_pick::protocol_name(protocol),
                body["tool_choice"]
            );
            assert_eq!(body["tool_choice"], expected, "{body}");
        }
    }

    /// 端到端：首轮请求体带 `tool_choice`，**第二轮不带**（不逼模型反复调工具）。
    #[test]
    fn tool_choice_goes_out_on_the_first_round_only() {
        let server = ScriptedServer::start(vec![
            tool_round("timetable_query", "call_1", "{\"week\":3}"),
            final_round("第 3 周有课"),
        ]);
        let config = config_with_tool_choice(&server.base_url(), "required");
        let chunks = run_collect(config, echo_registry());
        let bodies = server.bodies.lock().unwrap().clone();
        assert_eq!(bodies.len(), 2, "两轮都该发出去");
        assert!(
            bodies[0].contains("\"tool_choice\":\"required\""),
            "首轮必须带 tool_choice：{}",
            bodies[0]
        );
        assert!(
            !bodies[1].contains("tool_choice"),
            "第二轮不该再强制：{}",
            bodies[1]
        );
        // 模型这次确实调了工具 → done 里 honored = true
        let done = parse_all(&chunks)
            .into_iter()
            .find(|event| event["type"] == "done")
            .unwrap();
        println!("[done] {done}");
        assert_eq!(done["tool_choice_honored"], true, "{done}");
    }

    /// 强制了但模型仍然只回文本：**不重试**，done 标注 `tool_choice_honored=false`。
    #[test]
    fn required_but_text_only_marks_not_honored_without_retry() {
        let server = ScriptedServer::start(vec![final_round("第 3 周有课")]);
        let config = config_with_tool_choice(&server.base_url(), "required");
        let chunks = run_collect(config, echo_registry());
        assert_eq!(server.request_count(), 1, "不得因为没调工具而重试");
        let events = parse_all(&chunks);
        let done = events.iter().find(|event| event["type"] == "done").unwrap();
        println!("[done] {done}");
        assert_eq!(done["tool_choice_honored"], false, "{done}");
        // meta 里回报请求值与实际生效值
        let meta = &events[0];
        println!("[meta] {meta}");
        assert_eq!(meta["tool_choice"], "required", "{meta}");
        assert_eq!(meta["tool_choice_effective"], "required", "{meta}");
    }

    /// 未识别的取值 → 按 auto 处理（不发字段），并在 meta 里如实回报。
    #[test]
    fn unknown_tool_choice_falls_back_to_auto() {
        let server = ScriptedServer::start(vec![final_round("你好")]);
        let config = config_with_tool_choice(&server.base_url(), "must");
        let chunks = run_collect(config, echo_registry());
        let bodies = server.bodies.lock().unwrap().clone();
        assert!(!bodies[0].contains("tool_choice"), "{}", bodies[0]);
        let meta = parse_all(&chunks).into_iter().next().unwrap();
        println!("[meta] {meta}");
        assert_eq!(meta["tool_choice"], "auto", "{meta}");
        assert_eq!(meta["tool_choice_effective"], "auto", "{meta}");
    }

    /// t118：`done` 帧带 `effective_protocol`——**最终实际成功**的协议（权威值）。
    #[test]
    fn done_frame_carries_the_final_effective_protocol() {
        // 正常路径：等于 meta 里的值
        let server = ScriptedServer::start(vec![final_round("你好")]);
        let config = byok_config(&server.base_url(), None);
        let events = parse_all(&run_collect(config, crate::tools::ToolRegistry::new()));
        let done = events
            .iter()
            .find(|event| event["type"] == "done")
            .expect("必须有 done");
        println!("[done] {done}");
        assert_eq!(done["effective_protocol"], "openai_completions", "{done}");

        // 运行时回退后再成功：done 必须反映**回退后**的协议（不是 meta 里的初判）
        let server = ScriptedServer::start_with_status(vec![
            (
                404,
                "{\"error\":{\"message\":\"no such endpoint\"}}".to_owned(),
            ),
            (200, responses_body("下午没课")),
        ]);
        let config = config_with_protocol(&server.base_url(), "openai_completions");
        let events = parse_all(&run_collect(config, crate::tools::ToolRegistry::new()));
        let meta = &events[0];
        let done = events
            .iter()
            .find(|event| event["type"] == "done")
            .expect("必须有 done");
        println!("[meta] {meta}");
        println!("[done] {done}");
        assert_eq!(meta["effective_protocol"], "openai_completions", "{meta}");
        assert_eq!(
            done["effective_protocol"], "openai_responses",
            "done 必须是最终成功的协议：{done}"
        );
    }

    /// meta 帧在真实流里带上 requested/effective 两个字段。
    #[test]
    fn meta_reports_both_protocols() {
        let server = ScriptedServer::start(vec![final_round("你好")]);
        let config = byok_config(&server.base_url(), None);
        let chunks = run_collect(config, crate::tools::ToolRegistry::new());
        let meta = parse_all(&chunks).into_iter().next().unwrap();
        println!("[meta] {meta}");
        assert_eq!(meta["type"], "meta");
        assert!(meta["requested_protocol"].is_string(), "{meta}");
        assert!(meta["effective_protocol"].is_string(), "{meta}");
        assert_eq!(meta["effective_protocol"], "openai_completions", "{meta}");
    }

    /// t109 回归：**空结果**与**有结果**两条工具路径都必须收到 `done`。
    ///
    /// 之前的用例只覆盖了"工具有返回"，而真实事故恰恰是"工具有返回 ⇒ 流不结束"。
    /// 这条用例把两条路径并排跑一遍，任何一条丢 `done` 都会红。
    #[test]
    fn both_empty_and_non_empty_tool_results_end_with_done() {
        for (label, content) in [
            ("空结果", ""),
            ("有结果", "「雨伞」找到 1 条本地记忆：\n- 雨伞是黑色的"),
        ] {
            let server = ScriptedServer::start(vec![
                tool_round("echo", "call_done", "{\"query\":\"雨伞\"}"),
                final_round("好"),
            ]);
            let mut registry = crate::tools::ToolRegistry::new();
            let payload = content.to_owned();
            registry.register("echo", move |_args| Ok(payload.clone()));
            let chunks = run_collect(byok_config(&server.base_url(), Some(2_000)), registry);
            let events = parse_all(&chunks);

            let results = results_of(&events);
            assert_eq!(results.len(), 1, "{label}：{chunks:#?}");
            assert_eq!(results[0]["ok"], true, "{label}：{results:?}");
            assert_eq!(
                events
                    .last()
                    .map(|event| event["type"].as_str().unwrap_or_default()),
                Some("done"),
                "{label}：最后一条必须是 done（工具事件之后流必须收尾）：{chunks:#?}"
            );
            assert!(
                events.iter().any(|event| event["type"] == "delta"),
                "{label}：第二轮文本应作为 delta 到达"
            );
            // 配对
            assert_eq!(calls_of(&events).len(), results.len(), "{label}");
        }
    }

    /// 工具失败：`ok:false` + 可读原因 + code。
    #[test]
    fn tool_failure_carries_a_code() {
        let server = ScriptedServer::start(vec![
            tool_round("boom", "call_2", "{}"),
            final_round("抱歉"),
        ]);
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register("boom", |_args| {
            Err(crate::error::CoreError::Invalid("参数缺失".into()))
        });
        let chunks = run_collect(byok_config(&server.base_url(), Some(2_000)), registry);
        let events = parse_all(&chunks);
        let results = results_of(&events);
        assert_eq!(results.len(), 1, "{chunks:#?}");
        assert_eq!(results[0]["ok"], false);
        assert_eq!(results[0]["code"], "BAD_ARGS", "{}", results[0]);
        assert!(!results[0]["summary"].as_str().unwrap().is_empty());
    }

    /// 超时：慢工具 → `TIMEOUT`，且**不拖住整条流**（后面仍有 done）。
    #[test]
    fn slow_tool_times_out_without_wedging_the_stream() {
        let server = ScriptedServer::start(vec![
            tool_round("slow", "call_3", "{}"),
            final_round("超时了"),
        ]);
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register("slow", |_args| {
            std::thread::sleep(std::time::Duration::from_millis(600));
            Ok("太慢了".to_owned())
        });
        let started = std::time::Instant::now();
        let chunks = run_collect(byok_config(&server.base_url(), Some(150)), registry);
        let elapsed = started.elapsed();
        let events = parse_all(&chunks);
        let results = results_of(&events);
        assert_eq!(results.len(), 1, "{chunks:#?}");
        assert_eq!(results[0]["ok"], false);
        assert_eq!(results[0]["code"], "TIMEOUT", "{}", results[0]);
        assert!(results[0]["summary"].as_str().unwrap().contains("超时"));
        assert_eq!(events.last().unwrap()["type"], "done", "流必须正常收尾");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "超时不该拖住流：{elapsed:?}"
        );
    }

    /// 取消：收到 `tool_call` 之后调用方要求停 → 必须补 `CANCELLED` 的 result，不留悬空。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_after_tool_call_closes_the_pair() {
        let server = ScriptedServer::start(vec![
            tool_round("echo", "call_4", "{}"),
            final_round("不该走到"),
        ]);
        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = std::sync::Arc::clone(&chunks);
        let config = byok_config(&server.base_url(), Some(2_000));
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register("echo", |_args| Ok("结果".to_owned()));
        tokio::task::spawn_blocking(move || {
            run_stream_with_registry(config, registry, move |json| {
                let mut guard = sink.lock().unwrap();
                let is_call = json.contains("\"tool_call\"");
                guard.push(json);
                // 看到 tool_call 就取消（模拟用户点"停止"）
                !is_call
            })
            .unwrap();
        })
        .await
        .unwrap();
        let guard = chunks.lock().unwrap().clone();
        let events = parse_all(&guard);
        let calls = calls_of(&events);
        let results = results_of(&events);
        assert_eq!(calls.len(), 1, "{guard:#?}");
        assert_eq!(results.len(), 1, "取消后必须补一条 result：{guard:#?}");
        assert_eq!(results[0]["code"], "CANCELLED", "{}", results[0]);
        assert_eq!(events.last().unwrap()["type"], "done");
    }

    /// **最重要的一条**：任何路径下 `tool_call` 数量 == `tool_result` 数量。
    #[test]
    fn never_leaves_a_dangling_tool_call() {
        // 场景 A：正常
        let server =
            ScriptedServer::start(vec![tool_round("echo", "call_a", "{}"), final_round("好")]);
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register("echo", |_args| Ok("ok".to_owned()));
        let events = parse_all(&run_collect(
            byok_config(&server.base_url(), Some(2_000)),
            registry,
        ));
        assert_eq!(
            calls_of(&events).len(),
            results_of(&events).len(),
            "正常路径：{events:#?}"
        );

        // 场景 B：工具失败
        let server = ScriptedServer::start(vec![
            tool_round("boom", "call_b", "{}"),
            final_round("抱歉"),
        ]);
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register("boom", |_args| {
            Err(crate::error::CoreError::Invalid("坏了".into()))
        });
        let events = parse_all(&run_collect(
            byok_config(&server.base_url(), Some(2_000)),
            registry,
        ));
        assert_eq!(
            calls_of(&events).len(),
            results_of(&events).len(),
            "失败路径：{events:#?}"
        );

        // 场景 C：上游第二轮直接断开（工具执行后，下一轮请求失败）
        let server = ScriptedServer::start(vec![tool_round("echo", "call_c", "{}")]);
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register("echo", |_args| Ok("ok".to_owned()));
        let events = parse_all(&run_collect(
            byok_config(&server.base_url(), Some(2_000)),
            registry,
        ));
        assert_eq!(
            calls_of(&events).len(),
            results_of(&events).len(),
            "上游错误路径也必须配对：{events:#?}"
        );
    }
}
