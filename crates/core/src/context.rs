//! 上下文预算：估算 token、判断何时压缩、生成压缩计划（ADR-05 / FR-E2）。
//!
//! 关键约束：**压缩只在一轮结束时做**，不能在一轮中途重排上下文，
//! 否则厂商的 prompt cache 会失效，成本直接翻倍。

use crate::memory::Message;
use yunong_shared::COMPRESS_TRIGGER_RATIO;

/// 估算一段文本的 token 数。
///
/// 保守策略（宁可高估，避免超限）：
/// - CJK 字符：1 字 ≈ 1 token
/// - 其他字符（拉丁/数字/符号）：4 字 ≈ 1 token
///
/// 注意：这是**估算**，用于预算与"先拒后转"；真实计费以厂商返回的 usage 为准。
pub fn estimate_tokens(text: &str) -> u32 {
    let mut cjk = 0_u32;
    let mut other = 0_u32;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    let est = cjk as f32 + (other as f32 / 4.0);
    est.ceil() as u32
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

/// 是否达到压缩阈值（默认 55%）。
pub fn should_compress(used_tokens: u32, budget_tokens: u32) -> bool {
    if budget_tokens == 0 {
        return true;
    }
    (used_tokens as f32 / budget_tokens as f32) >= COMPRESS_TRIGGER_RATIO
}

/// 剩余可用 token（饱和到 0）。
pub fn remaining_tokens(used_tokens: u32, budget_tokens: u32) -> u32 {
    budget_tokens.saturating_sub(used_tokens)
}

/// 压缩计划：告诉调用方"剪哪几条、保留几条最近、预计省多少"。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactionPlan {
    /// 建议剪枝的消息 id（按"最该剪"排序）
    pub prune_ids: Vec<i64>,
    /// 最近这么多条必须保留（含当前轮）
    pub keep_recent: usize,
    /// 预计释放的 token
    pub freed_tokens: u32,
    /// 剪枝后预计占用
    pub projected_tokens: u32,
}

impl CompactionPlan {
    pub fn is_empty(&self) -> bool {
        self.prune_ids.is_empty()
    }
}

/// 至少保留的最近消息条数（一轮对话 + 一点上下文）。
pub const MIN_KEEP_RECENT: usize = 6;

/// 生成压缩计划。
///
/// 策略（与 ADR-05 和产品需求的"剪枝优先级"一致）：
/// 1. 最近 [`MIN_KEEP_RECENT`] 条一律不动；
/// 2. 在剩下的消息里，优先剪**token 最多**的（通常就是工具返回/长文档）；
/// 3. 一直剪到占用回落到预算的 40% 以内，或没有可剪的了。
pub fn plan_compaction(messages: &[Message], budget_tokens: u32) -> CompactionPlan {
    let used: u32 = messages.iter().map(|m| m.tokens).sum();
    if used == 0 || messages.len() <= MIN_KEEP_RECENT {
        return CompactionPlan {
            prune_ids: vec![],
            keep_recent: messages.len(),
            freed_tokens: 0,
            projected_tokens: used,
        };
    }

    let target: u32 = (budget_tokens as f32 * 0.40) as u32;
    let split = messages.len() - MIN_KEEP_RECENT;
    let mut candidates: Vec<&Message> = messages[..split]
        .iter()
        .filter(|m| !m.pruned && m.tokens > 0)
        .collect();
    // token 多的先剪；同量时按时间早的先剪
    candidates.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.id.cmp(&b.id)));

    let mut prune_ids = Vec::new();
    let mut freed = 0_u32;
    for m in candidates {
        if used.saturating_sub(freed) <= target {
            break;
        }
        prune_ids.push(m.id);
        freed += m.tokens;
    }

    CompactionPlan {
        prune_ids,
        keep_recent: MIN_KEEP_RECENT,
        freed_tokens: freed,
        projected_tokens: used.saturating_sub(freed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, tokens: u32) -> Message {
        Message {
            id,
            session_id: "s1".into(),
            role: "tool".into(),
            content: "x".into(),
            tokens,
            pruned: false,
            created_at_ms: id,
        }
    }

    #[test]
    fn cjk_estimates_one_token_per_char() {
        assert_eq!(estimate_tokens("你好"), 2);
        assert_eq!(estimate_tokens(""), 0);
        // 4 个 ASCII 字符 ≈ 1 token
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2); // ceil(5/4)
    }

    #[test]
    fn compress_triggers_at_55_percent() {
        assert!(!should_compress(54, 100));
        assert!(should_compress(55, 100));
        assert!(should_compress(100, 100));
        assert!(should_compress(1, 0));
    }

    #[test]
    fn keeps_recent_and_prunes_biggest_first() {
        // 10 条：其中 id=2 占了 600 token
        let mut msgs = vec![msg(1, 50), msg(2, 600), msg(3, 50), msg(4, 50)];
        for id in 5..=10 {
            msgs.push(msg(id, 50));
        }
        let plan = plan_compaction(&msgs, 1000);
        assert_eq!(plan.keep_recent, MIN_KEEP_RECENT);
        // 最近 6 条（id 5..10）绝不能出现在剪枝列表里
        assert!(
            plan.prune_ids.iter().all(|id| *id < 5),
            "误剪最近消息: {:?}",
            plan.prune_ids
        );
        // 最大的那条应当被优先剪掉
        assert_eq!(plan.prune_ids.first(), Some(&2));
        assert!(plan.freed_tokens >= 600);
        assert!(plan.projected_tokens < 950);
    }

    #[test]
    fn short_history_is_untouched() {
        let msgs = vec![msg(1, 999), msg(2, 999)];
        let plan = plan_compaction(&msgs, 1000);
        assert!(plan.is_empty());
        assert_eq!(plan.projected_tokens, 1998);
    }
}
