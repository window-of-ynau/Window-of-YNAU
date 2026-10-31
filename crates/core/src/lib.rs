//! 云农之窗 · 端侧核心
//!
//! 这件事是**胖核心**：编排、记忆、上下文压缩、检索、日历全部在这里，
//! Flutter 只做渲染（ADR-07）。核心不依赖网络，也不依赖 UI——所以它能被单元测试覆盖。
//!
//! 敏感数据边界：本 crate 的 `keystore` 是**唯一**接触 Cookie / API Key 的地方。

#![forbid(unsafe_code)]

pub mod calendar;
pub mod context;
pub mod error;
pub mod gateway;
pub mod index;
pub mod keystore;
pub mod memory;
pub mod parse;
pub mod protocols;
pub mod tools;

pub use error::{CoreError, CoreResult};

/// 核心版本（随 crate 版本走）。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 常用导入。
pub mod prelude {
    pub use crate::calendar::{parse_weeks, Course, Occurrence, PeriodTime};
    pub use crate::context::{estimate_tokens, plan_compaction, should_compress, CompactionPlan};
    pub use crate::error::{CoreError, CoreResult};
    pub use crate::gateway::{CloudProxy, DirectProvider, ModelGateway};
    pub use crate::index::{Bm25Index, LocalIndex, SearchHit};
    pub use crate::keystore::{SecretKind, SecretStore};
    pub use crate::memory::{Blob, BlobKind, MemoryStore, Message};
    pub use crate::parse::{parse_timetable, ParseError};
    pub use crate::protocols::{build_body, parse_event};
    pub use crate::tools::{ToolCall, ToolOutcome, ToolRegistry};
}
