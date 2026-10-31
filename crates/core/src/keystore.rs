//! 凭据存储：**全系统唯一**接触 Cookie / API Key 的地方（ADR-04 / ADR-11）。
//!
//! 真实实现（W2）：Android 走 Keystore（JNI），Windows 走 DPAPI / Credential Manager。
//! 本文件先给出 trait 与内存实现，让上层逻辑现在就能被测试。

use std::collections::HashMap;

use crate::error::{CoreError, CoreResult};

/// 哪一类凭据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// 用户的模型 API Key（BYOK），**绝不上云**
    ApiKey,
    /// 学校会话 Cookie，**绝不上云**（ADR-11）
    SchoolCookie,
}

impl SecretKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::SchoolCookie => "school_cookie",
        }
    }

    pub const fn all() -> [Self; 2] {
        [Self::ApiKey, Self::SchoolCookie]
    }
}

/// 安全存储。`get` 只在核心内部调用（发请求前取一次，用完即弃）。
pub trait SecretStore {
    fn set(&mut self, kind: SecretKind, value: &str) -> CoreResult<()>;
    fn has(&self, kind: SecretKind) -> bool;
    fn get(&self, kind: SecretKind) -> CoreResult<String>;
    fn delete(&mut self, kind: SecretKind) -> CoreResult<()>;
}

/// 内存实现：只用于测试与桌面开发，**不可用于生产**。
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    entries: HashMap<SecretKind, String>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn set(&mut self, kind: SecretKind, value: &str) -> CoreResult<()> {
        if value.trim().is_empty() {
            return Err(CoreError::Invalid(format!("{} 不能为空", kind.as_str())));
        }
        self.entries.insert(kind, value.to_owned());
        Ok(())
    }

    fn has(&self, kind: SecretKind) -> bool {
        self.entries.contains_key(&kind)
    }

    fn get(&self, kind: SecretKind) -> CoreResult<String> {
        self.entries
            .get(&kind)
            .cloned()
            .ok_or(CoreError::SecretMissing(kind.as_str()))
    }

    fn delete(&mut self, kind: SecretKind) -> CoreResult<()> {
        self.entries.remove(&kind);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_has_get_delete() {
        let mut s = InMemorySecretStore::new();
        assert!(!s.has(SecretKind::ApiKey));
        s.set(SecretKind::ApiKey, "sk-test").unwrap();
        assert!(s.has(SecretKind::ApiKey));
        assert_eq!(s.get(SecretKind::ApiKey).unwrap(), "sk-test");
        s.delete(SecretKind::ApiKey).unwrap();
        assert!(!s.has(SecretKind::ApiKey));
        assert!(matches!(
            s.get(SecretKind::ApiKey),
            Err(CoreError::SecretMissing(_))
        ));
    }

    #[test]
    fn empty_secret_is_rejected() {
        let mut s = InMemorySecretStore::new();
        assert!(matches!(
            s.set(SecretKind::SchoolCookie, "   "),
            Err(CoreError::Invalid(_))
        ));
        assert!(!s.has(SecretKind::SchoolCookie));
    }

    #[test]
    fn kinds_are_independent() {
        let mut s = InMemorySecretStore::new();
        s.set(SecretKind::ApiKey, "k").unwrap();
        s.set(SecretKind::SchoolCookie, "c").unwrap();
        assert_eq!(s.get(SecretKind::ApiKey).unwrap(), "k");
        assert_eq!(s.get(SecretKind::SchoolCookie).unwrap(), "c");
    }
}
