//! 联网搜索抽象（ADR-19 决策 5）。
//!
//! # 为什么要有这一层
//!
//! 三条通道的搜索能力并不相同：
//! - **套餐 + Responses 协议**：上游模型自带联网搜索，我们**不自己发请求**，只声明"可用"；
//! - **套餐 + 其它协议**：走我方服务端代理搜索（端点由 lead-server 提供，本模块只留接口）；
//! - **BYOK**：用户自带搜索 Key（存 DPAPI，复用 `yunong_set_secret`）；
//! - **都没有**：`SearchSource::None` —— UI 必须**显式**告知"未配置联网搜索"，
//!   **禁止静默降级**（"看起来搜了其实没搜"比报错更糟）。
//!
//! 只走 HTTP JSON API，不引任何搜索 SDK（ADR-19「明确不做」第 4 条）。

use crate::error::{CoreError, CoreResult};

/// 一条搜索结果（不含正文抽取/RAG，那是后续任务）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: String,
}

/// 搜索提供方：HTTP JSON API。
pub trait SearchProvider {
    fn search(&self, query: &str, limit: usize) -> CoreResult<Vec<SearchHit>>;
}

/// 当前会话实际使用的搜索来源（UI 显示这个，**不许撒谎**）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSource {
    /// 套餐 + Responses：上游内置搜索
    BuiltIn,
    /// 套餐 + 其它协议：我方服务端代理
    Proxy,
    /// BYOK：用户自带 Key
    Byok,
    /// 未配置：**必须明说**
    None,
}

impl SearchSource {
    /// 中文标签（与 UI 文案一致，单测锁定）。
    pub fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "内置",
            Self::Proxy => "我方代理",
            Self::Byok => "自带 Key",
            Self::None => "未配置",
        }
    }

    pub fn available(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// 给定的通道/协议/凭据状态下，搜索来源是什么（**纯函数，四通道优先级**）。
///
/// - `channel`：`cloud` / `byok`
/// - `protocol`：`openai_responses` / 其它
/// - `has_byok_key`：用户是否已保存自带搜索 Key
pub fn resolve_source(channel: &str, protocol: &str, has_byok_key: bool) -> SearchSource {
    match channel {
        "cloud" => {
            if protocol == "openai_responses" {
                SearchSource::BuiltIn
            } else {
                SearchSource::Proxy
            }
        }
        "byok" => {
            if has_byok_key {
                SearchSource::Byok
            } else {
                SearchSource::None
            }
        }
        _ => SearchSource::None,
    }
}

/// 给 UI 的搜索状态（`available:false` 时 UI 必须常驻提示）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchStatus {
    pub source: SearchSource,
    /// 中文标签：内置 / 我方代理 / 自带 Key / 未配置
    pub label: String,
    pub available: bool,
    /// 未配置时的诚实提示（available 时为空串）
    pub hint: String,
}

impl SearchStatus {
    pub fn for_source(source: SearchSource) -> Self {
        Self {
            source,
            label: source.label().to_owned(),
            available: source.available(),
            hint: if source.available() {
                String::new()
            } else {
                "未配置联网搜索，模型无法获取实时信息".to_owned()
            },
        }
    }
}

/// 我方代理搜索的端点（lead-server 提供后填这里；空 = 端点未就绪）。
pub const PROXY_SEARCH_PATH: &str = "/api/v1/search";

/// 代理搜索：HTTP JSON API，**不引 SDK**。
///
/// 端点未就绪时 `search()` 返回 `NOT_CONFIGURED`，绝不假装成功。
pub struct ProxySearchProvider {
    base_url: String,
    /// 端点接线后用于 `Authorization` 头；当前仅保存，不参与请求（故允许暂时未读）
    #[allow(dead_code)]
    access_token: String,
}

impl ProxySearchProvider {
    pub fn new(base_url: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            access_token: access_token.into(),
        }
    }

    /// 端点就绪与否（空 base_url = 未就绪）。
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty()
    }

    /// 拼请求地址（纯函数，便于单测；不真的发请求）。
    pub fn endpoint(&self, query: &str, limit: usize) -> String {
        format!(
            "{}{PROXY_SEARCH_PATH}?q={}&limit={limit}",
            self.base_url.trim_end_matches('/'),
            url_encode(query)
        )
    }
}

impl SearchProvider for ProxySearchProvider {
    fn search(&self, query: &str, limit: usize) -> CoreResult<Vec<SearchHit>> {
        if !self.is_configured() {
            return Err(CoreError::Remote {
                code: yunong_shared::ErrorCode::NotFound,
                message: "搜索端点未就绪（lead-server 未提供 /api/v1/search）".to_owned(),
                retry_after: None,
            });
        }
        // 端点由 lead-server 提供后在这里接 HTTP 调用；当前只做接口占位。
        let _ = self.endpoint(query, limit);
        Err(CoreError::Remote {
            code: yunong_shared::ErrorCode::Internal,
            message: "代理搜索尚未接线（端点已预留）".to_owned(),
            retry_after: None,
        })
    }
}

/// 最小 URL 编码（只处理查询串里必须转义的字符；不引额外依赖）。
pub fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_with_responses_uses_built_in_search() {
        let source = resolve_source("cloud", "openai_responses", false);
        assert_eq!(source, SearchSource::BuiltIn);
        assert_eq!(source.label(), "内置");
        assert!(SearchStatus::for_source(source).available);
    }

    #[test]
    fn cloud_with_other_protocol_uses_our_proxy() {
        for protocol in ["openai_completions", "anthropic_messages"] {
            let source = resolve_source("cloud", protocol, false);
            assert_eq!(source, SearchSource::Proxy, "{protocol}");
            assert_eq!(source.label(), "我方代理");
        }
    }

    #[test]
    fn byok_depends_on_user_key() {
        assert_eq!(
            resolve_source("byok", "openai_completions", true),
            SearchSource::Byok
        );
        assert_eq!(
            resolve_source("byok", "openai_completions", true).label(),
            "自带 Key"
        );
        // 没填 Key → 未配置（不是"我们偷偷不搜"）
        let none = resolve_source("byok", "openai_completions", false);
        assert_eq!(none, SearchSource::None);
        assert_eq!(none.label(), "未配置");
    }

    #[test]
    fn nothing_configured_is_honest() {
        let status = SearchStatus::for_source(resolve_source("byok", "openai_responses", false));
        assert!(!status.available);
        assert_eq!(status.label, "未配置");
        assert!(status.hint.contains("未配置联网搜索"), "{}", status.hint);
        // 可用时不给提示（不打扰用户）
        assert!(SearchStatus::for_source(SearchSource::BuiltIn)
            .hint
            .is_empty());
    }

    #[test]
    fn proxy_endpoint_is_built_without_network() {
        let provider = ProxySearchProvider::new("https://api.example.com/", "token");
        assert!(provider.is_configured());
        assert_eq!(
            provider.endpoint("云南 天气", 5),
            "https://api.example.com/api/v1/search?q=%E4%BA%91%E5%8D%97%20%E5%A4%A9%E6%B0%94&limit=5"
        );
        let empty = ProxySearchProvider::new("", "");
        assert!(!empty.is_configured());
        // 未就绪时明确报错，绝不假装搜到
        assert!(empty.search("x", 3).is_err());
    }

    #[test]
    fn unknown_channel_is_none() {
        assert_eq!(
            resolve_source("magic", "openai_responses", true),
            SearchSource::None
        );
    }
}
