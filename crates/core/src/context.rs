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
/// # 这是**刻意的保守估计**，不是"接近真实分词器"
///
/// 这个数字只用于**准入预算与压缩触发**（`should_compress` / `plan_compaction`），
/// 不用于计费（计费以厂商返回的 `usage` 为准）。两种误差的代价不对称：
///
/// - **高估** → 提前压缩，丢一点上下文（用户能感知但可接受）；
/// - **低估** → 请求溢出（直接失败）或超预算（花钱）。
///
/// 所以我们选高估。参照 DSH-DCP 的 CJK 计价（约 **2 字符/token**，见
/// `.internal/research/dsh-dcp-evaluation.md` §2.3 与 `dsh-dcp/lib/summarizer.js:98-102`），
/// 同一段文本上我们的估值约为它的 **1.0–1.9 倍**：
///
/// | 样本 | 我们 | DCP | 倍数 |
/// |---|---|---|---|
/// | 纯中文散文 1047 字 | 714 | 431 | 1.66× |
/// | 单条中文对话 77 字 | 73 | 39 | 1.87× |
/// | 中文注释 + 代码 4747 字 | 1450 | 1289 | 1.12× |
/// | 中英混排 310 字 | 121 | 93 | 1.30× |
/// | 纯 ASCII | 21 | 21 | 1.00× |
///
/// 纯汉字理论上限是 **2.0×**（我们 1 字/token，DCP 2 字符/token）；含中文标点时略低，
/// 因为 `is_cjk` 没把全角标点算进 CJK（按 4 字符/token 反而更保守）。
///
/// **若将来接入真实 tokenizer（按模型实测），用它的计数替换本函数再校准一次**；
/// 在那之前，别为了"看起来更准"把 1 字/token 调成 0.5——那会把上面的误差方向反过来。
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

    /// DSH-DCP 的 CJK 计价（`dsh-dcp/lib/summarizer.js:98-102`）：CJK 2 字符/token，
    /// 其余 4 字符/token。这里只用来**对照方向**，不是要改成它。
    fn dcp_estimate(text: &str) -> u32 {
        let (mut cjk, mut other) = (0_f32, 0_f32);
        for ch in text.chars() {
            // DCP 把假名/谚文/全角标点也算 CJK；中文正文里主要差在标点上
            let code = ch as u32;
            let wide = matches!(code, 0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF
                | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0xAC00..=0xD7AF);
            if wide {
                cjk += 1.0;
            } else {
                other += 1.0;
            }
        }
        (cjk / 2.0 + other / 4.0).ceil() as u32
    }

    /// 我们**刻意高估**：中文上永远不小于 DCP 的估值（纯汉字接近 2 倍）。
    ///
    /// 这条用例锁的是**方向**，不是具体倍数：将来若有人把 1 字/token 改成 0.5 去"贴近
    /// 真实分词器"，这个断言会立刻红，提醒他先读完 `estimate_tokens` 的文档。
    #[test]
    fn cjk_estimate_is_deliberately_conservative_versus_dcp() {
        let cases = [
            // 纯汉字：1 字/token vs 0.5 字/token → 约 2 倍
            "帮我把这学期的课表整理一下周一到周五每天有哪几门课哪几门是单周上的",
            // 含中文标点
            "下周三下午想去图书馆，帮我看看有没有课；如果有，能不能换到周五。",
            // 中文 + 代码标识符（混排）
            "这段 plan_compaction 为什么先剪大的？是不是按时间顺序更合理？",
            // 纯 ASCII：两者必须一致（都不高估英文）
            "fn main() { let plan = plan_compaction(&messages, 32000); }",
        ];
        for text in cases {
            let ours = estimate_tokens(text);
            let theirs = dcp_estimate(text);
            assert!(
                ours >= theirs,
                "我们的估值必须 ≥ DCP（高估是刻意的）：{text:?} ours={ours} dcp={theirs}"
            );
            // 也不该高估到荒谬（超过 2.1 倍说明公式写错了）
            assert!(
                ours as f32 <= theirs as f32 * 2.1 + 1.0,
                "{text:?} ours={ours} dcp={theirs} 倍数过大"
            );
        }
        // 纯 ASCII 上两者相等：保守只针对 CJK
        let ascii = "fn main() { let plan = plan_compaction(&messages, 32000); }";
        assert_eq!(estimate_tokens(ascii), dcp_estimate(ascii));
    }
}
