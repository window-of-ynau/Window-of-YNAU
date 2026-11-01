//! 真实上游冒烟测试（**永远 `#[ignore]`，不进 CI**）。
//!
//! CI 里没有密钥也没有外网，所以真实厂商的 TLS + SSE 只能人工跑一次。
//! 跑法（PowerShell，Key 只放当前进程环境变量，不落盘、不入库）：
//!
//! ```powershell
//! $env:YUNONG_REAL_API_KEY="sk-..."
//! cargo test -p yunong-core --test real_upstream -- --ignored --nocapture
//! ```
//!
//! 可选覆盖：`YUNONG_REAL_BASE_URL`（默认 `https://api.deepseek.com`）、
//! `YUNONG_REAL_MODEL`（默认 `deepseek-chat`）。

use std::time::{Duration, Instant};

use yunong_core::gateway::{DirectProvider, Flow, ModelGateway};
use yunong_shared::{ChatMessage, ChatRequest, Chunk, FinishReason, Protocol};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn api_key() -> String {
    let key = std::env::var("YUNONG_REAL_API_KEY")
        .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
        .expect("需要环境变量 YUNONG_REAL_API_KEY（或 DEEPSEEK_API_KEY）");
    assert!(key.starts_with("sk-"), "Key 形状不对，拒绝发送");
    key
}

fn request(protocol: Protocol, prompt: &str) -> ChatRequest {
    ChatRequest {
        session_id: "s_real".into(),
        turn_id: "t_real".into(),
        round: 1,
        model: env_or("YUNONG_REAL_MODEL", "deepseek-chat"),
        protocol,
        messages: vec![ChatMessage::user(prompt)],
        tools: vec![],
        max_output_tokens: 64,
        stream: true,
        estimated_input_tokens: 0,
    }
}

/// 真的连一次上游：验证 TLS（rustls）、SSE 解析、`Done` 与 usage 合并。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要真实上游 Key（YUNONG_REAL_API_KEY）"]
async fn real_upstream_streams_end_to_end() {
    let provider = DirectProvider::new(
        Protocol::OpenaiCompletions,
        env_or("YUNONG_REAL_BASE_URL", "https://api.deepseek.com"),
        env_or("YUNONG_REAL_MODEL", "deepseek-chat"),
    )
    .with_api_key(api_key());

    let req = request(Protocol::OpenaiCompletions, "用一句话回答：1+1 等于几？");

    let mut texts = String::new();
    let mut saw_done = false;
    let mut saw_usage = false;
    let mut on_chunk = |chunk: Chunk| {
        match &chunk {
            Chunk::Delta { text } => texts.push_str(text),
            Chunk::Usage { .. } => saw_usage = true,
            Chunk::Done { .. } => saw_done = true,
            _ => {}
        }
        Flow::Continue
    };

    let started = Instant::now();
    let outcome = provider
        .chat_stream(&req, &mut on_chunk)
        .await
        .expect("真实上游调用失败（检查网络/代理/Key）");
    let elapsed = started.elapsed();

    println!("--- 真实上游结果 ---");
    println!("耗时      : {elapsed:?}");
    println!("回复文本  : {texts:?}");
    println!("finish    : {:?}", outcome.finish);
    println!("usage     : {:?}", outcome.usage);
    println!("request_id: {:?}", outcome.request_id);

    assert!(!texts.trim().is_empty(), "没收到任何文本增量");
    assert!(saw_done, "流里没有 Done");
    assert_eq!(outcome.finish, FinishReason::Stop, "正常结束应为 Stop");
    assert!(!outcome.cancelled, "不该被取消");
    // usage 是成本监控的关键：真实上游应当给出（DeepSeek 需 stream_options.include_usage）
    assert!(
        saw_usage || outcome.usage.tokens_in > 0 || outcome.usage.tokens_out > 0,
        "拿不到 usage，记账会退化成纯估算"
    );
    assert!(elapsed < Duration::from_secs(90), "首事件+整段超过 90s，超时配置或网络有问题");
}

/// 并发冒烟：用来给「50 并发」这个上限找现实依据，而不是拍脑袋。
///
/// 默认 8 路并发（省 token）；可用 `YUNONG_REAL_CONCURRENCY` 调整。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实上游 Key（YUNONG_REAL_API_KEY）"]
async fn real_upstream_handles_concurrent_streams() {
    let concurrency: usize = env_or("YUNONG_REAL_CONCURRENCY", "8").parse().unwrap_or(8);
    let key = api_key();
    let base = env_or("YUNONG_REAL_BASE_URL", "https://api.deepseek.com");
    let model = env_or("YUNONG_REAL_MODEL", "deepseek-chat");

    let mut handles = Vec::new();
    for i in 0..concurrency {
        let key = key.clone();
        let base = base.clone();
        let model = model.clone();
        handles.push(tokio::spawn(async move {
            let provider = DirectProvider::new(Protocol::OpenaiCompletions, base, model)
                .with_api_key(key);
            let req = request(Protocol::OpenaiCompletions, &format!("只回答一个字：{i}"));
            let started = Instant::now();
            let mut first_byte: Option<Duration> = None;
            let mut on_chunk = |_c: Chunk| {
                if first_byte.is_none() {
                    first_byte = Some(started.elapsed());
                }
                Flow::Continue
            };
            match provider.chat_stream(&req, &mut on_chunk).await {
                Ok(outcome) => Ok((started.elapsed(), first_byte, outcome.usage)),
                Err(err) => Err(format!("{err}")),
            }
        }));
    }

    let started = Instant::now();
    let mut ok = 0usize;
    let mut failures = Vec::new();
    let mut ttfts = Vec::new();
    for handle in handles {
        match handle.await.expect("任务 panic") {
            Ok((total, ttft, usage)) => {
                ok += 1;
                if let Some(t) = ttft {
                    ttfts.push(t);
                }
                println!("  ✓ {total:?}  ttft={ttft:?}  usage={usage:?}");
            }
            Err(err) => {
                failures.push(err.clone());
                println!("  ✗ {err}");
            }
        }
    }
    let wall = started.elapsed();
    ttfts.sort();
    println!("--- 并发 {concurrency} 结果 ---");
    println!("总耗时: {wall:?}；成功 {ok}；失败 {}", failures.len());
    if let Some(p50) = ttfts.get(ttfts.len() / 2) {
        println!("TTFT 中位数: {p50:?}");
    }
    assert_eq!(failures.len(), 0, "有 {}/{} 路失败：{:?}", failures.len(), concurrency, failures);
}
