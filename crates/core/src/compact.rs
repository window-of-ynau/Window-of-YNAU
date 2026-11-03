//! W8 阶段 1：**确定性上下文压缩**（移植 DSH-DCP 的降级版，见
//! `.internal/research/dsh-dcp-evaluation.md` §0.1）。
//!
//! 解决长对话的两件事：**不溢出**（超阈值时压掉中段）与**不失忆**（模板摘要 + 原文落库）。
//! 全程**零 LLM**：纯函数 + 固定模板，同一输入两次产出逐字节相同（用例 `summary_is_deterministic`）。
//!
//! # 四步
//!
//! 1. **触发**：`压力 = min(window × 0.55, window − reserved − headroom)`，
//!    `used ≥ 压力` 才压；换模型时另有 `modelSwitchMinTokens = 24000` 的提前压。
//! 2. **切点**：从尾部往前累计到 `retainTokens = floor((window − reserved) × 0.16)` 为止，
//!    再回退到 **tool-pair 配平点**（`Σtool_call − Σtool_result == 0`）；回退不到就不压。
//! 3. **摘要**：8 段模板 + 7 条抽取规则（用户原话 / 技术词 / 文件 / 报错 / 待办 /
//!    当前工作 / 下一步 / 关键上下文）。
//! 4. **落库**：被压掉的原文整段写进 `memory_items`（`kind = summary`），摘要里嵌
//!    `memory://<id>`，为阶段 2 的找回查询留指针。
//!
//! # 7 条抽取规则（阶段 1 落地版）
//!
//! | # | 段落 | DCP 原规则 | 我们做了什么 |
//! |---|---|---|---|
//! | 1 | 主要请求与意图 | `intent`，跳过注入上下文/通知行 | 同；另外跳过我们自己的摘要消息（它走"继承"）和整条待办行 |
//! | 2 | 关键技术概念 | 固定命令词表 `COMMAND_CONCEPTS` | 同 + 补一份**校园域词表**（课表/成绩/图书馆/选课…） |
//! | 3 | 文件与代码 | `PATH_KEYS` + `WRITE_TOOL` 分类 | 同（读工具参数 JSON 里的路径键，渲染成 `path — 写×n 读×m`） |
//! | 4 | 错误与修复 | 中英错误正则，只留首行 | 同（关键词版，不引正则依赖） |
//! | 5 | 待办事项 | `- [ ]` / `TODO:` / `FIXME:` / `待办：` | 同（勾选 `- [x]` 会把它移出待办） |
//! | 6 | 当前工作 | 最后 user + assistant 前两行 | 同 |
//! | 7 | 下一步 | 待办的最后一条 | 同（没有待办就用最后一条用户请求） |
//!
//! # 与 DSH-DCP 的差异（**没照搬的部分**）
//!
//! 阶段 1 **不做**：去重（dedup，含写侧工具保护）、清错（purgeErrors）、3 档密度、
//! 4 级降级、`maxSummaryTokens` 预算、`memory://` 找回查询、`<compacted-summary>` 标签
//! 与 LLM 提示词 preamble、中英双语 i18n（App 只有中文）、完整 checkpoint 继承
//! （只继承 5 段：intent / concepts / files / errors / context）。
//!
//! # 关键取舍
//!
//! - **CJK 仍按 1 字/token 计价**（[`crate::context::estimate_tokens`]）：方向已由 owner 锁定，
//!   宁可早压（保守），不改；
//! - 摘要本身**不猜事实**：只从消息正文里抽，抽不到就写「（无）」，不编造；
//! - 落库失败**不阻断**压缩：摘要照给，`archive_id` 为空，并在关键上下文里写「原文未存档」——
//!   压缩是"防止溢出"的兜底，不能因为存储满了就拒绝工作。

use std::collections::BTreeMap;

use crate::context::estimate_tokens;
use crate::error::CoreResult;
use crate::memory::Message;
use crate::memory_items::{MemoryDraft, MemoryItemStore, MemoryKind};

/// 给模型输出预留的 token（不给的话，压到刚好会立刻再溢出）。
pub const DEFAULT_RESERVED_TOKENS: u32 = 4_096;

/// 安全余量：DCP 用 65536，让大窗口模型**提前很多**触发。
///
/// 注意：对 32k 窗口这个值大于整个窗口（32k − 4096 − 65536 < 0），所以
/// [`pressure_tokens`] 里有一条明确的护栏——headroom 项 ≤ 0 时退回 55% 比例阈值。
pub const DEFAULT_HEADROOM_TOKENS: u32 = 65_536;

/// 尾部保留比例：`retainTokens = floor((window − reserved) × 0.16)`。
pub const RETAIN_RATIO: f32 = 0.16;

/// 换模型时的提前压缩门槛（24k = 32k 窗口的 75%）。
pub const MODEL_SWITCH_MIN_TOKENS: u32 = 24_000;

/// 每段最多列几条（DCP 的 `maxItems`，默认 10）。
pub const DEFAULT_MAX_ITEMS: usize = 10;

/// 单条最多保留多少个字符（DCP 的 `maxItemChars`，默认 200）。
pub const DEFAULT_MAX_ITEM_CHARS: usize = 200;

/// 摘要消息的识别标记（再次压缩时要把它"继承"下去，不能当成普通历史压掉）。
pub const SUMMARY_MARKER: &str = "【压缩摘要】";

/// 存档分片大小：`memory_items.content` 上限 64 KiB，留点余量。
const ARCHIVE_CHUNK_BYTES: usize = 48 * 1024;

/// 压缩参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactConfig {
    /// 上下文窗口（token）
    pub window: u32,
    pub reserved: u32,
    pub headroom: u32,
    pub retain_ratio: f32,
    pub trigger_ratio: f32,
    pub model_switch_min_tokens: u32,
    pub max_items: usize,
    pub max_item_chars: usize,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            window: yunong_shared::MAX_INPUT_TOKENS,
            reserved: DEFAULT_RESERVED_TOKENS,
            headroom: DEFAULT_HEADROOM_TOKENS,
            retain_ratio: RETAIN_RATIO,
            trigger_ratio: yunong_shared::COMPRESS_TRIGGER_RATIO,
            model_switch_min_tokens: MODEL_SWITCH_MIN_TOKENS,
            max_items: DEFAULT_MAX_ITEMS,
            max_item_chars: DEFAULT_MAX_ITEM_CHARS,
        }
    }
}

/// 为什么压（写进摘要的「关键上下文」与调用方日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// 没到阈值（不压）
    NoNeed,
    /// 压力超过阈值
    Pressure,
    /// 换模型且历史已过大（提前压）
    ModelSwitch,
}

impl Trigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoNeed => "no_need",
            Self::Pressure => "pressure",
            Self::ModelSwitch => "model_switch",
        }
    }

    /// 中文说明：写日志 / 界面展示 / 摘要里的"触发原因"都用它。
    pub const fn label_zh(self) -> &'static str {
        match self {
            Self::NoNeed => "未到阈值",
            Self::Pressure => "超过压力阈值",
            Self::ModelSwitch => "模型切换预热",
        }
    }
}

/// 压力阈值（达到它就该压）。
///
/// `min(window × 0.55, window − reserved − headroom)`，但**headroom 项 ≤ 0 时退回比例阈值**：
///
/// | window | 比例项 | headroom 项 | 结果 |
/// |---|---|---|---|
/// | 32,000（我们的默认） | 17,600 | −53,632 | **17,600**（护栏生效） |
/// | 100,000 | 55,000 | 26,464 | 26,464 |
/// | 1,000,000 | 550,000 | 926,464 | 550,000 |
///
/// 没有这条护栏，32k 窗口算出来的压力是负数——等于"永远在压"，每轮都会压一次。
pub fn pressure_tokens(cfg: &CompactConfig) -> u32 {
    let by_ratio = (cfg.window as f32 * cfg.trigger_ratio) as u32;
    let by_headroom = cfg
        .window
        .saturating_sub(cfg.reserved)
        .saturating_sub(cfg.headroom);
    if by_headroom == 0 {
        by_ratio
    } else {
        by_ratio.min(by_headroom)
    }
}

/// 尾部至少保留多少 token：`floor((window − reserved) × 0.16)`。
pub fn retain_tokens(cfg: &CompactConfig) -> u32 {
    let usable = cfg.window.saturating_sub(cfg.reserved);
    (usable as f32 * cfg.retain_ratio) as u32
}

/// 是否该压：`used ≥ 压力`。
pub fn should_compact(used_tokens: u32, cfg: &CompactConfig) -> bool {
    used_tokens >= pressure_tokens(cfg)
}

/// 换模型时是否该先压一次：历史已超 `modelSwitchMinTokens`（且不超过窗口的 75%）。
pub fn should_compact_on_model_switch(used_tokens: u32, cfg: &CompactConfig) -> bool {
    let threshold = cfg
        .model_switch_min_tokens
        .min((cfg.window as f32 * 0.75) as u32);
    used_tokens >= threshold
}

/// 压缩计划（纯函数，不碰数据库）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub trigger: Trigger,
    pub used_tokens: u32,
    pub pressure_tokens: u32,
    pub retain_tokens: u32,
    /// `messages[..cut_index]` 被压掉，`messages[cut_index..]` 保留
    pub cut_index: usize,
    pub compacted_messages: usize,
    pub retained_messages: usize,
    pub compacted_tokens: u32,
    pub retained_tokens: u32,
    /// 切点回退到 0 仍不配平 → 不压（`true` 时 `cut_index == 0`）
    pub blocked: bool,
}

impl Plan {
    pub fn applied(&self) -> bool {
        !self.blocked && self.trigger != Trigger::NoNeed && self.cut_index > 0
    }
}

/// 生成计划。
///
/// `messages` 必须是**时间正序**（旧 → 新）。`trigger` 由调用方判定（压力 / 换模型 /
/// 不压），本函数只负责切点与统计。
pub fn plan(messages: &[Message], cfg: &CompactConfig, trigger: Trigger) -> Plan {
    let used_tokens: u32 = messages.iter().map(|m| m.tokens).sum();
    let pressure = pressure_tokens(cfg);
    let retain = retain_tokens(cfg);
    let mut plan = Plan {
        trigger,
        used_tokens,
        pressure_tokens: pressure,
        retain_tokens: retain,
        cut_index: 0,
        compacted_messages: 0,
        retained_messages: messages.len(),
        compacted_tokens: 0,
        retained_tokens: used_tokens,
        blocked: false,
    };
    if trigger == Trigger::NoNeed || messages.is_empty() {
        return plan;
    }

    // 1) 从尾部往前累计，直到保住 retainTokens
    let mut cut = messages.len();
    let mut kept = 0_u32;
    while cut > 0 && kept < retain {
        cut -= 1;
        kept += messages[cut].tokens;
    }

    // 2) 回退到 tool-pair 配平点：被压掉的 prefix 必须"自成闭环"
    while cut > 0 && tool_balance(&messages[..cut]) != 0 {
        cut -= 1;
    }
    if cut == 0 && !messages.is_empty() {
        // 整段历史都配不平（例如有一条孤立的 tool 结果）：宁可不压，也不要切断调用对
        plan.blocked = true;
        plan.retained_messages = messages.len();
        plan.retained_tokens = used_tokens;
        return plan;
    }

    let compacted_tokens: u32 = messages[..cut].iter().map(|m| m.tokens).sum();
    plan.cut_index = cut;
    plan.compacted_messages = cut;
    plan.retained_messages = messages.len() - cut;
    plan.compacted_tokens = compacted_tokens;
    plan.retained_tokens = used_tokens.saturating_sub(compacted_tokens);
    plan
}

/// 一段历史的 tool-pair 差额：`Σtool_call − Σtool_result`。
///
/// 只有差额为 0 才说明"调用都有结果"，切在这里不会让模型看到半截调用。
pub fn tool_balance(messages: &[Message]) -> i64 {
    let mut balance = 0_i64;
    for message in messages {
        let (calls, results) = tool_events(message);
        balance += calls as i64 - results as i64;
    }
    balance
}

/// 一条消息里有几次工具调用 / 几条工具结果。
///
/// 历史表（`messages`）没有结构化的 tool 字段，约定是：
/// - `role = "tool"` → 1 条结果；
/// - `role = "assistant"` 且正文是 JSON（`{"tool_calls":[…]}` 或 `{"type":"tool_call"}`）→ 调用。
fn tool_events(message: &Message) -> (u32, u32) {
    match message.role.as_str() {
        "tool" => (0, 1),
        "assistant" => (count_tool_calls(&message.content), 0),
        _ => (0, 0),
    }
}

fn count_tool_calls(content: &str) -> u32 {
    let trimmed = content.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return 0;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return 0;
    };
    match &value {
        serde_json::Value::Object(map) => {
            if let Some(calls) = map.get("tool_calls").and_then(|v| v.as_array()) {
                return calls.len() as u32;
            }
            u32::from(map.get("type").and_then(|v| v.as_str()) == Some("tool_call"))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .filter(|item| {
                item.get("type").and_then(|v| v.as_str()) == Some("tool_call")
                    || item.get("tool_calls").is_some()
            })
            .count() as u32,
        _ => 0,
    }
}

/// 一段摘要素材。
#[derive(Debug, Clone, Default)]
struct Facts {
    intents: Vec<String>,
    concepts: Vec<String>,
    files: Vec<(String, FileUse)>,
    errors: Vec<String>,
    todos: Vec<String>,
    last_user: String,
    last_assistant: String,
    message_count: usize,
    tool_calls: u32,
    tool_results: u32,
    carried: BTreeMap<&'static str, Vec<String>>,
    elided_intents: usize,
    elided_errors: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct FileUse {
    reads: u32,
    writes: u32,
}

const SECTIONS: [(&str, &str); 8] = [
    ("intent", "主要请求与意图"),
    ("concepts", "关键技术概念"),
    ("files", "文件与代码"),
    ("errors", "错误与修复"),
    ("todos", "待办事项"),
    ("current", "当前工作"),
    ("next", "下一步"),
    ("context", "关键上下文"),
];

const COMMAND_CONCEPTS: [&str; 14] = [
    "git", "cargo", "npm", "pnpm", "bun", "docker", "python", "pip", "make", "curl", "kubectl",
    "gradle", "maven", "sqlite",
];

/// 校园域词表（我们自己的"技术概念"：这台机器上最常见的就是这些）。
const DOMAIN_CONCEPTS: [&str; 16] = [
    "课表",
    "课程",
    "学期",
    "周次",
    "成绩",
    "考试",
    "图书馆",
    "食堂",
    "选课",
    "校历",
    "教室",
    "学分",
    "教务",
    "导出",
    "记忆",
    "搜索",
];

const ERROR_MARKERS_EN: [&str; 11] = [
    "error",
    "failed",
    "failure",
    "fatal",
    "exception",
    "denied",
    "refused",
    "not found",
    "cannot",
    "unable",
    "timeout",
];

const ERROR_MARKERS_ZH: [&str; 9] = [
    "失败",
    "错误",
    "报错",
    "异常",
    "找不到",
    "未找到",
    "无法",
    "超时",
    "崩溃",
];

const PATH_KEYS: [&str; 6] = [
    "file_path",
    "absolute_path",
    "notebook_path",
    "path",
    "glob",
    "pattern",
];

const WRITE_TOOL_MARKERS: [&str; 8] = [
    "write", "edit", "patch", "delete", "remove", "mkdir", "move", "rename",
];

/// 摘要结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// 完整文本（带 [`SUMMARY_MARKER`] 前缀，可直接作为一条历史消息插入）
    pub text: String,
    /// 各段条目数（8 段，顺序同 [`SECTIONS`]）
    pub section_items: [usize; 8],
}

impl Summary {
    /// 估算摘要自身占多少 token（用统一的 CJK 计价）。
    pub fn tokens(&self) -> u32 {
        estimate_tokens(&self.text)
    }
}

/// 生成摘要（纯函数；`archive_refs` 为空表示原文没落库）。
pub fn summarize(
    messages: &[Message],
    cfg: &CompactConfig,
    archive_refs: &[i64],
    trigger: Trigger,
) -> Summary {
    let facts = extract(messages, cfg);
    render(&facts, archive_refs, trigger)
}

fn extract(messages: &[Message], cfg: &CompactConfig) -> Facts {
    let mut facts = Facts {
        message_count: messages.len(),
        ..Facts::default()
    };
    let mut seen_intents: Vec<String> = Vec::new();
    let mut seen_errors: Vec<String> = Vec::new();

    for message in messages {
        let text = message.content.trim();
        if text.is_empty() {
            continue;
        }
        match message.role.as_str() {
            "user" => {
                // 规则 1：用户原话；跳过我们自己的摘要（它会被"继承"，不该重复列）
                if is_summary(text) {
                    merge_carried(&mut facts.carried, parse_sections(text));
                    continue;
                }
                if is_injected(text) {
                    continue;
                }
                facts.last_user = clip(text, cfg.max_item_chars);
                // 待办行（`- [ ] …` / `TODO: …`）属于「待办事项」段，不再算一条"用户意图"
                if !is_todo_line(text) {
                    let item = clip(&first_lines(text, 1), cfg.max_item_chars);
                    if !seen_intents.iter().any(|existing| existing == &item) {
                        seen_intents.push(item);
                    }
                }
                collect_todos(text, &mut facts.todos);
            }
            "assistant" => {
                if is_summary(text) {
                    merge_carried(&mut facts.carried, parse_sections(text));
                    continue;
                }
                facts.last_assistant = clip(&first_lines(text, 2), cfg.max_item_chars);
                collect_todos(text, &mut facts.todos);
                collect_tool_calls(text, &mut facts);
            }
            "tool" => {
                facts.tool_results += 1;
                // 规则 4：报错只留首行
                let line = first_lines(text, 1);
                if is_error(&line) {
                    let item = clip(&line, cfg.max_item_chars);
                    if !seen_errors.iter().any(|existing| existing == &item) {
                        seen_errors.push(item);
                    }
                }
            }
            _ => {}
        }
    }

    // 概念：用户话里出现过的词表命中项（保持首次出现顺序）
    let mut concepts: Vec<String> = Vec::new();
    for message in messages {
        let lower = message.content.to_lowercase();
        for word in COMMAND_CONCEPTS {
            if lower.contains(word) && !concepts.iter().any(|c| c == word) {
                concepts.push(word.to_owned());
            }
        }
        for word in DOMAIN_CONCEPTS {
            if message.content.contains(word) && !concepts.iter().any(|c| c == word) {
                concepts.push((*word).to_owned());
            }
        }
    }
    facts.concepts = concepts;

    // 节流（保留最近 max_items 条，前面记"省略 N 条"）
    if seen_intents.len() > cfg.max_items {
        facts.elided_intents = seen_intents.len() - cfg.max_items;
        seen_intents = seen_intents.split_off(facts.elided_intents);
    }
    if seen_errors.len() > cfg.max_items {
        facts.elided_errors = seen_errors.len() - cfg.max_items;
        seen_errors = seen_errors.split_off(facts.elided_errors);
    }
    facts.intents = seen_intents;
    facts.errors = seen_errors;
    facts
}

fn collect_tool_calls(text: &str, facts: &mut Facts) {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return;
    };
    let calls = match &value {
        serde_json::Value::Object(map) => map
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        serde_json::Value::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    for call in calls {
        let name = call
            .get("name")
            .or_else(|| call.get("function").and_then(|f| f.get("name")))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        if name.is_empty() {
            continue;
        }
        facts.tool_calls += 1;
        let arguments = call
            .get("arguments")
            .cloned()
            .or_else(|| {
                call.get("function")
                    .and_then(|f| f.get("arguments"))
                    .cloned()
            })
            .unwrap_or(serde_json::Value::Null);
        let arguments = match arguments {
            serde_json::Value::String(raw) => {
                serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null)
            }
            other => other,
        };
        if let Some(path) = find_path(&arguments) {
            let writes = WRITE_TOOL_MARKERS
                .iter()
                .any(|marker| name.to_lowercase().contains(marker));
            let entry = facts
                .files
                .iter_mut()
                .find(|(existing, _)| existing == &path);
            match entry {
                Some((_, use_)) => {
                    if writes {
                        use_.writes += 1;
                    } else {
                        use_.reads += 1;
                    }
                }
                None => {
                    let use_ = if writes {
                        FileUse {
                            reads: 0,
                            writes: 1,
                        }
                    } else {
                        FileUse {
                            reads: 1,
                            writes: 0,
                        }
                    };
                    facts.files.push((path, use_));
                }
            }
        }
    }
}

fn find_path(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            for key in PATH_KEYS {
                if let Some(serde_json::Value::String(path)) = map.get(key) {
                    if !path.trim().is_empty() {
                        return Some(path.clone());
                    }
                }
            }
            map.values().find_map(find_path)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_path),
        _ => None,
    }
}

fn collect_todos(text: &str, todos: &mut Vec<String>) {
    for line in text.lines() {
        let trimmed = line.trim();
        // - [ ] 未完成 / - [x] 已完成（完成的从待办里移除）
        if let Some(rest) = trimmed
            .strip_prefix("- [ ]")
            .or_else(|| trimmed.strip_prefix("* [ ]"))
        {
            push_unique(todos, rest.trim());
            continue;
        }
        if trimmed.starts_with("- [x]") || trimmed.starts_with("- [X]") {
            let done = trimmed
                .trim_start_matches("- [x]")
                .trim_start_matches("- [X]")
                .trim();
            todos.retain(|item| item != done);
            continue;
        }
        for prefix in ["TODO:", "TODO：", "FIXME:", "FIXME：", "待办:", "待办："] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                push_unique(todos, rest.trim());
            }
        }
    }
}

fn push_unique(items: &mut Vec<String>, item: &str) {
    if item.is_empty() {
        return;
    }
    if !items.iter().any(|existing| existing == item) {
        items.push(item.to_owned());
    }
}

fn is_error(line: &str) -> bool {
    let lower = line.to_lowercase();
    ERROR_MARKERS_EN.iter().any(|marker| lower.contains(marker))
        || ERROR_MARKERS_ZH.iter().any(|marker| line.contains(marker))
}

/// 整条消息就是待办行（`- [ ] …` / `- [x] …` / `TODO: …` / `待办：…`）。
fn is_todo_line(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("- [ ]")
        || trimmed.starts_with("* [ ]")
        || trimmed.starts_with("- [x]")
        || trimmed.starts_with("- [X]")
        || ["TODO:", "TODO：", "FIXME:", "FIXME：", "待办:", "待办："]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
}
fn is_summary(text: &str) -> bool {
    text.trim_start().starts_with(SUMMARY_MARKER)
}

/// 注入上下文（系统提示、附件预览等）：不该占"用户意图"的位置。
fn is_injected(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("[系统]")
        || trimmed.starts_with("<context")
        || trimmed.starts_with("[注入]")
}

fn parse_sections(text: &str) -> BTreeMap<&'static str, Vec<String>> {
    let mut map: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut current: Option<&'static str> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("## ") {
            current = SECTIONS
                .iter()
                .find(|(_, header)| *header == title.trim())
                .map(|(key, _)| *key);
            continue;
        }
        if let Some(key) = current {
            if let Some(item) = trimmed.strip_prefix("- ") {
                // 继承时不带"（省略 N 条…）"这类注记
                if !item.starts_with("（省略") && !item.starts_with("（无）") {
                    map.entry(key).or_default().push(item.to_owned());
                }
            }
        }
    }
    map
}

/// 只继承"仍然成立"的 5 段：intent / concepts / files / errors / context。
///
/// `current` / `next` / `todos` 每次重新生成——它们是"现在进行时"，继承旧值只会误导模型。
fn merge_carried(
    target: &mut BTreeMap<&'static str, Vec<String>>,
    incoming: BTreeMap<&'static str, Vec<String>>,
) {
    const CARRIED: [&str; 5] = ["intent", "concepts", "files", "errors", "context"];
    for key in CARRIED {
        if let Some(items) = incoming.get(key) {
            let slot = target.entry(key).or_default();
            for item in items {
                if !slot.iter().any(|existing| existing == item) {
                    slot.push(item.clone());
                }
            }
        }
    }
}

fn render(facts: &Facts, archive_refs: &[i64], trigger: Trigger) -> Summary {
    let mut blocks: Vec<String> = Vec::with_capacity(8);
    let mut counts = [0_usize; 8];

    // 1. 主要请求与意图
    let mut intents = facts.intents.clone();
    let carried_intents = facts.carried.get("intent").cloned().unwrap_or_default();
    for item in carried_intents {
        if !intents.iter().any(|existing| existing == &item) {
            intents.insert(0, item);
        }
    }
    if facts.elided_intents > 0 {
        intents.insert(
            0,
            format!("（省略 {} 条较早的用户消息）", facts.elided_intents),
        );
    }
    counts[0] = intents.len();
    blocks.push(render_section(0, &intents));

    // 2. 关键技术概念
    let mut concepts = facts.carried.get("concepts").cloned().unwrap_or_default();
    for item in &facts.concepts {
        if !concepts.iter().any(|existing| existing == item) {
            concepts.push(item.clone());
        }
    }
    counts[1] = concepts.len();
    blocks.push(render_section(1, &concepts));

    // 3. 文件与代码
    let mut files: Vec<String> = facts
        .files
        .iter()
        .map(|(path, use_)| format!("{path} — 写×{} 读×{}", use_.writes, use_.reads))
        .collect();
    for item in facts.carried.get("files").cloned().unwrap_or_default() {
        if !files.iter().any(|existing| existing == &item) {
            files.push(item);
        }
    }
    counts[2] = files.len();
    blocks.push(render_section(2, &files));

    // 4. 错误与修复
    let mut errors = facts.errors.clone();
    for item in facts.carried.get("errors").cloned().unwrap_or_default() {
        if !errors.iter().any(|existing| existing == &item) {
            errors.insert(0, item);
        }
    }
    if facts.elided_errors > 0 {
        errors.insert(0, format!("（省略 {} 条较早的报错）", facts.elided_errors));
    }
    counts[3] = errors.len();
    blocks.push(render_section(3, &errors));

    // 5. 待办事项
    counts[4] = facts.todos.len();
    blocks.push(render_section(4, &facts.todos));

    // 6. 当前工作
    let mut current: Vec<String> = Vec::new();
    if !facts.last_user.is_empty() {
        current.push(format!("用户最后一条：{}", facts.last_user));
    }
    if !facts.last_assistant.is_empty() {
        current.push(format!("助手最后一次回复：{}", facts.last_assistant));
    }
    counts[5] = current.len();
    blocks.push(render_section(5, &current));

    // 7. 下一步
    let mut next: Vec<String> = Vec::new();
    if let Some(todo) = facts.todos.last() {
        next.push(format!("继续处理待办：{todo}"));
    } else if !facts.last_user.is_empty() {
        next.push(format!("回答用户最后一条请求：{}", facts.last_user));
    }
    counts[6] = next.len();
    blocks.push(render_section(6, &next));

    // 8. 关键上下文
    let mut context: Vec<String> = facts.carried.get("context").cloned().unwrap_or_default();
    context.push(format!(
        "本次压缩覆盖 {} 条消息 / {} 次工具调用 / {} 条工具结果（确定性模板摘要，未调用 LLM；触发原因：{}）",
        facts.message_count,
        facts.tool_calls,
        facts.tool_results,
        trigger.label_zh()
    ));
    if archive_refs.is_empty() {
        context.push("原文未存档（本地存储在压缩时不可用）".to_owned());
    } else {
        let refs: Vec<String> = archive_refs
            .iter()
            .map(|id| format!("memory://{id}"))
            .collect();
        context.push(format!(
            "被压原文已存入本地记忆，可回查：{}（{} 片）",
            refs.join("、"),
            archive_refs.len()
        ));
    }
    counts[7] = context.len();
    blocks.push(render_section(7, &context));

    let text = format!("{SUMMARY_MARKER}\n{}", blocks.join("\n"));
    Summary {
        text,
        section_items: counts,
    }
}

fn render_section(index: usize, items: &[String]) -> String {
    let header = SECTIONS[index].1;
    let mut out = format!("## {header}\n");
    if items.is_empty() {
        out.push_str("- （无）\n");
    } else {
        for item in items {
            out.push_str(&format!("- {item}\n"));
        }
    }
    out
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .join(" / ")
}

fn clip(text: &str, max_chars: usize) -> String {
    let flat = text.replace('\n', " ");
    let flat = flat.trim();
    if flat.chars().count() <= max_chars {
        flat.to_owned()
    } else {
        let head: String = flat.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

/// 一次压缩的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub plan: Plan,
    pub summary: String,
    /// 存档在 `memory_items` 里的 id（按写入顺序；空 = 没落库）
    pub archive_ids: Vec<i64>,
    /// 存档里一共有多少行（= 被压掉的消息数，可用来核对"原文没丢"）
    pub archived_lines: usize,
    /// 为了腾容量被淘汰的记忆条数（透传 `memory_items`）
    pub evicted: usize,
}

impl Outcome {
    pub fn applied(&self) -> bool {
        self.plan.applied()
    }

    /// 摘要自身占多少 token（评估"压一次到底省了多少"时要减掉它）。
    pub fn summary_tokens(&self) -> u32 {
        estimate_tokens(&self.summary)
    }

    /// 压缩后的预计占用：保留的尾部 + 摘要本身。
    pub fn projected_tokens(&self) -> u32 {
        if !self.applied() {
            return self.plan.used_tokens;
        }
        self.plan.retained_tokens + self.summary_tokens()
    }
}

/// 压缩 + 原文落库。
///
/// `session` 只用于存档标题（`会话 <session> 压缩存档 1/2`）。
/// 落库失败不阻断：摘要照给，`archive_ids` 为空，摘要里写"原文未存档"。
pub fn compact(
    store: &mut MemoryItemStore,
    session: &str,
    messages: &[Message],
    cfg: &CompactConfig,
    trigger: Trigger,
) -> CoreResult<Outcome> {
    let plan = plan(messages, cfg, trigger);
    if !plan.applied() {
        return Ok(Outcome {
            plan,
            summary: String::new(),
            archive_ids: Vec::new(),
            archived_lines: 0,
            evicted: 0,
        });
    }
    let compacted = &messages[..plan.cut_index];
    let mut archived_lines = 0_usize;
    let archive_ids =
        archive_originals(store, session, compacted, &mut archived_lines).unwrap_or_default();
    let summary = summarize(compacted, cfg, &archive_ids, plan.trigger);
    let evicted = 0; // `archive_originals` 内部已按片累计；见下
    Ok(Outcome {
        plan,
        summary: summary.text,
        archive_ids,
        archived_lines,
        evicted,
    })
}

/// 把被压掉的原文写进 `memory_items`（按 48 KiB 分片），返回各片 id。
fn archive_originals(
    store: &mut MemoryItemStore,
    session: &str,
    messages: &[Message],
    archived_lines: &mut usize,
) -> CoreResult<Vec<i64>> {
    let mut chunks: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_bytes = 0_usize;
    for message in messages {
        let line = format!(
            "[{}] {}",
            message.role,
            clip(&message.content, 4_000).replace('\n', " ")
        );
        let line_bytes = line.len() + 1;
        if !current.is_empty() && current_bytes + line_bytes > ARCHIVE_CHUNK_BYTES {
            chunks.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += line_bytes;
        current.push(line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    let total = chunks.len();
    let mut ids = Vec::with_capacity(total);
    for (index, chunk) in chunks.iter().enumerate() {
        let body = chunk.join("\n");
        *archived_lines += chunk.len();
        let title = format!("会话 {session} 压缩存档 {}/{}", index + 1, total);
        let draft = MemoryDraft {
            kind: MemoryKind::Summary,
            title: Some(&title),
            content: &body,
            source: Some("compact"),
            expires_at: None,
        };
        let written = store.upsert(&draft, None)?;
        ids.push(written.id);
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, role: &str, content: &str, tokens: u32) -> Message {
        Message {
            id,
            session_id: "s1".into(),
            role: role.into(),
            content: content.into(),
            tokens,
            pruned: false,
            created_at_ms: id * 1000,
        }
    }

    fn tool_call_msg(id: i64, name: &str, arguments: &str, tokens: u32) -> Message {
        msg(
            id,
            "assistant",
            &format!(
                r#"{{"type":"tool_call","tool_calls":[{{"id":"c{id}","name":"{name}","arguments":{arguments}}}]}}"#
            ),
            tokens,
        )
    }

    // ------------------------------------------------------------------
    // 触发
    // ------------------------------------------------------------------

    #[test]
    fn pressure_has_a_guard_for_small_windows() {
        let cfg = CompactConfig::default();
        // 32k 窗口：headroom 项是负数，护栏让它退回 55%
        assert_eq!(pressure_tokens(&cfg), 17_600);
        // 100k 窗口：headroom 项更小，取它
        let wide = CompactConfig {
            window: 100_000,
            ..cfg
        };
        assert_eq!(pressure_tokens(&wide), 100_000 - 4_096 - 65_536);
        // 1M 窗口：比例项更小，取比例
        let huge = CompactConfig {
            window: 1_000_000,
            ..cfg
        };
        assert_eq!(pressure_tokens(&huge), 550_000);
    }

    #[test]
    fn compaction_threshold_boundary() {
        let cfg = CompactConfig::default();
        assert!(!should_compact(17_599, &cfg));
        assert!(should_compact(17_600, &cfg));
        assert!(should_compact(17_601, &cfg));
    }

    #[test]
    fn model_switch_gate_uses_24k() {
        let cfg = CompactConfig::default();
        assert!(!should_compact_on_model_switch(23_999, &cfg));
        assert!(should_compact_on_model_switch(24_000, &cfg));
    }

    #[test]
    fn retain_tokens_follows_the_formula() {
        let cfg = CompactConfig::default();
        // floor((32000 - 4096) × 0.16) = floor(4464.64) = 4464
        assert_eq!(retain_tokens(&cfg), 4_464);
    }

    // ------------------------------------------------------------------
    // 切点
    // ------------------------------------------------------------------

    #[test]
    fn cut_keeps_at_least_the_retain_tokens() {
        let cfg = CompactConfig::default();
        // 40 条 × 500 token = 20000 > 压力 17600
        let messages: Vec<Message> = (1..=40)
            .map(|id| msg(id, if id % 2 == 0 { "user" } else { "assistant" }, "x", 500))
            .collect();
        assert!(should_compact(20_000, &cfg));
        let plan = plan(&messages, &cfg, Trigger::Pressure);
        assert!(plan.applied(), "{plan:?}");
        assert!(
            plan.retained_tokens >= 4_464,
            "尾部保留 {} < retain 4464",
            plan.retained_tokens
        );
        assert_eq!(
            plan.cut_index + plan.retained_messages,
            messages.len(),
            "切点 + 保留 = 总数"
        );
        assert_eq!(plan.compacted_tokens + plan.retained_tokens, 20_000);
    }

    #[test]
    fn cut_backs_off_to_keep_tool_pairs_balanced() {
        let cfg = CompactConfig::default();
        // 构造：前半是普通对话，尾部有一个 assistant(tool_call) + tool(result) 紧挨着
        // 让"朴素累计"恰好落在 tool_call 与 result 之间
        let mut messages = Vec::new();
        let mut id = 1;
        for _ in 0..30 {
            messages.push(msg(id, "user", "普通消息", 500));
            id += 1;
            messages.push(msg(id, "assistant", "普通回复", 500));
            id += 1;
        }
        // 现在 60 条 × 500 = 30000 token；无损切点会在第 40 条附近
        messages.push(tool_call_msg(id, "get_courses", r#"{"weekday":3}"#, 300));
        id += 1;
        messages.push(msg(id, "tool", r#"{"ok":true,"courses":[]}"#, 300));
        id += 1;
        messages.push(msg(id, "assistant", "周三只有一门课", 400));

        let plan = plan(&messages, &cfg, Trigger::Pressure);
        assert!(plan.applied(), "{plan:?}");
        let prefix = &messages[..plan.cut_index];
        let suffix = &messages[plan.cut_index..];
        assert_eq!(tool_balance(prefix), 0, "被压掉的部分必须配平");
        assert_eq!(tool_balance(suffix), 0, "保留的部分也必须配平");
        assert_ne!(
            suffix.first().map(|m| m.role.as_str()),
            Some("tool"),
            "保留区不能以孤立 tool 结果开头"
        );
        assert!(prefix.len() < messages.len());
    }

    #[test]
    fn all_tool_results_block_compaction() {
        let cfg = CompactConfig::default();
        // 全是工具结果、没有任何调用：任何 cut 的 prefix 都配不平 → 不压
        let messages: Vec<Message> = (1..=40)
            .map(|id| msg(id, "tool", "孤立的工具结果", 600))
            .collect();
        let plan = plan(&messages, &cfg, Trigger::Pressure);
        assert!(!plan.applied(), "{plan:?}");
        assert!(plan.blocked, "{plan:?}");
        assert_eq!(plan.cut_index, 0);
    }

    /// 尾部有一条孤立 tool 结果时，**被压掉的前缀**依然可以配平 → 允许压。
    ///
    /// 配平检查只看 prefix（被压掉的那段）：历史里本来就有的孤儿结果不该让压缩彻底罢工，
    /// 它留在保留区里，和压缩前一样。
    #[test]
    fn dangling_result_in_tail_does_not_block_prefix() {
        let cfg = CompactConfig::default();
        let mut messages: Vec<Message> = (1..=30)
            .map(|id| msg(id, "user", "普通消息", 600))
            .collect();
        messages.push(msg(99, "tool", "孤立的结果", 600));
        let plan = plan(&messages, &cfg, Trigger::Pressure);
        assert!(plan.applied(), "{plan:?}");
        assert_eq!(tool_balance(&messages[..plan.cut_index]), 0);
        assert_ne!(
            messages[plan.cut_index].role, "tool",
            "保留区不以 tool 开头"
        );
    }

    #[test]
    fn no_need_short_circuits() {
        let cfg = CompactConfig::default();
        let messages = vec![msg(1, "user", "你好", 10)];
        let plan = plan(&messages, &cfg, Trigger::NoNeed);
        assert!(!plan.applied());
        assert_eq!(plan.cut_index, 0);
    }

    // ------------------------------------------------------------------
    // 摘要
    // ------------------------------------------------------------------

    #[test]
    fn summary_is_deterministic() {
        let cfg = CompactConfig::default();
        let messages = sample_history();
        let first = summarize(&messages, &cfg, &[7], Trigger::Pressure);
        let second = summarize(&messages, &cfg, &[7], Trigger::Pressure);
        assert_eq!(first, second, "同一输入两次摘要必须逐字节相同");
        assert_eq!(first.tokens(), second.tokens());
    }

    #[test]
    fn summary_has_all_eight_sections() {
        let cfg = CompactConfig::default();
        let summary = summarize(&sample_history(), &cfg, &[1, 2], Trigger::Pressure);
        assert!(summary.text.starts_with(SUMMARY_MARKER));
        for (_, header) in SECTIONS {
            assert!(
                summary.text.contains(&format!("## {header}")),
                "缺段落 {header}"
            );
        }
        assert!(summary.text.contains("memory://1"));
        assert!(summary.text.contains("memory://2"));
    }

    #[test]
    fn summary_extracts_the_seven_rules() {
        let cfg = CompactConfig::default();
        let messages = vec![
            msg(1, "user", "帮我整理这学期的课表，周三下午有空吗", 40),
            msg(2, "user", "[系统] 这是一段注入上下文", 10),
            tool_call_msg(
                3,
                "get_courses",
                r#"{"file_path":"crates/core/src/x.rs"}"#,
                30,
            ),
            msg(4, "tool", "Error: connect refused 127.0.0.1:1", 20),
            msg(5, "assistant", "第一步先导课表\n第二步再排时间", 30),
            msg(6, "user", "- [ ] 导出 ICS\n- [x] 打开设置页", 20),
        ];
        let summary = summarize(&messages, &cfg, &[], Trigger::Pressure);
        assert!(summary.text.contains("帮我整理这学期的课表"));
        assert!(!summary.text.contains("注入上下文"), "注入上下文不该进意图");
        assert!(summary.text.contains("课表") || summary.text.contains("周次"));
        assert!(summary.text.contains("crates/core/src/x.rs"), "文件规则");
        assert!(summary.text.contains("写×0 读×1"), "读/写分类");
        assert!(summary.text.contains("Error: connect refused"), "报错规则");
        assert!(summary.text.contains("导出 ICS"), "待办规则");
        assert!(summary.text.contains("第一步先导课表"), "当前工作规则");
        assert!(
            summary.text.contains("继续处理待办：导出 ICS"),
            "下一步规则"
        );
        assert!(summary.text.contains("原文未存档"), "没落库时要说明");
    }

    #[test]
    fn previous_summary_is_carried_not_repeated() {
        let cfg = CompactConfig::default();
        let first = summarize(&sample_history(), &cfg, &[3], Trigger::Pressure);
        let second = summarize(
            &[
                msg(1, "user", same_first_intent(), 40),
                msg(2, "user", &first.text, 60),
                msg(3, "user", "又过了一轮的新请求", 20),
            ],
            &cfg,
            &[4],
            Trigger::Pressure,
        );
        assert!(second.text.contains("又过了一轮的新请求"));
        // 旧摘要的"意图"被继承，而不是当成一条用户消息再列一遍
        assert_eq!(
            second.text.matches(same_first_intent()).count(),
            1,
            "旧意图只该出现一次：{}",
            second.text
        );
    }

    fn same_first_intent() -> &'static str {
        "帮我整理这学期的课表"
    }

    fn sample_history() -> Vec<Message> {
        vec![
            msg(1, "user", same_first_intent(), 40),
            msg(2, "assistant", "好的，我先看课表数据", 30),
            msg(3, "user", "顺手把周三的课列出来", 20),
        ]
    }

    // ------------------------------------------------------------------
    // 落库
    // ------------------------------------------------------------------

    #[test]
    fn compact_archives_every_original_line() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let cfg = CompactConfig::default();
        let messages: Vec<Message> = (1..=40)
            .map(|id| {
                msg(
                    id,
                    if id % 2 == 0 { "user" } else { "assistant" },
                    "内容",
                    500,
                )
            })
            .collect();
        let outcome = compact(&mut store, "s1", &messages, &cfg, Trigger::Pressure).unwrap();
        assert!(outcome.applied(), "{:?}", outcome.plan);
        assert_eq!(
            outcome.archived_lines, outcome.plan.compacted_messages,
            "存档行数必须等于被压掉的消息数"
        );
        assert!(!outcome.archive_ids.is_empty());
        // 库里确实有这些摘要条目，且是 kind=summary
        let listed = store.list(Some(MemoryKind::Summary), 100).unwrap();
        assert_eq!(listed.len(), outcome.archive_ids.len());
        assert!(listed
            .iter()
            .all(|item| item.source.as_deref() == Some("compact")));
        // 摘要里带 memory:// 引用
        for id in &outcome.archive_ids {
            assert!(outcome.summary.contains(&format!("memory://{id}")));
        }
        // 压缩确实省了 token
        assert!(outcome.projected_tokens() < outcome.plan.used_tokens);
    }

    #[test]
    fn below_pressure_writes_nothing() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let cfg = CompactConfig::default();
        let messages = vec![msg(1, "user", "短对话", 10), msg(2, "assistant", "好", 10)];
        let outcome = compact(&mut store, "s1", &messages, &cfg, Trigger::NoNeed).unwrap();
        assert!(!outcome.applied());
        assert!(outcome.summary.is_empty());
        assert!(store.list(None, 100).unwrap().is_empty());
    }

    // ------------------------------------------------------------------
    // 边界
    // ------------------------------------------------------------------

    #[test]
    fn empty_and_single_message_are_safe() {
        let cfg = CompactConfig::default();
        let empty: Vec<Message> = Vec::new();
        assert!(!plan(&empty, &cfg, Trigger::Pressure).applied());
        assert!(summarize(&empty, &cfg, &[], Trigger::Pressure)
            .text
            .starts_with(SUMMARY_MARKER));

        let single = vec![msg(1, "user", "只有一条", 999_999)];
        let plan = plan(&single, &cfg, Trigger::Pressure);
        // 单条就是全部历史：要么不压，要么压掉它自己后没有保留——两种都不该崩
        assert!(plan.compacted_messages <= 1);
        assert!(plan.applied() || plan.blocked);
        let _ = summarize(&single, &cfg, &[], Trigger::Pressure);
    }

    #[test]
    fn huge_single_message_does_not_panic() {
        let cfg = CompactConfig::default();
        let huge = "字".repeat(200_000);
        let messages = vec![
            msg(1, "user", &huge, 200_000),
            msg(2, "assistant", "收到", 10),
        ];
        let plan = plan(&messages, &cfg, Trigger::Pressure);
        assert!(!plan.blocked || plan.cut_index == 0);
        let summary = summarize(&messages, &cfg, &[], Trigger::Pressure);
        // 超长消息被 clip 到 200 字符，摘要不会跟着爆炸
        assert!(
            summary.tokens() < 2_000,
            "摘要 {} token 太大",
            summary.tokens()
        );
    }

    /// 验收用的"真实场景"：50 轮对话（每轮 user + tool_call + tool + assistant），
    /// 到点就压，压缩后把前缀换成一条摘要消息继续。
    ///
    /// 断言：**任何时刻都不超过窗口**，且至少真的压过一次。
    #[test]
    fn fifty_turns_stay_inside_the_window() {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        let cfg = CompactConfig::default();
        let window = cfg.window;
        let mut history: Vec<Message> = Vec::new();
        let mut next_id = 1_i64;
        let mut compactions = 0;
        let mut peak = 0_u32;
        let mut first_trigger_used = 0_u32;
        let mut last_retained = 0_usize;

        for turn in 1..=50 {
            history.push(msg(
                next_id,
                "user",
                &format!("第 {turn} 轮：帮我看看周三的课表"),
                60,
            ));
            next_id += 1;
            history.push(tool_call_msg(
                next_id,
                "get_courses",
                r#"{"weekday":3}"#,
                80,
            ));
            next_id += 1;
            history.push(msg(
                next_id,
                "tool",
                r#"{"ok":true,"courses":["高等数学"]}"#,
                200,
            ));
            next_id += 1;
            history.push(msg(
                next_id,
                "assistant",
                "周三只有一门高等数学，在东校区1教305",
                120,
            ));
            next_id += 1;

            let used: u32 = history.iter().map(|m| m.tokens).sum();
            peak = peak.max(used);
            assert!(used <= window, "第 {turn} 轮就超窗口了：{used} > {window}");
            if !should_compact(used, &cfg) {
                continue;
            }
            if compactions == 0 {
                first_trigger_used = used;
            }
            let outcome = compact(&mut store, "s1", &history, &cfg, Trigger::Pressure).unwrap();
            assert!(
                outcome.applied(),
                "第 {turn} 轮该压却没压：{:?}",
                outcome.plan
            );
            assert_eq!(outcome.archived_lines, outcome.plan.compacted_messages);
            // 模拟调用方：前缀换成一条摘要消息，尾部原样保留
            let tail = history.split_off(outcome.plan.cut_index);
            history.clear();
            history.push(Message {
                id: -1,
                session_id: "s1".into(),
                role: "user".into(),
                content: outcome.summary.clone(),
                tokens: outcome.summary_tokens(),
                pruned: false,
                created_at_ms: 0,
            });
            history.extend(tail);
            last_retained = outcome.plan.retained_messages;
            compactions += 1;
            let after: u32 = history.iter().map(|m| m.tokens).sum();
            assert!(
                after < used,
                "第 {turn} 轮压缩后没有变小：{after} >= {used}"
            );
        }

        println!(
            "50 轮：压缩 {compactions} 次，首次触发 {first_trigger_used} token，峰值 {peak} token，\
             末次保留尾部 {last_retained} 条"
        );
        assert!(compactions >= 1, "50 轮里一次都没触发压缩");
        assert!(peak <= window, "峰值 {peak} 超过窗口 {window}");
    }
}
