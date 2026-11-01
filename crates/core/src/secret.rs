//! BYOK 凭据的**平台安全存储**（W7 P0）。
//!
//! # 硬规矩
//!
//! 1. **明文永不出 Rust**：`get`/`status` 只返回 `{set, masked}`；明文只有
//!    [`KeyStore::load_raw`] 能在发请求前取一次，且不经过任何 FFI 返回值。
//! 2. **禁止自研加密**：Windows 用 DPAPI（`CryptProtectData`，与当前用户绑定）；
//!    Android/其它平台没有等价实现时**降级为内存**（`degraded:true`），
//!    既不会落明文文件，也不会写"自己 AES 再把密钥放同目录"的假加密文件。
//! 3. **换用户读不出来是预期行为**：DPAPI 与 Windows 账户绑定，卸载/换用户后
//!    重新填写即可，不要当 bug 修。
//! 4. key 名只允许 `[a-z0-9_-]{1,64}`（防路径穿越：key 会进文件名）。

use std::collections::HashMap;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::sync::Mutex;

/// key 名规则：小写字母/数字/下划线/连字符，1..=64。
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// 掩码：`sk-****cdef`（保留前 3 + 后 4；太短只显示 `****`）。
pub fn mask_secret(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "****".to_owned();
    }
    let head: String = chars.iter().take(3).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}****{tail}")
}

/// 一次查询/写入的结果（**不含明文**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStatus {
    pub set: bool,
    pub masked: String,
    /// true = 本次只存在内存里（重启失效）
    pub degraded: bool,
    pub reason: Option<String>,
}

impl SecretStatus {
    pub fn unset(degraded: bool, reason: Option<String>) -> Self {
        Self {
            set: false,
            masked: String::new(),
            degraded,
            reason,
        }
    }
}

/// 存储错误（**任何变体都不得携带明文**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// key 名不合法
    InvalidKey(String),
    /// 值为空 / 超长
    InvalidValue(String),
    /// 读写失败（IO / DPAPI 报错）
    Storage(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKey(message) => write!(f, "key 不合法：{message}"),
            Self::InvalidValue(message) => write!(f, "值不合法：{message}"),
            Self::Storage(message) => write!(f, "存储失败：{message}"),
        }
    }
}

/// 凭据存储接口（便于测试注入假实现测降级）。
pub trait KeyStore: Send + Sync {
    fn set(&self, key: &str, value: &str) -> Result<SecretStatus, SecretError>;
    fn status(&self, key: &str) -> Result<SecretStatus, SecretError>;
    fn delete(&self, key: &str) -> Result<bool, SecretError>;
    /// **只在 Rust 内部用**（发请求前取一次）；不参与任何 FFI 返回值。
    fn load_raw(&self, key: &str) -> Result<Option<String>, SecretError>;
    /// 是否降级（内存态）
    fn degraded(&self) -> bool;
    fn reason(&self) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// 内存实现（非 Windows 平台 / DPAPI 不可用时的降级）
// ---------------------------------------------------------------------------

/// 进程内存存储：**重启失效**，明确标记 `degraded`。
#[derive(Default)]
pub struct MemoryKeyStore {
    entries: Mutex<HashMap<String, String>>,
    reason: Option<String>,
}

impl MemoryKeyStore {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            reason: Some(reason.into()),
        }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl KeyStore for MemoryKeyStore {
    fn set(&self, key: &str, value: &str) -> Result<SecretStatus, SecretError> {
        validate(key, value)?;
        self.guard().insert(key.to_owned(), value.to_owned());
        Ok(SecretStatus {
            set: true,
            masked: mask_secret(value),
            degraded: true,
            reason: self.reason.clone(),
        })
    }

    fn status(&self, key: &str) -> Result<SecretStatus, SecretError> {
        if !valid_key(key) {
            return Err(SecretError::InvalidKey(key.to_owned()));
        }
        let guard = self.guard();
        match guard.get(key) {
            Some(value) => Ok(SecretStatus {
                set: true,
                masked: mask_secret(value),
                degraded: true,
                reason: self.reason.clone(),
            }),
            None => Ok(SecretStatus::unset(true, self.reason.clone())),
        }
    }

    fn delete(&self, key: &str) -> Result<bool, SecretError> {
        if !valid_key(key) {
            return Err(SecretError::InvalidKey(key.to_owned()));
        }
        Ok(self.guard().remove(key).is_some())
    }

    fn load_raw(&self, key: &str) -> Result<Option<String>, SecretError> {
        if !valid_key(key) {
            return Err(SecretError::InvalidKey(key.to_owned()));
        }
        Ok(self.guard().get(key).cloned())
    }

    fn degraded(&self) -> bool {
        true
    }

    fn reason(&self) -> Option<String> {
        self.reason.clone()
    }
}

fn validate(key: &str, value: &str) -> Result<(), SecretError> {
    if !valid_key(key) {
        return Err(SecretError::InvalidKey(
            "只允许 [a-z0-9_-]，长度 1..=64".to_owned(),
        ));
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(SecretError::InvalidValue("值不能为空".to_owned()));
    }
    if trimmed.chars().count() > 4096 {
        return Err(SecretError::InvalidValue("值超过 4096 字符".to_owned()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 文件实现（Windows DPAPI）
// ---------------------------------------------------------------------------

/// 文件存储：`<base>/secrets/<key>.bin`（DPAPI 密文） + `<key>.json`（掩码元数据，不含明文）。
#[cfg(windows)]
pub struct DpapiKeyStore {
    dir: PathBuf,
    /// DPAPI 不可用时退化为内存（**绝不写明文文件**）
    fallback: MemoryKeyStore,
    unavailable_reason: Mutex<Option<String>>,
}

#[cfg(windows)]
impl DpapiKeyStore {
    /// 打开（必要时创建）目录。目录创建失败也不 panic：退化成内存态。
    pub fn open(base: impl AsRef<Path>) -> Self {
        let dir = base.as_ref().join("secrets");
        let fallback_reason = match std::fs::create_dir_all(&dir) {
            Ok(()) => None,
            Err(err) => Some(format!("无法创建凭据目录（{err}）：本次仅内存生效")),
        };
        Self {
            dir,
            fallback: MemoryKeyStore::new("DPAPI 不可用：本次仅内存生效"),
            unavailable_reason: Mutex::new(fallback_reason),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn bin_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.bin"))
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    fn mark_unavailable(&self, reason: String) -> Option<String> {
        let mut guard = match self.unavailable_reason.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(reason.clone());
        Some(reason)
    }

    fn unavailable(&self) -> Option<String> {
        let guard = match self.unavailable_reason.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.clone()
    }
}

#[cfg(windows)]
impl KeyStore for DpapiKeyStore {
    fn set(&self, key: &str, value: &str) -> Result<SecretStatus, SecretError> {
        validate(key, value)?;
        if let Some(reason) = self.unavailable() {
            return self.fallback.set(key, value).map(|mut status| {
                status.reason = Some(reason);
                status
            });
        }
        let ciphertext = match dpapi::protect(value.as_bytes()) {
            Ok(bytes) => bytes,
            Err(err) => {
                let reason =
                    self.mark_unavailable(format!("DPAPI 加密失败（{err}）：本次仅内存生效"));
                let mut status = self.fallback.set(key, value)?;
                status.reason = reason;
                return Ok(status);
            }
        };
        // 先写密文，再写元数据（顺序失败也不会留下"有掩码没密文"的假状态）
        if let Err(err) = std::fs::write(self.bin_path(key), &ciphertext) {
            let reason = self.mark_unavailable(format!("密文落盘失败（{err}）：本次仅内存生效"));
            let mut status = self.fallback.set(key, value)?;
            status.reason = reason;
            return Ok(status);
        }
        let meta = serde_json::json!({
            "scheme": "dpapi",
            "masked": mask_secret(value),
            "updated_at": time::OffsetDateTime::now_utc().unix_timestamp(),
        });
        let _ = std::fs::write(
            self.meta_path(key),
            serde_json::to_vec_pretty(&meta).unwrap_or_default(),
        );
        Ok(SecretStatus {
            set: true,
            masked: mask_secret(value),
            degraded: false,
            reason: None,
        })
    }

    fn status(&self, key: &str) -> Result<SecretStatus, SecretError> {
        if !valid_key(key) {
            return Err(SecretError::InvalidKey(key.to_owned()));
        }
        // 只读元数据：不需要解密，也就没有明文泄漏面
        let meta = std::fs::read_to_string(self.meta_path(key)).ok();
        let memory = self.fallback.status(key)?;
        match meta {
            Some(text) => {
                let masked = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("masked")
                            .and_then(|m| m.as_str())
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                Ok(SecretStatus {
                    set: !masked.is_empty(),
                    masked,
                    degraded: false,
                    reason: None,
                })
            }
            None => {
                if memory.set {
                    return Ok(memory);
                }
                Ok(SecretStatus::unset(false, self.unavailable()))
            }
        }
    }

    fn delete(&self, key: &str) -> Result<bool, SecretError> {
        if !valid_key(key) {
            return Err(SecretError::InvalidKey(key.to_owned()));
        }
        let removed_bin = std::fs::remove_file(self.bin_path(key)).is_ok();
        let removed_meta = std::fs::remove_file(self.meta_path(key)).is_ok();
        let removed_memory = self.fallback.delete(key)?;
        Ok(removed_bin || removed_meta || removed_memory)
    }

    fn load_raw(&self, key: &str) -> Result<Option<String>, SecretError> {
        if !valid_key(key) {
            return Err(SecretError::InvalidKey(key.to_owned()));
        }
        match std::fs::read(self.bin_path(key)) {
            Ok(ciphertext) => match dpapi::unprotect(&ciphertext) {
                Ok(plain) => String::from_utf8(plain)
                    .map(Some)
                    .map_err(|_| SecretError::Storage("凭据内容不是 UTF-8".to_owned())),
                Err(err) => Err(SecretError::Storage(format!("DPAPI 解密失败：{err}"))),
            },
            Err(_) => self.fallback.load_raw(key),
        }
    }

    fn degraded(&self) -> bool {
        self.unavailable().is_some()
            || self
                .fallback
                .status("probe")
                .map(|s| s.degraded)
                .unwrap_or(false)
    }

    fn reason(&self) -> Option<String> {
        self.unavailable()
    }
}

/// DPAPI 的最小封装（手写声明，**不引新依赖**）。
///
/// 约束：`CryptProtectData` 与当前 Windows 用户绑定——换用户/重装读不出来是预期行为。
#[cfg(windows)]
#[allow(unsafe_code)]
mod dpapi {
    use std::ffi::c_void;
    use std::ptr;

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            data_descr: *const u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            data_in: *const DataBlob,
            descr_out: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        let input = DataBlob {
            cb_data: plain.len() as u32,
            pb_data: plain.as_ptr() as *mut u8,
        };
        let mut output = DataBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };
        let ok = unsafe {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!(
                "CryptProtectData 失败（{}）",
                std::io::Error::last_os_error()
            ));
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec() };
        unsafe { LocalFree(output.pb_data as *mut c_void) };
        Ok(bytes)
    }

    pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let input = DataBlob {
            cb_data: ciphertext.len() as u32,
            pb_data: ciphertext.as_ptr() as *mut u8,
        };
        let mut output = DataBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!(
                "CryptUnprotectData 失败（{}）",
                std::io::Error::last_os_error()
            ));
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec() };
        unsafe { LocalFree(output.pb_data as *mut c_void) };
        Ok(bytes)
    }
}

/// 按平台选存储：Windows → DPAPI 文件；其它 → 内存（明确降级）。
pub fn default_store(base: impl AsRef<Path>) -> Box<dyn KeyStore> {
    #[cfg(windows)]
    {
        Box::new(DpapiKeyStore::open(base))
    }
    #[cfg(not(windows))]
    {
        let _ = base.as_ref();
        Box::new(MemoryKeyStore::new(
            "当前平台未接入系统安全存储：本次仅内存生效，重启需重新填写",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn temp_base() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yunong-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn key_name_rules() {
        assert!(valid_key("byok_api_key"));
        assert!(valid_key("a-1"));
        for bad in ["", "UPPER", "a b", "a/b", "../x", "中文"] {
            assert!(!valid_key(bad), "{bad:?} 不该合法");
        }
        assert!(!valid_key(&"a".repeat(65)));
    }

    #[test]
    fn mask_keeps_head_and_tail_only() {
        assert_eq!(
            mask_secret("sk-test-0123456789abcdef0123456789abcdef"),
            "sk-****cdef"
        );
        assert_eq!(mask_secret("short"), "****");
        assert!(!mask_secret("sk-test-0123456789abcdef0123456789abcdef").contains("c12f"));
    }

    #[test]
    fn memory_store_lifecycle() {
        let store = MemoryKeyStore::new("测试降级");
        assert!(!store.status("k").unwrap().set);
        let status = store.set("k", "sk-1234567890abc").unwrap();
        assert!(status.set);
        assert!(status.degraded);
        assert_eq!(status.masked, "sk-****0abc");
        let status = store.status("k").unwrap();
        assert!(status.set);
        assert_eq!(status.masked, "sk-****0abc");
        // 只有内部 `load_raw` 能拿到明文（不经过 FFI）
        assert_eq!(
            store.load_raw("k").unwrap().as_deref(),
            Some("sk-1234567890abc")
        );
        assert!(store.delete("k").unwrap());
        assert!(!store.status("k").unwrap().set);
        assert!(!store.delete("k").unwrap());
    }

    #[test]
    fn invalid_values_are_rejected() {
        let store = MemoryKeyStore::new("x");
        assert!(matches!(
            store.set("k", "").unwrap_err(),
            SecretError::InvalidValue(_)
        ));
        assert!(matches!(
            store.set("k", &"a".repeat(4097)).unwrap_err(),
            SecretError::InvalidValue(_)
        ));
        assert!(matches!(
            store.set("BAD", "v").unwrap_err(),
            SecretError::InvalidKey(_)
        ));
    }

    #[test]
    fn status_json_never_contains_plaintext() {
        let store = MemoryKeyStore::new("x");
        let plain = "sk-test-0123456789abcdef0123456789abcdef";
        let status = store.set("byok", plain).unwrap();
        let json = serde_json::to_string(&serde_json::json!({
            "set": status.set,
            "masked": status.masked,
            "degraded": status.degraded,
        }))
        .unwrap();
        assert!(!json.contains(plain), "返回 JSON 泄漏明文：{json}");
        assert!(!json.contains("0123456789abcdef"), "返回 JSON 含明文片段：{json}");
        assert!(json.contains("sk-****cdef"));
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip_on_windows() {
        let base = temp_base();
        let store = DpapiKeyStore::open(&base);
        let plain = "sk-test-0123456789abcdef0123456789abcdef";
        let status = store.set("byok_api_key", plain).unwrap();
        assert!(
            !status.degraded,
            "DPAPI 应可用（当前用户会话）：{:?}",
            status.reason
        );
        assert_eq!(status.masked, "sk-****cdef");

        // 密文文件存在且不是明文
        let bin = base.join("secrets").join("byok_api_key.bin");
        assert!(bin.exists(), "密文文件应存在");
        let ciphertext = std::fs::read(&bin).unwrap();
        assert!(!ciphertext.is_empty());
        let contains_plain = ciphertext
            .windows(plain.len())
            .any(|window| window == plain.as_bytes());
        assert!(!contains_plain, "密文文件里不得出现明文");

        // 元数据只有掩码
        let meta = std::fs::read_to_string(base.join("secrets").join("byok_api_key.json")).unwrap();
        assert!(meta.contains("sk-****cdef"));
        assert!(!meta.contains(plain), "元数据不得出现明文");

        // 内部可解密（仅 Rust 使用）
        assert_eq!(
            store.load_raw("byok_api_key").unwrap().as_deref(),
            Some(plain)
        );

        // 对外 status 不带明文
        let status = store.status("byok_api_key").unwrap();
        assert!(status.set);
        assert_eq!(status.masked, "sk-****cdef");

        // 删除后为未设置
        assert!(store.delete("byok_api_key").unwrap());
        assert!(!store.status("byok_api_key").unwrap().set);
        let _ = std::fs::remove_dir_all(&base);
    }
}
