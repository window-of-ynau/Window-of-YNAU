//! 云农之窗 · 端侧核心
//!
//! 这件事是**胖核心**：编排、记忆、上下文压缩、检索、日历全部在这里，
//! Flutter 只做渲染（ADR-07）。核心能被单元测试覆盖：**唯一的网络出口是
//! [`gateway`]**（BYOK 直连 / 套餐代理），其余模块都是纯本地逻辑。
//!
//! 敏感数据边界：本 crate 的 `keystore` 是**唯一**接触 Cookie / API Key 的地方；
//! `gateway` 只在发请求前取一次 Key，用完即弃（`ApiKeySource::Provider`）。

#![deny(unsafe_code)]

pub mod auth;
pub mod backfill;
pub mod calendar;
pub mod chat_ffi;
pub mod compact;
pub mod context;
pub mod error;
pub mod gateway;
pub mod ics;
pub mod index;
pub mod keystore;
pub mod local_search;
pub mod memory;
pub mod memory_items;
pub mod memory_ref;
pub mod parse;
pub mod protocol_pick;
pub mod protocols;
pub mod schedule;
pub mod search;
pub mod secret;
pub mod store;
pub mod titlebar;
pub mod tool_events;
pub mod tool_loop;
pub mod tools;

pub use error::{CoreError, CoreResult};

/// C ABI 出口（Flutter `dart:ffi` 调这里）。
///
/// 本 crate 其余部分仍然 `deny(unsafe_code)`；`unsafe` 只在这个模块里解包裸指针。
#[allow(unsafe_code)]
pub mod ffi;

/// 核心版本（随 crate 版本走）。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 常用导入。
pub mod prelude {
    pub use crate::auth::{
        refresh_blocking, refresh_with, AuthRefreshError, RefreshTimeouts, Refreshed, ACCESS_KEY,
        REFRESH_KEY,
    };
    pub use crate::calendar::{parse_term_start, parse_weeks, Course, IcsEvent};
    pub use crate::compact::{
        compact, pressure_tokens, retain_tokens, should_compact, should_compact_on_model_switch,
        summarize, CompactConfig, Outcome, Plan as CompactPlan, Summary, Trigger,
    };
    pub use crate::context::{estimate_tokens, plan_compaction, should_compress, CompactionPlan};
    pub use crate::error::{CoreError, CoreResult};
    pub use crate::gateway::{
        collect_chunks, ApiKeySource, CloudProxy, DirectProvider, Flow, ModelGateway,
        StreamOutcome, Timeouts,
    };
    pub use crate::ics::{build_events, export_term_ics, IcsExport};
    pub use crate::index::{Bm25Index, LocalIndex, SearchHit};
    pub use crate::keystore::{SecretKind, SecretStore};
    pub use crate::local_search::{
        SearchDoc, SearchResult, SearchStats, SearchStore, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT,
        SNIPPET_RADIUS,
    };
    pub use crate::memory::{Blob, BlobKind, MemoryStore, Message};
    pub use crate::memory_items::{
        MemoryDraft, MemoryItem, MemoryItemStore, MemoryKind, MemoryWrite,
    };
    pub use crate::parse::{parse_timetable, parse_timetable_with_report, ParseError, ParseReport};
    pub use crate::protocols::{build_body, build_body_with, parse_events, BuildOptions};
    pub use crate::schedule::{
        compress_weeks, merge, MergedSchedule, SchedulePayload, WeekPayload,
    };
    pub use crate::store::{CourseStore, ImportOutcome, StoredCourse, TermRecord};
    pub use crate::titlebar::{set_dark as set_titlebar_dark, TitlebarError, TitlebarReport};
    pub use crate::tool_loop::{run as run_tool_loop, LoopConfig, LoopEvent, LoopOutcome};
    pub use crate::tools::{ToolCall, ToolOutcome, ToolRegistry};
}
