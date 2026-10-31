//! 云农之窗 · 契约层（唯一事实来源）
//!
//! 端（Flutter/Rust core）、云（axum）、OpenAPI、文档全部从这里生成。
//! **任何协议改动先改这里**，再改实现（见 `docs/04-ADR.md` ADR-13）。

#![forbid(unsafe_code)]

pub mod entitlement;
pub mod error;
pub mod protocol;

pub use entitlement::{Channel, Entitlement};
pub use error::{ApiError, ErrorBody, ErrorCode, ErrorEnvelope};
pub use protocol::{
    ChatMessage, ChatRequest, Chunk, FinishReason, Protocol, Role, ToolSpec, Usage,
};

/// 单轮输入的 token 上限：保护毛利，也防止一次长上下文烧穿预算（ADR-03）。
pub const MAX_INPUT_TOKENS: u32 = 32_000;

/// 一次用户提问最多允许几轮工具循环（ADR-05）。
pub const MAX_ROUNDS: u8 = 4;

/// 上下文的压缩触发比例（ADR-05 / 产品需求 FR-E2）。
pub const COMPRESS_TRIGGER_RATIO: f32 = 0.55;
