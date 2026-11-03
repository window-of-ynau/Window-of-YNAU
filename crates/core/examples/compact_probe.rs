//! 压缩探针：跑一遍 W8 阶段 1 / 阶段 2 的压缩，打印压缩率与 `memory://` 往返样本。
//!
//! ```text
//! cargo run -p yunong-core --example compact_probe
//! ```
//!
//! 平台无关（不碰任何 Windows API），Linux CI 上也能编。

use yunong_core::compact::{
    compact, density_for, pressure_tokens, summarize_with_density, CompactConfig, Density, Trigger,
};
use yunong_core::memory::Message;
use yunong_core::memory_items::MemoryItemStore;
use yunong_core::memory_ref::{expand_text, parse_refs, search_refs};

fn msg(id: i64, role: &str, content: &str, tokens: u32) -> Message {
    Message {
        id,
        session_id: "probe".into(),
        role: role.into(),
        content: content.into(),
        tokens,
        pruned: false,
        created_at_ms: id * 1_000,
    }
}

/// 造一段"很像真实开发对话"的历史：有意图、有错误、有重复粘贴、有一条超长粘贴。
fn synth() -> Vec<Message> {
    let mut messages = Vec::new();
    let mut id = 1;
    for round in 0..60 {
        messages.push(msg(
            id,
            "user",
            &format!("第 {round} 轮：帮我看下课表里周三有没有课，顺便查一下天气。"),
            120,
        ));
        id += 1;
        messages.push(msg(
            id,
            "assistant",
            &format!("第 {round} 轮回复：周三第 3-4 节有《数据库原理》，天气多云转晴。"),
            160,
        ));
        id += 1;
        if round % 7 == 3 {
            messages.push(msg(id, "tool", "Error: 429 Too Many Requests", 80));
            id += 1;
        }
    }
    // 重复粘贴 8 次（dedup 要吃掉）
    for _ in 0..8 {
        messages.push(msg(id, "user", "记住：宿舍楼 3 栋 502", 60));
        id += 1;
    }
    // 一条超长粘贴（会被落库换引用）
    messages.push(msg(id, "user", &"很长的日志行\n".repeat(6_000), 60_000));
    messages
}

fn main() {
    let cfg = CompactConfig {
        window: 32_000,
        ..CompactConfig::default()
    };
    let messages = synth();
    let used: u32 = messages.iter().map(|m| m.tokens).sum();
    println!("=== 输入 ===");
    println!(
        "消息 {} 条，合计 {used} token；压力阈值 {} token；密度档 {}",
        messages.len(),
        pressure_tokens(&cfg),
        density_for(used, &cfg).as_str()
    );

    // 阶段 1 对照：不做 shrink、固定 Full 密度、无降级
    let mut store1 = MemoryItemStore::open_in_memory().unwrap();
    let compacted = &messages[..messages.len() - 10]; // 粗略模拟 plan 的切点
    let compacted_tokens: u32 = compacted.iter().map(|m| m.tokens).sum();
    let stage1 = summarize_with_density(compacted, &cfg, &[], Trigger::Pressure, Density::Full);
    println!("\n=== 阶段 1（Full 密度，不降级）===");
    println!(
        "摘要 {} token（被压部分 {} token → 摘要压缩率 {:.1}%）",
        stage1.tokens(),
        compacted_tokens,
        (1.0 - stage1.tokens() as f32 / compacted_tokens as f32) * 100.0
    );

    // 阶段 2：shrink 超长消息 + 按压力选密度 + 摘要超预算自动降级
    let mut store2 = MemoryItemStore::open_in_memory().unwrap();
    let outcome = compact(&mut store2, "probe", &messages, &cfg, Trigger::Pressure).unwrap();
    println!("\n=== 阶段 2 ===");
    println!(
        "density={}, degradation={} 级, shrunk={} 条超长消息, 存档 {} 片, 摘要 {} token（摘要压缩率 {:.1}%）",
        outcome.density.as_str(),
        outcome.degradation,
        outcome.shrunk,
        outcome.archive_ids.len(),
        outcome.summary_tokens(),
        (1.0 - outcome.summary_tokens() as f32 / compacted_tokens as f32) * 100.0
    );
    println!(
        "预计压缩后占用：{} token（保留尾部 {} + 摘要 {}）",
        outcome.projected_tokens(),
        outcome.plan.retained_tokens,
        outcome.summary_tokens()
    );

    // memory:// 往返
    let refs = parse_refs(&outcome.summary);
    println!("\n=== memory:// 往返 ===");
    println!(
        "摘要里解析出 {} 个引用：{:?}",
        refs.len(),
        &refs[..refs.len().min(5)]
    );
    if let Some(first) = refs.first() {
        let expanded = expand_text(&store2, &format!("memory://{first}")).unwrap();
        if let Some(item) = expanded.first() {
            let head: String = item.content.chars().take(60).collect();
            println!(
                "{{\"ref_id\":{},\"kind\":\"{}\",\"title\":{},\"content_head\":\"{}\",\"content_chars\":{}}}",
                item.ref_id,
                item.kind,
                match &item.title {
                    Some(title) => format!("\"{title}\""),
                    None => "null".to_owned(),
                },
                head.replace('\n', " "),
                item.content.chars().count()
            );
        }
    }

    // 检索找回
    let all = yunong_core::memory_items::MemoryItemStore::list(
        &store2,
        Some(yunong_core::memory_items::MemoryKind::Summary),
        10,
    )
    .unwrap();
    println!("\n=== 落库内容 ===");
    for item in &all {
        println!(
            "#{} kind={} chars={} 含「第 5 轮」={} 含「课表」={}",
            item.id,
            item.kind,
            item.content.chars().count(),
            item.content.contains("第 5 轮"),
            item.content.contains("课表")
        );
    }
    let hits = search_refs(&store2, "课表", 3).unwrap();
    println!("\n=== 关键词检索（课表）===");
    println!("命中 {} 条", hits.len());
    for hit in &hits {
        println!(
            "{{\"ref_id\":{},\"snippet\":\"{}\",\"truncated\":{}}}",
            hit.ref_id,
            hit.snippet.replace('"', "'"),
            hit.truncated
        );
    }

    // 可复现性
    let mut store3 = MemoryItemStore::open_in_memory().unwrap();
    let again = compact(&mut store3, "probe", &messages, &cfg, Trigger::Pressure).unwrap();
    println!(
        "\n=== 可复现性 ===\n两次压缩摘要逐字节相同：{}",
        outcome.summary == again.summary
    );
    // 防止 store1 未使用告警（阶段 1 对照用不到落库）
    let _ = &mut store1;
}
