//! C ABI 探针：**不经过 Dart**，直接按 C ABI 调 `yunong_chat_stream`，回答
//!
//! > "用真实 C ABI + 回调，chunk 能不能到？"
//!
//! 起一个手写的假上游（`std::net::TcpListener`，零依赖），返回固定 SSE 流，然后
//! 用 `extern "C" fn` 回调把每条 chunk 打印出来，最后统计：
//! 回调次数、首条/末条、是否收到 `done`、`yunong_chat_stream` 返回的 handle。
//!
//! ```text
//! cargo run -p yunong-core --example chat_ffi_probe
//! cargo run -p yunong-core --example chat_ffi_probe -- --cancel-after 3
//! cargo run -p yunong-core --example chat_ffi_probe -- --bad-url
//! ```
//!
//! 退出码：`0` = 符合预期；`2` = 超时没收到 `done`；`3` = 一条 chunk 都没收到；
//! `4` = 期望收到错误 chunk 却收不到（错误路径断了）。

use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use yunong_core::ffi::{yunong_chat_cancel, yunong_chat_stream};

/// 回调里累积的状态。
///
/// 回调是 `extern "C" fn`，**不能捕获环境**，所以所有共享状态都挂在 `user` 指针上。
struct Sink {
    chunks: Mutex<Vec<String>>,
    count: AtomicUsize,
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
    println!("[probe] chunk #{n:<3} {}", brief(&text, 180));
    if let Ok(mut chunks) = sink.chunks.lock() {
        chunks.push(text);
    }

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

/// 取 `delta.text`（文本增量）用于展示。
fn delta_text(chunk: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(chunk).ok()?;
    value
        .get("text")
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

/// 上游收到的请求行（诊断 `base_url` 用）。
fn start_fake_sse() -> (u16, Arc<Mutex<Option<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("绑定假上游端口失败");
    let port = listener.local_addr().expect("取端口失败").port();
    let seen = Arc::new(Mutex::new(None));
    let seen_in_thread = Arc::clone(&seen);
    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            serve_sse(stream, &seen_in_thread);
        }
    });
    (port, seen, handle)
}

fn serve_sse(mut stream: TcpStream, seen: &Arc<Mutex<Option<String>>>) {
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

    // 逐字 delta（OpenAI Completions 的形状，与本仓库 fake_sse 一致）
    for text in ["你", "好", "，", "探", "针"] {
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
        if write_sse(&mut stream, &format!("data: {payload}\n\n")).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    // 结束帧 + usage
    let finish = serde_json::json!({
        "id": "chatcmpl-probe",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "gpt-probe",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12 }
    });
    let _ = write_sse(&mut stream, &format!("data: {finish}\n\n"));
    let _ = write_sse(&mut stream, "data: [DONE]\n\n");
    let _ = stream.flush();
}

fn write_sse(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    stream.write_all(text.as_bytes())?;
    stream.flush()
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
}

fn parse_args() -> Args {
    let mut args = Args {
        cancel_after: 0,
        bad_url: false,
        timeout_ms: 10_000,
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
            "--bad-url" => args.bad_url = true,
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
    println!("=== C ABI 探针：yunong_chat_stream ===");

    // 1) 假上游（--bad-url 时故意指向一个没人监听的端口）
    let (port, seen, upstream) = if args.bad_url {
        println!("模式: --bad-url（指向未监听的端口，验证错误能否回调到 UI）");
        (1_u16, Arc::new(Mutex::new(None)), thread::spawn(|| {}))
    } else {
        let (port, seen, handle) = start_fake_sse();
        println!("假上游: http://127.0.0.1:{port}/v1（SSE：5 条逐字 delta + usage + [DONE]）");
        (port, seen, handle)
    };

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
            // 取消后不该再有 done：取消一旦发出、且已到第 N 条，就算 settled
            // （取消是"停止读取"，之后不会再有 chunk，所以不能等第 N+1 条）
            sink.cancel_sent.load(Ordering::SeqCst) && count >= args.cancel_after
        } else {
            sink.has_done() || sink.has_error()
        };
        if settled || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // 取消路径：取消后**不该再有任何 chunk**。上面按"收到第 N 条"就已经 settle 了，
    // 这里再等一小段确认真的静默（而不是刚好卡在两次 chunk 之间）。
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
    // 取消/错误路径下流可能还在收尾，给一点时间让后面的 chunk 落进来（用于观察是否真的停）
    thread::sleep(Duration::from_millis(200));

    // 5) 汇总
    let chunks = sink.snapshot();
    let count = chunks.len();
    let first = chunks.first().map(|c| brief(c, 120)).unwrap_or_default();
    let last = chunks.last().map(|c| brief(c, 120)).unwrap_or_default();
    let done = sink.has_done();
    let errored = sink.has_error();
    let types: Vec<String> = chunks.iter().filter_map(|c| chunk_type(c)).collect();
    let text: String = chunks.iter().filter_map(|c| delta_text(c)).collect();
    if let Some(seen) = seen.lock().ok().and_then(|slot| slot.clone()) {
        println!("[probe] 上游收到的请求行: {seen}");
    }

    println!("=== 汇总 ===");
    println!("handle            = {handle}");
    println!("回调次数          = {count}");
    println!("chunk 类型序列    = {types:?}");
    println!("拼出的文本        = {text:?}");
    println!("首条              = {first}");
    println!("末条              = {last}");
    println!("收到 done         = {done}");
    println!("收到 error        = {errored}");
    println!("耗时              = {} ms", elapsed.as_millis());

    let _ = upstream.join();

    // 6) 判定
    if count == 0 {
        eprintln!("\n❌ 结论：一条 chunk 都没到回调 → **FFI 这条路不通**（Rust 侧问题）");
        std::process::exit(3);
    }
    if expect_error {
        if errored {
            println!("\n✅ 结论：错误路径也走回调（UI 能收到 Chunk::Error，不会只是转圈）");
            return;
        }
        eprintln!("\n❌ 结论：期望收到 error chunk，但没收到 → 错误被吞了（UI 会一直转圈）");
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
    if done && count > 0 {
        println!("\n✅ 结论：**C ABI + 回调通**——chunk 能到，done 能到（问题不在 Rust 侧）");
        return;
    }
    eprintln!("\n❌ 结论：收到 {count} 条但没等到 done → 流没走完（超时或上游卡住）");
    std::process::exit(2);
}
