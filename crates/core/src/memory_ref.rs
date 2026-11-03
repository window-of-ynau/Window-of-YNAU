//! `memory://` 引用找回（W8 阶段 2）。
//!
//! 阶段 1 的摘要里嵌了 `memory://<id>`，但**只能看不能取**——压缩后就再也拿不回原文。
//! 这个模块补上"取回"和"在摘要里检索"：
//!
//! - [`parse_ref`] / [`parse_refs`]：从任意文本（摘要、消息）里解析引用；
//! - [`expand`]：按 id 取回原文（含 kind/title/来源，便于界面展示）；
//! - [`expand_text`]：一次把一段文本里的**所有**引用取回（超长消息切多片时会有多个）；
//! - [`search_refs`]：在已落库的摘要/原文里按关键词检索，返回 `{ref_id, snippet, original}`。
//!
//! 引用本身只是个整数：它指向 `memory_items` 里的一行，**不保证那行还在**
//! （容量淘汰/用户手动删都会让它消失）。所以 [`expand`] 返回 `Option`，
//! 调用方要能处理"引用失效"（界面上显示"原文已被清理"比崩掉好）。

use serde::{Deserialize, Serialize};

use crate::error::CoreResult;
use crate::memory_items::{MemoryItem, MemoryItemStore, MemoryKind};

/// 引用前缀。
pub const REF_PREFIX: &str = "memory://";

/// 检索结果里最多内联多少字的原文（再长就让调用方按 id 再取一次）。
const INLINE_ORIGINAL_CHARS: usize = 2_000;

/// 取回的一条原文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedRef {
    /// 引用 id（就是 `memory://<id>` 里的 id）
    pub ref_id: i64,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 原文
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl ExpandedRef {
    fn from_item(item: MemoryItem) -> Self {
        Self {
            ref_id: item.id,
            kind: item.kind,
            title: item.title,
            content: item.content,
            source: item.source,
        }
    }
}

/// 检索命中的一条。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefHit {
    pub ref_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 命中处的上下文片段（≤ 160 字）
    pub snippet: String,
    /// 原文（超过 2000 字时为空，用 `ref_id` 再 [`expand`]）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    /// 原文是否被省略（true = 太长，请用 ref_id 取）
    pub truncated: bool,
}

/// `"memory://12"` → `12`；允许前后有标点/空白。
pub fn parse_ref(text: &str) -> Option<i64> {
    let start = text.find(REF_PREFIX)? + REF_PREFIX.len();
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// 解析一段文本里**所有** `memory://<id>`（按出现顺序，去重）。
///
/// 超长消息被切成多片时会写成 `memory://3 memory://4`，这里会全部取到。
pub fn parse_refs(text: &str) -> Vec<i64> {
    let mut ids: Vec<i64> = Vec::new();
    let mut rest = text;
    while let Some(position) = rest.find(REF_PREFIX) {
        let after = &rest[position + REF_PREFIX.len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            if let Ok(id) = digits.parse::<i64>() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        rest = after;
    }
    ids
}

/// 按 id 取回原文；引用失效（被淘汰/删除）时返回 `None`。
pub fn expand(store: &MemoryItemStore, ref_id: i64) -> CoreResult<Option<ExpandedRef>> {
    Ok(store.get(ref_id)?.map(ExpandedRef::from_item))
}

/// 把一段文本里的所有引用都取回（顺序同 [`parse_refs`]；失效的直接跳过）。
pub fn expand_text(store: &MemoryItemStore, text: &str) -> CoreResult<Vec<ExpandedRef>> {
    let mut out = Vec::new();
    for id in parse_refs(text) {
        if let Some(item) = expand(store, id)? {
            out.push(item);
        }
    }
    Ok(out)
}

/// 在已落库的记忆（摘要 + 超长原文）里按关键词检索。
///
/// 检索范围：`memory_items` 的 `summary` 类（压缩摘要与被切片的超长原文都记在这里）。
/// 匹配规则：**大小写不敏感的子串**（不做分词——RAG 的语义检索是另一条路，
/// 这里要的是"压完还能按关键字找回来"，确定、够用即可）。
pub fn search_refs(store: &MemoryItemStore, query: &str, limit: usize) -> CoreResult<Vec<RefHit>> {
    let query = query.trim();
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let needle = query.to_lowercase();
    let pool = store.list(Some(MemoryKind::Summary), limit.saturating_mul(50).max(200))?;
    let mut hits = Vec::new();
    for item in pool {
        if hits.len() >= limit {
            break;
        }
        let haystack = item.content.to_lowercase();
        let title_hit = item
            .title
            .as_deref()
            .map(|title| title.to_lowercase().contains(&needle))
            .unwrap_or(false);
        if !haystack.contains(&needle) && !title_hit {
            continue;
        }
        let chars = item.content.chars().count();
        let (original, truncated) = if chars <= INLINE_ORIGINAL_CHARS {
            (Some(item.content.clone()), false)
        } else {
            (None, true)
        };
        hits.push(RefHit {
            ref_id: item.id,
            title: item.title.clone(),
            snippet: snippet_around(&item.content, query, 160),
            original,
            truncated,
        });
    }
    Ok(hits)
}

/// 取命中位置前后的片段（找不到就取开头）。
fn snippet_around(content: &str, needle: &str, limit: usize) -> String {
    let lower = content.to_lowercase();
    let position = lower.find(&needle.to_lowercase()).unwrap_or(0);
    // 字节位置 → 字符位置（CJK 下两者不同，必须换）
    let char_start = content[..position].chars().count();
    let chars: Vec<char> = content.chars().collect();
    let start = char_start.saturating_sub(40).min(chars.len());
    let end = (start + limit).min(chars.len());
    let mut text: String = chars[start..end].iter().collect();
    text = text.replace('\n', " ");
    if end < chars.len() {
        text.push('…');
    }
    if start > 0 {
        text.insert(0, '…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_items::{MemoryDraft, MemoryKind};

    fn store_with(items: &[(&str, &str)]) -> MemoryItemStore {
        let mut store = MemoryItemStore::open_in_memory().unwrap();
        for (title, content) in items {
            store
                .upsert(
                    &MemoryDraft {
                        kind: MemoryKind::Summary,
                        title: Some(title),
                        content,
                        source: Some("test"),
                        expires_at: None,
                    },
                    None,
                )
                .unwrap();
        }
        store
    }

    #[test]
    fn parse_ref_handles_surrounding_noise() {
        assert_eq!(parse_ref("见 memory://42 处"), Some(42));
        assert_eq!(parse_ref("memory://7"), Some(7));
        assert_eq!(parse_ref("没有引用"), None);
        assert_eq!(parse_ref("memory://"), None);
        assert_eq!(parse_ref("memory://abc"), None);
    }

    #[test]
    fn parse_refs_collects_all_and_dedupes() {
        let text = "memory://3 和 memory://4，再提一次 memory://3";
        assert_eq!(parse_refs(text), vec![3, 4]);
        assert!(parse_refs("无引用").is_empty());
    }

    #[test]
    fn expand_returns_exact_original() {
        let store = store_with(&[("标题", "原文内容：雨伞是黑色的")]);
        let got = expand(&store, 1).unwrap().unwrap();
        assert_eq!(got.content, "原文内容：雨伞是黑色的");
        assert_eq!(got.title.as_deref(), Some("标题"));
        assert_eq!(got.kind, "summary");
        // 不存在的引用：None，不报错
        assert!(expand(&store, 999).unwrap().is_none());
    }

    #[test]
    fn expand_text_follows_every_ref() {
        let store = store_with(&[("第一片", "AAA"), ("第二片", "BBB")]);
        let got = expand_text(&store, "memory://1 memory://2").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].content, "AAA");
        assert_eq!(got[1].content, "BBB");
    }

    #[test]
    fn search_refs_finds_by_content_and_reports_snippet() {
        let store = store_with(&[("课表", "周三记得带雨具和雨伞"), ("杂事", "买牛奶")]);
        let hits = search_refs(&store, "雨伞", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ref_id, 1);
        assert!(hits[0].snippet.contains("雨伞"));
        assert_eq!(hits[0].original.as_deref(), Some("周三记得带雨具和雨伞"));
        assert!(!hits[0].truncated);
        // 大小写不敏感
        assert_eq!(search_refs(&store, "牛奶", 10).unwrap().len(), 1);
        // 空查询 / 空结果
        assert!(search_refs(&store, "  ", 10).unwrap().is_empty());
        assert!(search_refs(&store, "不存在", 10).unwrap().is_empty());
    }

    #[test]
    fn search_refs_limits_and_marks_long_originals() {
        let long = "雨".repeat(INLINE_ORIGINAL_CHARS + 10);
        let store = store_with(&[("长文", &long), ("短文", "也有雨")]);
        let hits = search_refs(&store, "雨", 1).unwrap();
        assert_eq!(hits.len(), 1, "limit 生效");
        let hits = search_refs(&store, "雨", 10).unwrap();
        let long_hit = hits
            .iter()
            .find(|hit| hit.title.as_deref() == Some("长文"))
            .unwrap();
        assert!(long_hit.truncated, "超过 2000 字不内联原文");
        assert!(long_hit.original.is_none());
        assert!(long_hit.snippet.contains('雨'));
    }
}
