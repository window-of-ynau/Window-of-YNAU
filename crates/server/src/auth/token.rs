//! 不透明令牌：生成、摘要与格式校验。
//!
//! 为什么不用 JWT（ADR-11）：JWT 要"登出立刻失效"就得额外维护黑名单，
//! 而我们的令牌本来就要落库——直接存摘要，吊销就是一行 UPDATE。

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 令牌的十六进制长度：两段 `Uuid::new_v4().simple()`（32 + 32）。
pub const TOKEN_HEX_LEN: usize = 64;

/// 生成一个令牌：64 个小写 hex 字符（两段 UUID v4，共 244 bit 随机）。
pub fn generate() -> String {
    let mut token = uuid_hex();
    token.push_str(&uuid_hex());
    token
}

/// 32 个小写 hex 字符（`Uuid::simple()` 的小写编码）。
fn uuid_hex() -> String {
    let uuid = Uuid::new_v4();
    let mut buffer = Uuid::encode_buffer();
    uuid.simple().encode_lower(&mut buffer).to_owned()
}

/// 数据库只存这个：令牌的 SHA-256（hex，64 字符）。
///
/// 库被读走也拿不到能用的令牌；摘要本身也不可逆。
pub fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// 格式校验：必须是 [`TOKEN_HEX_LEN`] 个小写 hex 字符。
///
/// 格式错直接 401，既省一次查库，也避免拿超长字符串去打哈希。
pub fn is_well_formed(token: &str) -> bool {
    token.len() == TOKEN_HEX_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_64_lowercase_hex_and_unique() {
        let a = generate();
        let b = generate();
        assert_eq!(a.len(), TOKEN_HEX_LEN);
        assert!(is_well_formed(&a), "生成的令牌必须自洽：{a}");
        assert_ne!(a, b);
    }

    #[test]
    fn hash_is_sha256_hex_and_not_reversible() {
        let token = generate();
        let digest = hash(&token);
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(digest, token, "库里不能出现明文令牌");
        assert_eq!(digest, hash(&token), "同一令牌的摘要必须稳定");
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        let too_long = "g".repeat(64);
        let uppercase = "A".repeat(64);
        let too_short = "a".repeat(63);
        for bad in [
            "",
            "abc",
            too_long.as_str(),
            uppercase.as_str(),
            too_short.as_str(),
        ] {
            assert!(!is_well_formed(bad), "不应通过：{bad:?}");
        }
        assert!(is_well_formed(&"0123456789abcdef".repeat(4)));
    }
}
