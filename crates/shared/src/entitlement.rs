//! 授权（entitlement）：按时间授权，不按 token 记账（ADR-03）。
//!
//! 云端只校验"这条授权是否还在有效期内"，不维护余额。

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// 两条通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// 用户自带 Key；请求端侧直连厂商，不经过我们
    Byok,
    /// 套餐；经云端代理转发
    Plan,
}

impl Channel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Byok => "byok",
            Self::Plan => "plan",
        }
    }
}

/// 一次请求可用的授权快照（由服务端在中间件里解析出）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlement {
    pub channel: Channel,
    /// 套餐代号（BYOK 为空）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// 到期时间；BYOK 为空（永不过期，因为不花我们的钱）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<OffsetDateTime>,
    /// 单用户同时只允许 1 个在途 turn
    pub concurrent_limit: u8,
    pub max_input_tokens: u32,
    /// 知识检索每分钟次数（BYOK 也有，用来保护我们的 RAG 服务）
    pub knowledge_rpm: u32,
    /// 近 7 天抽样成本（分），仅用于自我感知与熔断
    #[serde(default)]
    pub sampled_cost_cents_7d: u64,
}

impl Entitlement {
    /// BYOK：不花钱，但知识检索仍限流。
    pub fn byok() -> Self {
        Self {
            channel: Channel::Byok,
            plan: None,
            expires_at: None,
            concurrent_limit: 1,
            max_input_tokens: crate::MAX_INPUT_TOKENS,
            knowledge_rpm: 30,
            sampled_cost_cents_7d: 0,
        }
    }

    /// 套餐：必须有到期时间。
    pub fn plan(name: impl Into<String>, expires_at: OffsetDateTime) -> Self {
        Self {
            channel: Channel::Plan,
            plan: Some(name.into()),
            expires_at: Some(expires_at),
            concurrent_limit: 1,
            max_input_tokens: crate::MAX_INPUT_TOKENS,
            knowledge_rpm: 60,
            sampled_cost_cents_7d: 0,
        }
    }

    /// 是否仍可用。BYOK 恒真；套餐看 `expires_at`。
    pub fn is_active(&self, now: OffsetDateTime) -> bool {
        match self.channel {
            Channel::Byok => true,
            Channel::Plan => self.expires_at.is_some_and(|exp| exp > now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    #[test]
    fn byok_never_expires() {
        let e = Entitlement::byok();
        assert!(e.is_active(OffsetDateTime::now_utc() + Duration::days(3650)));
        assert_eq!(e.channel, Channel::Byok);
    }

    #[test]
    fn plan_expires_at_boundary() {
        let now = OffsetDateTime::now_utc();
        let e = Entitlement::plan("plan_30d", now + Duration::days(30));
        assert!(e.is_active(now));
        // 正好到期的那一刻视为过期（用 > 比较）
        assert!(!e.is_active(now + Duration::days(30)));
    }

    #[test]
    fn channel_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Channel::Plan).unwrap(), "\"plan\"");
    }
}
