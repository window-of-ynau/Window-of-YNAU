//! Windows 标题栏跟随 App 主题（DWM 深色模式）。
//!
//! 原生标题栏（最小化/最大化/关闭那一条）默认永远是系统浅色，跟我们的三套主题完全脱节。
//! Windows 10 1809+ 提供了 `DWMWA_USE_IMMERSIVE_DARK_MODE`，把它设成 `TRUE` 就是深色栏。
//!
//! # 拿 hwnd 不需要任何 Dart 插件
//!
//! Flutter Windows runner 的窗口类名固定是 [`FLUTTER_WINDOW_CLASS`]：
//! 先用 `FindWindowW`（快），失败再用 `EnumWindows` + 类名匹配（更稳，能应付
//! "标题栏被改过 / 有多个同名窗口"的情况）。
//!
//! # 属性号有两个值，必须都试
//!
//! | 系统 | `DWMWA_USE_IMMERSIVE_DARK_MODE` |
//! |---|---|
//! | Windows 10 1809 ~ 2004 | **19** |
//! | Windows 10 20H1+ / Windows 11 | **20** |
//!
//! 先试 20，失败（HRESULT != S_OK）再试 19；两个都失败才报错——否则在旧系统上会**静默无效**。
//!
//! # 调用时机
//!
//! **必须在窗口创建之后**调：App 启动时调一次（跟当前主题一致），用户切主题时再调。
//! 窗口还没建好时本函数返回 [`TitlebarError::WindowNotFound`]，调用方重试即可（不要崩）。
//!
//! # 非 Windows
//!
//! 直接返回 [`TitlebarError::Unsupported`]（FFI 层映射成 `UNSUPPORTED`），不 panic、不做任何事。

/// Flutter Windows runner 的窗口类名。
pub const FLUTTER_WINDOW_CLASS: &str = "FLUTTER_RUNNER_WIN32_WINDOW";

/// Windows 10 20H1+ 的属性号。
///
/// `#[cfg(windows)]`：非 Windows 平台用不到这两个常量，不 gate 就会在 Linux CI 上
/// 变成 `dead_code` 警告（`-D warnings` 直接红）。
#[cfg(windows)]
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// Windows 10 1809 ~ 2004 的属性号（同一个属性的旧编号）。
#[cfg(windows)]
const DWMWA_USE_IMMERSIVE_DARK_MODE_OLD: u32 = 19;

/// 设置成功后的报告（便于日志/排障：到底用了哪个属性号、命中的是哪个窗口）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitlebarReport {
    /// 是否已切换成深色
    pub dark: bool,
    /// 实际生效的 DWM 属性号（20 或 19）
    pub attribute: u32,
    /// 命中的窗口句柄（十进制，日志用）
    pub hwnd: u64,
}

/// 失败原因（决定 FFI 返回码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitlebarError {
    /// 非 Windows 平台：没有这个概念
    Unsupported,
    /// 没找到 Flutter 窗口（还没创建 / 已退出）
    WindowNotFound,
    /// 两个属性号都失败（DWM 不可用 / 远程桌面 / 旧系统）
    DwmFailed(String),
}

impl TitlebarError {
    /// FFI 返回码。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "UNSUPPORTED",
            Self::WindowNotFound => "NOT_FOUND",
            Self::DwmFailed(_) => "DWM_FAILED",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Unsupported => "当前平台不支持设置标题栏颜色（仅 Windows）".to_owned(),
            Self::WindowNotFound => {
                "没找到云农之窗的窗口（标题栏设置需在窗口创建之后调用）".to_owned()
            }
            Self::DwmFailed(message) => message.clone(),
        }
    }
}

/// 让 Windows 标题栏跟随主题：`dark = true` 深色栏，`false` 浅色栏。
///
/// 非 Windows 返回 [`TitlebarError::Unsupported`]；找不到窗口返回 `WindowNotFound`。
pub fn set_dark(dark: bool) -> Result<TitlebarReport, TitlebarError> {
    #[cfg(windows)]
    {
        windows_impl::set_dark(dark)
    }
    #[cfg(not(windows))]
    {
        let _ = dark;
        Err(TitlebarError::Unsupported)
    }
}

/// 找到 Flutter 主窗口（窗口还没创建时返回 `None`）。
pub fn find_flutter_window() -> Option<u64> {
    #[cfg(windows)]
    {
        windows_impl::find_window()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_impl {
    use std::ffi::c_void;
    use std::ptr;

    use super::{TitlebarError, TitlebarReport, FLUTTER_WINDOW_CLASS};

    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class_name: *const u16, window_name: *const u16) -> *mut c_void;
        fn EnumWindows(
            callback: extern "system" fn(*mut c_void, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn GetClassNameW(hwnd: *mut c_void, buffer: *mut u16, max_count: i32) -> i32;
    }

    #[link(name = "dwmapi")]
    extern "system" {
        /// 返回 HRESULT：`0`（S_OK）成功。
        fn DwmSetWindowAttribute(
            hwnd: *mut c_void,
            attribute: u32,
            value: *const c_void,
            size: u32,
        ) -> i32;
    }

    /// 把字符串转成 NUL 结尾的 UTF-16。
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 窗口类名是否是我们找的那个。
    fn class_matches(hwnd: *mut c_void) -> bool {
        let mut buffer = [0_u16; 128];
        // 返回的是"写入的字符数（不含 NUL）"，0 表示失败
        let len = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        if len <= 0 {
            return false;
        }
        let name = String::from_utf16_lossy(&buffer[..len as usize]);
        name == FLUTTER_WINDOW_CLASS
    }

    extern "system" fn enum_callback(hwnd: *mut c_void, lparam: isize) -> i32 {
        if class_matches(hwnd) {
            // 把命中的句柄写回调用方，并停止枚举
            let slot = lparam as *mut *mut c_void;
            unsafe { *slot = hwnd };
            return 0; // FALSE：停止
        }
        1 // TRUE：继续
    }

    /// 先 `FindWindowW`（快），再 `EnumWindows` 兜底（标题被改过也能找到）。
    pub(super) fn find_window() -> Option<u64> {
        let class = wide(FLUTTER_WINDOW_CLASS);
        let found = unsafe { FindWindowW(class.as_ptr(), ptr::null()) };
        if !found.is_null() {
            return Some(found as usize as u64);
        }
        let mut hit: *mut c_void = ptr::null_mut();
        let _ = unsafe { EnumWindows(enum_callback, &mut hit as *mut *mut c_void as isize) };
        (!hit.is_null()).then_some(hit as usize as u64)
    }

    pub(super) fn set_dark(dark: bool) -> Result<TitlebarReport, TitlebarError> {
        let hwnd = find_window().ok_or(TitlebarError::WindowNotFound)? as *mut c_void;
        // Win32 BOOL 是 i32；DwmSetWindowAttribute 要求值的字节长度
        let value: i32 = i32::from(dark);
        let mut failures: Vec<(u32, i32)> = Vec::new();
        for attribute in [
            super::DWMWA_USE_IMMERSIVE_DARK_MODE,
            super::DWMWA_USE_IMMERSIVE_DARK_MODE_OLD,
        ] {
            let hr = unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    attribute,
                    &value as *const i32 as *const c_void,
                    std::mem::size_of::<i32>() as u32,
                )
            };
            if hr == 0 {
                return Ok(TitlebarReport {
                    dark,
                    attribute,
                    hwnd: hwnd as usize as u64,
                });
            }
            failures.push((attribute, hr));
        }
        let detail = failures
            .iter()
            .map(|(attribute, hr)| format!("属性 {attribute} 失败（HRESULT 0x{:08X}）", *hr as u32))
            .collect::<Vec<_>>()
            .join("；");
        Err(TitlebarError::DwmFailed(format!(
            "设置标题栏颜色失败：{detail}（远程桌面/精简版系统可能不支持）"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 非 Windows：必须是 `UNSUPPORTED`，绝不 panic。
    #[cfg(not(windows))]
    #[test]
    fn titlebar_is_unsupported_off_windows() {
        let error = set_dark(true).unwrap_err();
        assert_eq!(error.code(), "UNSUPPORTED");
        assert!(error.message().contains("Windows"));
        assert_eq!(find_flutter_window(), None);
    }

    /// Windows：窗口在就跑通（DWM 属性 20 或 19），不在就报 `NOT_FOUND`；两种都不许 panic。
    #[cfg(windows)]
    #[test]
    fn titlebar_applies_or_reports_not_found() {
        match set_dark(true) {
            Ok(report) => {
                assert!(report.dark);
                assert!(
                    report.attribute == 20 || report.attribute == 19,
                    "属性号只能是 20 或 19：{}",
                    report.attribute
                );
                assert_ne!(report.hwnd, 0);
                // 切回去，别把开发机上的窗口留在深色（测试里尽力而为）
                let _ = set_dark(false);
            }
            Err(error) => {
                assert_eq!(error.code(), "NOT_FOUND", "{error:?}");
                assert!(error.message().contains("窗口"));
            }
        }
    }

    /// 找不到窗口时的错误码是明确的（有窗口时这条也成立：返回 Ok 或 NOT_FOUND）。
    #[test]
    fn error_codes_are_stable() {
        assert_eq!(TitlebarError::Unsupported.code(), "UNSUPPORTED");
        assert_eq!(TitlebarError::WindowNotFound.code(), "NOT_FOUND");
        assert_eq!(TitlebarError::DwmFailed("x".into()).code(), "DWM_FAILED");
        assert!(!TitlebarError::DwmFailed("boom".into()).message().is_empty());
    }
}
