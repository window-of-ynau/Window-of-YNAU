//! 标题栏探针：给**正在运行的**云农之窗窗口切标题栏深/浅色（做截图对比用）。
//!
//! ```text
//! cargo run -p yunong-core --example titlebar_probe -- which   # 只找窗口，不改
//! cargo run -p yunong-core --example titlebar_probe -- dark
//! cargo run -p yunong-core --example titlebar_probe -- light
//! cargo run -p yunong-core --example titlebar_probe -- demo    # 自建窗口，深/浅各量一次像素
//! ```
//!
//! `demo` 模式不依赖 App：自己注册一个**同名窗口类**（`FLUTTER_RUNNER_WIN32_WINDOW`）的窗口，
//! 依次 `set_dark(true)` / `set_dark(false)`，再用 `GetPixel` 采标题栏那一行的像素颜色，
//! 用来客观证明 DWM 属性在这台机器上真的生效（截图之外多一层数字证据）。
//!
//! 退出码：`0` = 成功（`which` 找到窗口也算成功）；`2` = 没找到窗口；`3` = DWM 失败；`4` = 非 Windows。

use yunong_core::titlebar::{find_flutter_window, set_dark, TitlebarError};

fn main() {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "which".to_owned());
    match mode.as_str() {
        "which" => match find_flutter_window() {
            Some(hwnd) => {
                println!("找到云农之窗窗口：hwnd = {hwnd}");
            }
            None => {
                eprintln!("没找到 FLUTTER_RUNNER_WIN32_WINDOW 窗口（App 没启动？）");
                std::process::exit(2);
            }
        },
        "dark" | "light" => {
            let want_dark = mode == "dark";
            match set_dark(want_dark) {
                Ok(report) => println!(
                    "已设置标题栏：dark={}，DWM 属性号={}，hwnd={}",
                    report.dark, report.attribute, report.hwnd
                ),
                Err(TitlebarError::WindowNotFound) => {
                    eprintln!("没找到窗口：标题栏要在窗口创建之后才能设置");
                    std::process::exit(2);
                }
                Err(err @ TitlebarError::DwmFailed(_)) => {
                    eprintln!("DWM 调用失败：{}", err.message());
                    std::process::exit(3);
                }
                Err(err @ TitlebarError::Unsupported) => {
                    eprintln!("{}", err.message());
                    std::process::exit(4);
                }
            }
        }
        "demo" => run_demo(),
        other => {
            eprintln!("未知参数：{other}（可用 which / dark / light / demo）");
            std::process::exit(1);
        }
    }
}

/// `demo`：自建一个 Flutter 同名窗口，深/浅各切一次并量像素。
#[cfg(not(windows))]
fn run_demo() {
    eprintln!("demo 模式只在 Windows 上有意义");
    std::process::exit(4);
}

#[cfg(windows)]
fn run_demo() {
    let hwnd = unsafe { create_probe_window() };
    println!("已创建探针窗口：hwnd = {}", hwnd as usize);
    // 置顶 + 前台：否则控制台窗口会盖住它，采到的是控制台的像素
    const HWND_TOPMOST: *mut std::ffi::c_void = -1_isize as *mut std::ffi::c_void;
    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, 700, 380, 900, 600, 0x0040); // SWP_SHOWWINDOW
        SetForegroundWindow(hwnd);
    }
    pump_messages(200);

    // 初始（系统是浅色 → 应采到浅色栏）
    println!("初始标题栏像素 = {}", unsafe {
        sample_caption(hwnd)
    });

    // 深色
    match set_dark(true) {
        Ok(report) => println!(
            "set_dark(true) → dark={}，DWM 属性号={}，hwnd={}",
            report.dark, report.attribute, report.hwnd
        ),
        Err(err) => {
            eprintln!("set_dark(true) 失败：{err:?}");
            std::process::exit(3);
        }
    }
    // DWM 是异步合成的：强制重绘 + 给一帧时间再量像素
    unsafe { force_redraw(hwnd) };
    std::thread::sleep(std::time::Duration::from_millis(600));
    println!("深色标题栏像素 = {}", unsafe {
        sample_caption(hwnd)
    });

    // 浅色
    if let Err(err) = set_dark(false) {
        eprintln!("set_dark(false) 失败：{err:?}");
        std::process::exit(3);
    }
    unsafe { force_redraw(hwnd) };
    std::thread::sleep(std::time::Duration::from_millis(600));
    println!("浅色标题栏像素 = {}", unsafe {
        sample_caption(hwnd)
    });

    unsafe { destroy_probe_window(hwnd) };
    println!("探针窗口已销毁");
}

// ---------------------------------------------------------------------------
// 自建窗口 + 采像素（只用 std + 手写 Win32 声明，零依赖）
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[repr(C)]
struct WndClassW {
    style: u32,
    lpfn_wnd_proc: Option<extern "system" fn(*mut std::ffi::c_void, u32, usize, isize) -> isize>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: *mut std::ffi::c_void,
    h_icon: *mut std::ffi::c_void,
    h_cursor: *mut std::ffi::c_void,
    hbr_background: *mut std::ffi::c_void,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
}

#[cfg(windows)]
#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: *mut std::ffi::c_void,
        menu: *mut std::ffi::c_void,
        instance: *mut std::ffi::c_void,
        param: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn DefWindowProcW(hwnd: *mut std::ffi::c_void, msg: u32, wparam: usize, lparam: isize)
        -> isize;
    fn DestroyWindow(hwnd: *mut std::ffi::c_void) -> i32;
    fn ShowWindow(hwnd: *mut std::ffi::c_void, cmd: i32) -> i32;
    fn UpdateWindow(hwnd: *mut std::ffi::c_void) -> i32;
    fn GetWindowRect(hwnd: *mut std::ffi::c_void, rect: *mut Rect) -> i32;
    fn SetWindowPos(
        hwnd: *mut std::ffi::c_void,
        insert_after: *mut std::ffi::c_void,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        flags: u32,
    ) -> i32;
    fn GetDC(hwnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn ReleaseDC(hwnd: *mut std::ffi::c_void, dc: *mut std::ffi::c_void) -> i32;
    fn PeekMessageW(
        msg: *mut std::ffi::c_void,
        hwnd: *mut std::ffi::c_void,
        filter_min: u32,
        filter_max: u32,
        remove: u32,
    ) -> i32;
    fn TranslateMessage(msg: *const std::ffi::c_void) -> i32;
    fn DispatchMessageW(msg: *const std::ffi::c_void) -> isize;
}

#[link(name = "gdi32")]
extern "system" {
    fn GetPixel(dc: *mut std::ffi::c_void, x: i32, y: i32) -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn SetForegroundWindow(hwnd: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
extern "system" fn probe_wnd_proc(
    hwnd: *mut std::ffi::c_void,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    // WM_DESTROY 时不退出进程（探针自己控制生命周期），其余交给默认处理
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 注册一个**与 Flutter runner 同名**的窗口类，创建并显示窗口。
#[cfg(windows)]
unsafe fn create_probe_window() -> *mut std::ffi::c_void {
    let class_name = wide("FLUTTER_RUNNER_WIN32_WINDOW");
    let title = wide("云农之窗 · 标题栏探针");
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = WndClassW {
        style: 0,
        lpfn_wnd_proc: Some(probe_wnd_proc),
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance: instance,
        h_icon: std::ptr::null_mut(),
        h_cursor: std::ptr::null_mut(),
        hbr_background: std::ptr::null_mut(),
        lpsz_menu_name: std::ptr::null(),
        lpsz_class_name: class_name.as_ptr(),
    };
    unsafe { RegisterClassW(&class) };
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            0x00CF_0000, // WS_OVERLAPPEDWINDOW
            120,
            120,
            900,
            600,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    if hwnd.is_null() {
        eprintln!("CreateWindowExW 失败");
        std::process::exit(3);
    }
    unsafe {
        ShowWindow(hwnd, 5); // SW_SHOW
        UpdateWindow(hwnd);
    }
    // 让窗口先画出来（并泵一点消息，否则界面是白的死窗口）
    pump_messages(300);
    hwnd
}

/// 让 DWM 重新合成框架（否则改了属性屏幕上的栏可能还是旧的）。
#[cfg(windows)]
unsafe fn force_redraw(hwnd: *mut std::ffi::c_void) {
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_FRAMECHANGED: u32 = 0x0020;
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
    pump_messages(120);
}

#[cfg(windows)]
unsafe fn destroy_probe_window(hwnd: *mut std::ffi::c_void) {
    unsafe {
        DestroyWindow(hwnd);
    }
    pump_messages(50);
}

/// 泵消息若干毫秒（让窗口能重绘）。
#[cfg(windows)]
fn pump_messages(millis: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(millis);
    let mut msg = [0_u8; 64];
    while std::time::Instant::now() < deadline {
        let has = unsafe {
            PeekMessageW(
                msg.as_mut_ptr() as *mut std::ffi::c_void,
                std::ptr::null_mut(),
                0,
                0,
                1, // PM_REMOVE
            )
        };
        if has != 0 {
            unsafe {
                TranslateMessage(msg.as_ptr() as *const std::ffi::c_void);
                DispatchMessageW(msg.as_ptr() as *const std::ffi::c_void);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 采"标题栏那一行"的屏幕像素（用屏幕 DC，绕开 DWM 合成区域读不到的问题）。
#[cfg(windows)]
unsafe fn sample_caption(hwnd: *mut std::ffi::c_void) -> String {
    let mut rect = Rect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetWindowRect(hwnd, &mut rect) };
    // 标题栏高度随 DPI 变：取窗口顶部往下 14px，横向避开图标区
    let x = rect.left + 200;
    let y = rect.top + 14;
    let dc = unsafe { GetDC(std::ptr::null_mut()) };
    if dc.is_null() {
        return "（GetDC 失败）".to_owned();
    }
    let pixel = unsafe { GetPixel(dc, x, y) };
    unsafe { ReleaseDC(std::ptr::null_mut(), dc) };
    if pixel == u32::MAX {
        return "（GetPixel 失败）".to_owned();
    }
    let red = pixel & 0xFF;
    let green = (pixel >> 8) & 0xFF;
    let blue = (pixel >> 16) & 0xFF;
    format!("RGB({red}, {green}, {blue})（屏幕坐标 {x},{y}）")
}
