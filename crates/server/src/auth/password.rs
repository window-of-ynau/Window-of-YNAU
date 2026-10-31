//! 密码哈希：只存 Argon2id 的 PHC 字符串（`$argon2id$v=19$m=...`）。

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use uuid::Uuid;

use crate::auth::error::AuthError;

/// 注册时密码的最小长度（按字符数算，中文密码也公平）。
pub const MIN_PASSWORD_LEN: usize = 8;

/// 上限：防止超长输入拖慢哈希（Argon2 的成本与输入长度有关）。
pub const MAX_PASSWORD_LEN: usize = 128;

/// 哈希密码，返回 PHC 字符串。
///
/// 盐自己生成：`SaltString::generate` 需要 `rand_core/getrandom` feature，
/// 本仓库的离线依赖没有打开它，而任务又不允许加依赖；
/// 于是用一段 UUID v4（122 bit 随机，PHC 明确认可 UUID 作盐）的 16 字节
/// 交给 `SaltString::encode_b64`，效果等价且零新增依赖。
pub fn hash(password: &str) -> Result<String, AuthError> {
    let salt = random_salt();
    let hashed = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AuthError::internal(format!("密码哈希失败：{e}")))?;
    Ok(hashed.to_string())
}

/// 校验密码。任何解析或计算失败都只意味着"不匹配"，不区分原因。
pub fn verify(password: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// 注册时的密码长度校验（登录不做，登录失败一律 401，避免泄露策略细节）。
pub fn validate_new(password: &str) -> Result<(), AuthError> {
    let len = password.chars().count();
    if len < MIN_PASSWORD_LEN {
        return Err(AuthError::validation(format!(
            "密码至少需要 {MIN_PASSWORD_LEN} 个字符"
        )));
    }
    if len > MAX_PASSWORD_LEN {
        return Err(AuthError::validation(format!(
            "密码不能超过 {MAX_PASSWORD_LEN} 个字符"
        )));
    }
    Ok(())
}

/// 16 字节随机盐 → PHC 的 B64 盐串（22 个字符）。
fn random_salt() -> SaltString {
    let uuid = Uuid::new_v4();
    // 16 字节编码成 B64 不可能失败；真失败也只可能是我把长度写错。
    SaltString::encode_b64(uuid.as_bytes()).expect("16 字节编码为盐必然成功")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_phc_argon2id_and_verifies() {
        let phc = hash("correct-horse-battery").expect("哈希应成功");
        assert!(
            phc.starts_with("$argon2id$"),
            "必须是 Argon2id 的 PHC 串：{phc}"
        );
        assert!(verify("correct-horse-battery", &phc));
        assert!(!verify("wrong-password", &phc));
        assert!(!verify("correct-horse-battery", "不是 PHC"));
    }

    #[test]
    fn same_password_gets_different_salted_hashes() {
        let a = hash("correct-horse-battery").expect("哈希应成功");
        let b = hash("correct-horse-battery").expect("哈希应成功");
        assert_ne!(a, b, "每次都要用新盐");
    }

    #[test]
    fn password_length_rules() {
        assert!(validate_new("1234567").is_err(), "7 个字符必须拒绝");
        assert!(validate_new("12345678").is_ok(), "8 个字符必须允许");
        assert!(validate_new(&"x".repeat(MAX_PASSWORD_LEN + 1)).is_err());
    }
}
