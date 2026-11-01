//! 流式对话的 FFI 支撑层（W7 P0）：配置解析 → 跑一次 `chat_stream` → 每个 Chunk 序列化回调。
//!
//! # 为什么需要这一层
//!
//! `gateway` 是纯 Rust 的流式 trait，Dart 侧要"边生成边显示"，必须走**回调**而不是
//! "调用→返回 JSON"。FFI 函数本体在 `ffi.rs`，这里放可单测的逻辑：
//! - [`ChatRunConfig`]：Dart 传的 JSON 配置（通道 / 协议 / 模型 / Key / 消息）；
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
use yunong_shared::{ChatMessage, ChatRequest, Chunk, ErrorCode, Protocol};

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
            tools: Vec::new(),
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

/// 跑一次流：每个 Chunk 交给 `on_chunk`（返回 false = 调用方要求取消）。
///
/// 返回的 `Err` **只表示"没能开始"**（配置错误、运行时没建起来）；一旦开始，
/// 流内错误一律以 `Chunk::Error` 交给回调。
pub fn run_stream(
    config: ChatRunConfig,
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
    let request = config.to_request();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| (ErrorCode::Internal, format!("运行时创建失败：{err}")))?;

    let mut cancelled = false;
    let mut callback = SendableClosure(on_chunk);
    // 返回"是否已被取消"，避免外层闭包再借 `cancelled`（会与 emit 的可变借用冲突）
    let mut emit = |chunk: Chunk| -> bool {
        let json = serde_json::to_string(&chunk)
            .unwrap_or_else(|_| error_chunk_json(ErrorCode::Internal, "序列化失败"));
        if !callback.call(json) {
            cancelled = true;
        }
        cancelled
    };

    let outcome = match config.channel.as_str() {
        "cloud" => {
            let gateway = crate::gateway::CloudProxy::new(
                config.cloud_base_url.clone().unwrap_or_default(),
                config.cloud_token.clone().unwrap_or_default(),
            );
            runtime.block_on(gateway.chat_stream(&request, &mut |chunk| {
                if emit(chunk) {
                    crate::gateway::Flow::Stop
                } else {
                    crate::gateway::Flow::Continue
                }
            }))
        }
        _ => {
            let gateway = crate::gateway::DirectProvider::new(
                parse_protocol(&config.protocol),
                config.base_url.clone().unwrap_or_default(),
                config.model.clone(),
            )
            .with_api_key(api_key.clone());
            runtime.block_on(gateway.chat_stream(&request, &mut |chunk| {
                if emit(chunk) {
                    crate::gateway::Flow::Stop
                } else {
                    crate::gateway::Flow::Continue
                }
            }))
        }
    };

    match outcome {
        Ok(_) => Ok(()),
        Err(err) => {
            let (code, message) = map_core_error(err);
            let _ = callback.call(error_chunk_json(code, message));
            Ok(())
        }
    }
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
}
