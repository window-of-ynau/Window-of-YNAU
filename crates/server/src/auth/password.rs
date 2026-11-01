//! 密码哈希：只存 Argon2id 的 PHC 字符串（`$argon2id$v=19$m=...`）。

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use uuid::Uuid;

use crate::auth::error::AuthError;

/// 注册时密码的最小长度（按字符数算，中文密码也公平）。
pub const MIN_PASSWORD_LEN: usize = 8;

/// 上限：防止超长输入拖慢哈希（Argon2 的成本与输入长度有关）。
pub const MAX_PASSWORD_LEN: usize = 128;

/// 哈希内存成本（KiB）——**显式钉死**，不用 `Argon2::default()`。
///
/// 红队 H1：`Argon2::default()` 的 m 值跟随 argon2 crate 的默认值，升级依赖时可能被
/// 静默改大/改小；而内存成本直接决定"匿名并发能打出多少内存峰值"，不能让它漂移。
/// 这里写死 19 MiB（crate 当前默认值），由 `params_match_pinned_budget` 单测守着。
pub const ARGON2_M_COST_KIB: u32 = 19 * 1024;
/// 时间成本（迭代次数）
pub const ARGON2_T_COST: u32 = 2;
/// 并行度
pub const ARGON2_P_COST: u32 = 1;

/// 构造固定参数的 Argon2id（Argon2id + v19 + 上面的成本参数）。
fn argon2() -> Argon2<'static> {
    let params = Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, None)
        .expect("Argon2 参数固定在合法范围内");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// 哈希密码，返回 PHC 字符串。
///
/// 盐自己生成：`SaltString::generate` 需要 `rand_core/getrandom` feature，
/// 本仓库的离线依赖没有打开它，而任务又不允许加依赖；
/// 于是用一段 UUID v4（122 bit 随机，PHC 明确认可 UUID 作盐）的 16 字节
/// 交给 `SaltString::encode_b64`，效果等价且零新增依赖。
pub fn hash(password: &str) -> Result<String, AuthError> {
    let salt = random_salt();
    let hashed = argon2()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AuthError::internal(format!("密码哈希失败：{e}")))?;
    Ok(hashed.to_string())
}

/// 校验密码。任何解析或计算失败都只意味着"不匹配"，不区分原因。
pub fn verify(password: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    argon2()
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

    /// 红队 H1：哈希成本必须**钉死在代码里**，不能被 argon2 crate 的默认值变更带走。
    ///
    /// 这个断言把"内存放大器"的参数写进测试：将来升级依赖导致默认值变化时这里会红，
    /// 逼着人显式决策要不要接受新的内存成本。
    #[test]
    fn params_match_pinned_budget() {
        let params = Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, None)
            .expect("固定参数必须合法");
        assert_eq!(params.m_cost(), ARGON2_M_COST_KIB, "内存成本被改动");
        assert_eq!(params.t_cost(), ARGON2_T_COST, "迭代次数被改动");
        assert_eq!(params.p_cost(), ARGON2_P_COST, "并行度被改动");
        assert_eq!(ARGON2_M_COST_KIB, 19_456, "19 MiB 是当前基线，改动要过评审");
        // PHC 串里必须能读出同样的成本：证明 hash() 真的用了这套参数
        let phc = hash("correct-horse-battery").expect("哈希成功");
        assert!(phc.contains("m=19456,t=2,p=1"), "PHC 参数不符：{phc}");
    }

    #[test]
    fn password_length_rules() {
        assert!(validate_new("1234567").is_err(), "7 个字符必须拒绝");
        assert!(validate_new("12345678").is_ok(), "8 个字符必须允许");
        assert!(validate_new(&"x".repeat(MAX_PASSWORD_LEN + 1)).is_err());
    }
}
