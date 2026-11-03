//! C ABI 探针：**不经过 Dart**，直接按 C ABI 调 `yunong_chat_stream`，回答
//!
//! > "用真实 C ABI + 回调，chunk 能不能到？"
//!
//! 起一个手写的假上游（`std::net::TcpListener`，零依赖），返回固定 SSE 流，然后
//! 用 `extern "C" fn` 回调把每条 chunk 打印出来，最后统计：
//! 回调次数、首条/末条、是否收到 `done`、`yunong_chat_stream` 返回的 handle，
//! 以及 **上游实际发出多少帧 vs 回调收到多少帧**（排查"说到一半就不说了"）。
//!
//! ```text
//! cargo run -p yunong-core --example chat_ffi_probe
//! cargo run -p yunong-core --example chat_ffi_probe -- --cancel-after 3
//! cargo run -p yunong-core --example chat_ffi_probe -- --bad-url
//! cargo run -p yunong-core --example chat_ffi_probe -- --frames 50 --interval-ms 50
//! cargo run -p yunong-core --example chat_ffi_probe -- --frames 50 --interval-ms 50 --split
//! ```
//!
//! 退出码：`0` = 符合预期；`2` = 超时/没收齐；`3` = 一条 chunk 都没收到；
//! `4` = 期望收到错误 chunk 却收不到；`5` = 上游发了 N 帧但回调只收到 M 帧（提前断流）。

use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use yunong_core::ffi::{yunong_chat_cancel, yunong_chat_stream, yunong_free_string};

/// 回调里累积的状态。
///
/// 回调是 `extern "C" fn`，**不能捕获环境**，所以所有共享状态都挂在 `user` 指针上。
struct Sink {
    chunks: Mutex<Vec<String>>,
    count: AtomicUsize,
    /// 收到的 `delta` 条数（和上游发的帧数对账）
    deltas: AtomicUsize,
    /// 泄漏模式用：不打印每条 chunk
    quiet: bool,

    /// `>0` 时收到第 N 条后主动取消（验证取消路径）
    cancel_after: usize,
    handle: AtomicI64,
    cancel_sent: AtomicBool,
}

impl Sink {
    fn new(cancel_after: usize) -> Self {
        Self {
            chunks: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
            deltas: AtomicUsize::new(0),
            quiet: false,
            cancel_after,
            handle: AtomicI64::new(0),
            cancel_sent: AtomicBool::new(false),
        }
    }

    fn snapshot(&self) -> Vec<String> {
        self.chunks.lock().map(|v| v.clone()).unwrap_or_default()
    }

    fn has_done(&self) -> bool {
        self.snapshot()
            .iter()
            .any(|chunk| chunk_type(chunk).as_deref() == Some("done"))
    }

    fn has_error(&self) -> bool {
        self.snapshot()
            .iter()
            .any(|chunk| chunk_type(chunk).as_deref() == Some("error"))
    }
}

/// C ABI 回调本体（签名必须与 `ChatChunkCallback` 完全一致）。
///
/// # Safety
///
/// `json` 由 Rust 侧保证在本次调用期间有效、NUL 结尾；`user` 是 `Sink` 的裸指针。
unsafe extern "C" fn on_chunk(json: *const std::os::raw::c_char, user: *mut std::os::raw::c_void) {
    if json.is_null() {
        eprintln!("[probe] ❌ 回调收到 NULL 指针");
        return;
    }
    let text = unsafe { CStr::from_ptr(json) }
        .to_string_lossy()
        .into_owned();
    let Some(sink) = (unsafe { (user as *const Sink).as_ref() }) else {
        eprintln!("[probe] ❌ 回调的 user 指针为空");
        return;
    };
    let n = sink.count.fetch_add(1, Ordering::SeqCst) + 1;
    if chunk_type(&text).as_deref() == Some("delta") {
        sink.deltas.fetch_add(1, Ordering::SeqCst);
    }
    if !sink.quiet {
        println!("[probe] chunk #{n:<3} {}", brief(&text, 160));
    }
    if let Ok(mut chunks) = sink.chunks.lock() {
        chunks.push(text);
    }
    // T82 契约：chunk_json 的所有权已转移给我们，拷完必须释放（等价于 Dart 侧 yunong_free_string）
    unsafe { yunong_free_string(json as *mut std::os::raw::c_char) };

    // 取消路径：收到第 N 条后调 `yunong_chat_cancel`，观察后续是否停下
    if sink.cancel_after > 0
        && n == sink.cancel_after
        && !sink.cancel_sent.swap(true, Ordering::SeqCst)
    {
        let handle = sink.handle.load(Ordering::SeqCst);
        let code = yunong_chat_cancel(handle);
        println!("[probe] ⏹  第 {n} 条后主动取消（handle={handle}，返回值={code}）");
    }
}

/// 从 chunk JSON 里取 `type`（只做展示与断言，不依赖 serde 的强类型）。
fn chunk_type(chunk: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(chunk).ok()?;
    value
        .get("type")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// 从 chunk JSON 里取增量文本或错误信息（展示用）。
fn chunk_body(chunk: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(chunk).ok()?;
    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_owned());
    }
    value
        .get("message")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn brief(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let head: String = flat.chars().take(max).collect();
        format!("{head}…")
    }
}

// ---------------------------------------------------------------------------
// 假上游：手写 HTTP + SSE（零依赖，只用 std::net）
// ---------------------------------------------------------------------------

/// 假上游的行为参数（对应命令行开关）。
#[derive(Clone, Copy)]
struct UpstreamSpec {
    frames: usize,
    interval: Duration,
    /// 每帧拆成两次 write（模拟"帧被 TCP 切开"）
    split: bool,
    /// 用 CRLF 收尾而不是 LF
    crlf: bool,
    /// 不发 `data: [DONE]`，直接关连接
    no_done: bool,
    /// 每帧 delta 额外填充多少字节（泄漏测试用；0 = 只发 `字{i}`）
    payload_bytes: usize,
}

struct UpstreamStats {
    /// 上游**成功写出去**的 delta 帧数
    deltas_sent: AtomicUsize,
    finish_sent: AtomicBool,
}

impl UpstreamStats {
    fn new() -> Self {
        Self {
            deltas_sent: AtomicUsize::new(0),
            finish_sent: AtomicBool::new(false),
        }
    }
}

/// 起好的假上游（端口 + 观测点）。
struct FakeUpstream {
    port: u16,
    /// 上游收到的请求行（诊断 `base_url` 用）
    seen: Arc<Mutex<Option<String>>>,
    stats: Arc<UpstreamStats>,
    handle: thread::JoinHandle<()>,
}

fn start_fake_sse(spec: UpstreamSpec) -> FakeUpstream {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("绑定假上游端口失败");
    let port = listener.local_addr().expect("取端口失败").port();
    let seen = Arc::new(Mutex::new(None));
    let stats = Arc::new(UpstreamStats::new());
    let seen_in_thread = Arc::clone(&seen);
    let stats_in_thread = Arc::clone(&stats);
    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            serve_sse(stream, &seen_in_thread, &stats_in_thread, spec);
        }
    });
    FakeUpstream {
        port,
        seen,
        stats,
        handle,
    }
}

fn serve_sse(
    mut stream: TcpStream,
    seen: &Arc<Mutex<Option<String>>>,
    stats: &Arc<UpstreamStats>,
    spec: UpstreamSpec,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 2048];
    // 读到请求头结束（+ 按 Content-Length 读满 body，避免对面还在写就回包）
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
        if let Some(end) = find_headers_end(&buffer) {
            let head = String::from_utf8_lossy(&buffer[..end]).to_string();
            if let Ok(mut slot) = seen.lock() {
                *slot = Some(head.lines().next().unwrap_or_default().to_owned());
            }
            let content_length = parse_content_length(&head);
            if buffer.len() >= end + content_length {
                break;
            }
        }
    }

    // 响应头：SSE + close（不给 keep-alive，读完就关）
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\n\
                Cache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }

    let sep = if spec.crlf { "\r\n\r\n" } else { "\n\n" };
    for index in 0..spec.frames {
        let text = if spec.payload_bytes == 0 {
            format!("字{index}")
        } else {
            format!("字{index}{}", "x".repeat(spec.payload_bytes))
        };
        let payload = serde_json::json!({
            "id": "chatcmpl-probe",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "gpt-probe",
            "choices": [{
                "index": 0,
                "delta": { "content": text },
                "finish_reason": serde_json::Value::Null
            }]
        });
        let frame = format!("data: {payload}{sep}");
        let wrote = if spec.split {
            // 拆成两次写：先把前半截发出去，睡一会儿再发剩下的（模拟 TCP 切开一帧）
            let (head_part, tail_part) = frame.split_at(frame.len() / 2);
            stream.write_all(head_part.as_bytes()).is_ok() && stream.flush().is_ok() && {
                thread::sleep(spec.interval / 2);
                stream.write_all(tail_part.as_bytes()).is_ok()
            }
        } else {
            stream.write_all(frame.as_bytes()).is_ok()
        };
        if !wrote {
            // 对面不再读了（取消）：上游侧也停，并记录已发多少
            return;
        }
        let _ = stream.flush();
        stats.deltas_sent.fetch_add(1, Ordering::SeqCst);
        if index + 1 < spec.frames {
            thread::sleep(spec.interval);
        }
    }

    // 结束帧 + usage
    let finish = serde_json::json!({
        "id": "chatcmpl-probe",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "gpt-probe",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 7,
            "completion_tokens": spec.frames,
            "total_tokens": 7 + spec.frames
        }
    });
    if stream
        .write_all(format!("data: {finish}{sep}").as_bytes())
        .is_ok()
    {
        stats.finish_sent.store(true, Ordering::SeqCst);
    }
    if !spec.no_done {
        let _ = stream.write_all(format!("data: [DONE]{sep}").as_bytes());
    }
    let _ = stream.flush();
}

fn find_headers_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
}

fn parse_content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 探针主体
// ---------------------------------------------------------------------------

struct Args {
    cancel_after: usize,
    bad_url: bool,
    timeout_ms: u64,
    frames: usize,
    interval_ms: u64,
    split: bool,
    crlf: bool,
    no_done: bool,
    /// `--leak N`：跑 N 次完整流做泄漏检查（0 = 不跑）
    leak_rounds: usize,
}

fn parse_args() -> Args {
    let mut args = Args {
        cancel_after: 0,
        bad_url: false,
        timeout_ms: 10_000,
        frames: 5,
        interval_ms: 20,
        split: false,
        crlf: false,
        no_done: false,
        leak_rounds: 0,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--cancel-after" => {
                args.cancel_after = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--cancel-after 需要一个数字");
            }
            "--frames" => {
                args.frames = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--frames 需要一个数字");
            }
            "--interval-ms" => {
                args.interval_ms = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--interval-ms 需要一个数字");
            }
            "--bad-url" => args.bad_url = true,
            "--split" => args.split = true,
            "--crlf" => args.crlf = true,
            "--no-done" => args.no_done = true,
            "--leak" => {
                args.leak_rounds = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--leak 需要一个数字");
            }
            "--timeout-ms" => {
                args.timeout_ms = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--timeout-ms 需要一个数字");
            }
            other => panic!("未知参数：{other}"),
        }
    }
    args
}

fn main() {
    let args = parse_args();
    if args.leak_rounds > 0 {
        run_leak(args.leak_rounds);
        return;
    }
    println!("=== C ABI 探针：yunong_chat_stream ===");

    // 1) 假上游（--bad-url 时故意指向一个没人监听的端口）
    let upstream = if args.bad_url {
        println!("模式: --bad-url（指向未监听的端口，验证错误能否回调到 UI）");
        FakeUpstream {
            port: 1,
            seen: Arc::new(Mutex::new(None)),
            stats: Arc::new(UpstreamStats::new()),
            handle: thread::spawn(|| {}),
        }
    } else {
        let spec = UpstreamSpec {
            frames: args.frames,
            interval: Duration::from_millis(args.interval_ms),
            split: args.split,
            crlf: args.crlf,
            no_done: args.no_done,
            payload_bytes: 0,
        };
        let upstream = start_fake_sse(spec);
        println!(
            "假上游: http://127.0.0.1:{}/v1（{} 帧 delta，间隔 {} ms{}{}）",
            upstream.port,
            args.frames,
            args.interval_ms,
            if args.split {
                "，每帧拆两次写"
            } else {
                ""
            },
            if args.no_done {
                "，不发 [DONE] 直接关连接"
            } else {
                "，随后 usage + [DONE]"
            }
        );
        upstream
    };
    let port = upstream.port;
    let seen = Arc::clone(&upstream.seen);
    let stats = Arc::clone(&upstream.stats);

    // 2) 组装 Dart 侧会传的 request JSON（字段与 ChatRunConfig 一致）
    let request = serde_json::json!({
        "channel": "byok",
        "protocol": "openai_completions",
        "model": "gpt-probe",
        "base_url": format!("http://127.0.0.1:{port}/v1"),
        "api_key": "sk-probe-not-a-real-key",
        "messages": [{ "role": "user", "content": "说三个字" }],
        "max_output_tokens": 64
    });
    let request =
        CString::new(serde_json::to_string(&request).expect("序列化请求失败")).expect("请求含 NUL");
    let db_path = CString::new("").expect("空串");

    // 3) 通过 C ABI 启动流；回调状态挂在 user 指针上
    let sink = Arc::new(Sink::new(args.cancel_after));
    let user = Arc::as_ptr(&sink) as *mut std::os::raw::c_void;
    let started = Instant::now();
    let handle =
        unsafe { yunong_chat_stream(request.as_ptr(), db_path.as_ptr(), Some(on_chunk), user) };
    println!("[probe] yunong_chat_stream 返回 handle = {handle}（0 = 入参被拒，不会起线程）");
    if handle <= 0 {
        eprintln!("[probe] ❌ handle 为 0：入参校验就失败了，线程根本没启动");
        std::process::exit(3);
    }
    sink.handle.store(handle, Ordering::SeqCst);

    // 4) 等流结束（done / error / 取消后静默）
    let deadline = Instant::now() + Duration::from_millis(args.timeout_ms);
    let expect_error = args.bad_url;
    loop {
        let count = sink.count.load(Ordering::SeqCst);
        let settled = if expect_error {
            sink.has_error()
        } else if args.cancel_after > 0 {
            sink.cancel_sent.load(Ordering::SeqCst) && count >= args.cancel_after
        } else {
            sink.has_done() || sink.has_error()
        };
        if settled || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // 取消路径：确认之后真的静默（不该再有新 chunk）
    if args.cancel_after > 0 && sink.cancel_sent.load(Ordering::SeqCst) {
        let quiet_from = sink.count.load(Ordering::SeqCst);
        let quiet_deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < quiet_deadline {
            thread::sleep(Duration::from_millis(20));
            if sink.count.load(Ordering::SeqCst) != quiet_from {
                println!("[probe] ⚠️  取消后仍收到新 chunk（取消没真正停流）");
                break;
            }
        }
    }
    let elapsed = started.elapsed();
    thread::sleep(Duration::from_millis(100));

    // 5) 汇总
    let chunks = sink.snapshot();
    let count = chunks.len();
    let deltas = sink.deltas.load(Ordering::SeqCst);
    let sent = stats.deltas_sent.load(Ordering::SeqCst);
    let finish_sent = stats.finish_sent.load(Ordering::SeqCst);
    let first = chunks.first().map(|c| brief(c, 120)).unwrap_or_default();
    let last = chunks.last().map(|c| brief(c, 120)).unwrap_or_default();
    let done = sink.has_done();
    let errored = sink.has_error();
    let types: Vec<String> = chunks.iter().filter_map(|c| chunk_type(c)).collect();
    let text: String = chunks.iter().filter_map(|c| chunk_body(c)).collect();
    if let Some(seen) = seen.lock().ok().and_then(|slot| slot.clone()) {
        println!("[probe] 上游收到的请求行: {seen}");
    }

    println!("=== 汇总 ===");
    println!("handle            = {handle}");
    println!("回调次数          = {count}");
    println!("上游发出 delta    = {sent} 帧（结束帧已发：{finish_sent}）");
    println!("回调收到 delta    = {deltas} 帧");
    println!("chunk 类型序列    = {types:?}");
    println!("拼出的文本        = {text:?}");
    println!("首条              = {first}");
    println!("末条              = {last}");
    println!("收到 done         = {done}");
    println!("收到 error        = {errored}");
    println!("耗时              = {} ms", elapsed.as_millis());

    let _ = upstream.handle.join();

    // 6) 判定
    if count == 0 {
        eprintln!("\n❌ 结论：一条 chunk 都没到回调 → **FFI 这条路不通**（Rust 侧问题）");
        std::process::exit(3);
    }
    if expect_error {
        if errored {
            println!("\n✅ 结论：错误路径也走回调（UI 能收到 Chunk::Error）");
            return;
        }
        eprintln!("\n❌ 结论：期望收到 error chunk，但没收到 → 错误被吞了");
        std::process::exit(4);
    }
    if args.cancel_after > 0 {
        if done {
            eprintln!("\n⚠️  取消后仍收到 done（取消没生效？）");
            std::process::exit(2);
        }
        println!(
            "\n✅ 结论：取消路径有效（第 {} 条后停流，未再收到 done）",
            args.cancel_after
        );
        return;
    }
    if deltas < sent {
        eprintln!(
            "\n❌ 结论：**提前断流**——上游发出 {sent} 帧，回调只收到 {deltas} 帧（差 {}）",
            sent - deltas
        );
        std::process::exit(5);
    }
    if done && deltas == sent {
        println!(
            "\n✅ 结论：**C ABI + 回调通**——上游 {sent} 帧全到，done 也到（Rust 侧无提前断流）"
        );
        return;
    }
    eprintln!("\n❌ 结论：收到 {count} 条但没等到 done → 流没走完（超时或上游卡住）");
    std::process::exit(2);
}

// ---------------------------------------------------------------------------
// 泄漏检查：跑 N 次完整流（含回调释放），RSS 不应线性增长
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[repr(C)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut std::os::raw::c_void;
    fn K32GetProcessMemoryInfo(
        process: *mut std::os::raw::c_void,
        counters: *mut ProcessMemoryCounters,
        size: u32,
    ) -> i32;
}

/// 当前进程的常驻内存（字节）；非 Windows 返回 None。
#[cfg(windows)]
fn rss_bytes() -> Option<u64> {
    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<ProcessMemoryCounters>() as u32,
        )
    };
    (ok != 0).then_some(counters.working_set_size as u64)
}

#[cfg(not(windows))]
fn rss_bytes() -> Option<u64> {
    None
}

fn total_rss() -> Option<u64> {
    rss_bytes()
}

/// 跑 `rounds` 次完整流；每次回调都按契约释放指针。若哪里漏了 free，RSS 会跟着涨。
fn run_leak(rounds: usize) {
    println!("=== 泄漏检查：{rounds} 次完整流（每帧 8 KiB，回调内释放）===");
    let before = total_rss();
    let mut chunks_total = 0_usize;
    for round in 0..rounds {
        let spec = UpstreamSpec {
            frames: 8,
            interval: Duration::from_millis(1),
            split: false,
            crlf: false,
            no_done: false,
            payload_bytes: 8192,
        };
        let upstream = start_fake_sse(spec);
        let port = upstream.port;
        let request = serde_json::json!({
            "channel": "byok",
            "protocol": "openai_completions",
            "model": "gpt-probe",
            "base_url": format!("http://127.0.0.1:{port}/v1"),
            "api_key": "sk-probe-not-a-real-key",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let request = CString::new(serde_json::to_string(&request).unwrap()).unwrap();
        let db_path = CString::new("").unwrap();
        let mut sink = Sink::new(0);
        sink.quiet = true;
        let sink = Arc::new(sink);
        let user = Arc::as_ptr(&sink) as *mut std::os::raw::c_void;
        let handle =
            unsafe { yunong_chat_stream(request.as_ptr(), db_path.as_ptr(), Some(on_chunk), user) };
        if handle <= 0 {
            eprintln!("第 {round} 轮 yunong_chat_stream 返回 0（入参被拒）");
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while !sink.has_done() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        let _ = upstream.handle.join();
        chunks_total += sink.count.load(Ordering::SeqCst);
        if (round + 1) % 50 == 0 {
            let now = total_rss();
            println!(
                "  第 {:>3} 轮：累计 {} 帧，RSS = {}",
                round + 1,
                chunks_total,
                now.map(|bytes| format!("{:.1} MiB", bytes as f64 / 1048576.0))
                    .unwrap_or_else(|| "n/a".to_owned())
            );
        }
    }
    let after = total_rss();
    println!(
        "总计 {} 帧；RSS {} → {}（差 {}）",
        chunks_total,
        before
            .map(|b| format!("{:.1} MiB", b as f64 / 1048576.0))
            .unwrap_or_else(|| "n/a".to_owned()),
        after
            .map(|b| format!("{:.1} MiB", b as f64 / 1048576.0))
            .unwrap_or_else(|| "n/a".to_owned()),
        match (before, after) {
            (Some(b), Some(a)) => format!("{:+.1} MiB", (a as f64 - b as f64) / 1048576.0),
            _ => "n/a".to_owned(),
        }
    );
    if chunks_total / rounds.max(1) < 5 {
        eprintln!(
            "⚠️ 每轮平均帧数偏低（{}），泄漏结论仅供参考",
            chunks_total / rounds.max(1)
        );
    }
}
