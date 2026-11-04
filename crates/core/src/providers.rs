//! 厂商能力表：**唯一**的"某家支持什么"的事实来源（t126）。
//!
//! # 为什么要这张表
//!
//! 之前有两类错误都出在"凭印象写厂商能力"：
//!
//! 1. 我们一度把 DeepSeek 标成"只支持 completions"，其实是三种 API 都支持（t118 已撤销）；
//! 2. 默认模型名用过 `deepseek-flash` / `deepseek-flash`，而这两个名字**不在官方文档里**。
//!
//! 所以这里把结论、**来源**与**查阅日期**写在一起；改这张表必须同时更新来源注释。
//!
//! # 来源（owner 提供的官方文档原文，captain 已逐条核对）
//!
//! - DeepSeek 文档「模型 & 价格」页：<https://api-docs.deepseek.com/quick_start/pricing>
//!   （模型列表与脚注；查阅日期 **2026-11-04**，由 owner 提供、captain 核对原文）
//! - DeepSeek「Responses API」页：同一文档站的 Responses 章节（工具调用事件、
//!   `tool_choice` 支持 `none/auto/required`；查阅日期 **2026-11-04**）
//! - DeepSeek「Anthropic 格式」页：`base_url = https://api.deepseek.com/anthropic`，
//!   `tools` 用 `name` + `input_schema`（不要传 `type`），`tool_choice` 的 `any`
//!   **按 auto 处理**（即不强制）；查阅日期 **2026-11-04**
//!
//! 说明：本仓库的构建环境**没有**外网访问（`api.deepseek.com` 探不通），
//! 所以以上事实来自 owner 转述的文档原文，不是我们实测的结果；
//! 端到端验证由 lead-client 用真实 Key 完成。**不要**在注释里加"我们实测过"这类说法。

/// 一家厂商（或兼容网关）的能力描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    /// 稳定标识（日志/诊断用）
    pub id: &'static str,
    /// OpenAI 兼容格式的 base url（`…/chat/completions`、`…/responses` 挂在它下面）
    pub openai_base: &'static str,
    /// Anthropic 兼容格式的 base url（`…/v1/messages` 挂在它下面）；没有就是 `None`
    pub anthropic_base: Option<&'static str>,
    /// 该家**当前文档中**的模型名（按文档顺序，第一个是默认）
    pub models: &'static [&'static str],
    /// 是否支持 OpenAI **Responses** API（`/responses` + `response.*` 流式事件）
    pub supports_responses: bool,
    /// 是否支持 **Anthropic Messages** 兼容层（`/anthropic/v1/messages`）
    pub supports_anthropic: bool,
    /// 是否支持**图像理解**（多模态输入）
    pub supports_vision: bool,
    /// Anthropic 兼容层里 `tool_choice={"type":"any"}` 是否**真的**强制调用工具。
    ///
    /// DeepSeek 的文档明确写"按 auto 处理"→ `false`；因此 messages 路径下
    /// 用户要 `required` 时，我们只能如实报 `tool_choice_effective="auto"`（t124/t126）。
    pub anthropic_any_forces_tools: bool,
    /// 上游是否**默认开启思考模式**（我们不发任何 `thinking` 参数，跟随默认）
    pub thinking_by_default: bool,
}

/// DeepSeek（来源见模块文档；查阅日期 2026-11-04）。
pub const DEEPSEEK: Provider = Provider {
    id: "deepseek",
    openai_base: "https://api.deepseek.com",
    anthropic_base: Some("https://api.deepseek.com/anthropic"),
    // 文档只有两个模型；脚注写明"模型名请使用 deepseek-flash"
    models: &["deepseek-flash", "deepseek-v4-pro"],
    supports_responses: true,
    supports_anthropic: true,
    // 图像理解仅 flash 支持（v4-pro 不支持）
    supports_vision: true,
    anthropic_any_forces_tools: false,
    thinking_by_default: true,
};

/// Anthropic 官方（原生 Messages API；不做 Responses/Completions 兼容）。
pub const ANTHROPIC: Provider = Provider {
    id: "anthropic",
    openai_base: "https://api.anthropic.com",
    anthropic_base: Some("https://api.anthropic.com"),
    models: &[],
    supports_responses: false,
    supports_anthropic: true,
    supports_vision: false,
    anthropic_any_forces_tools: true,
    thinking_by_default: false,
};

/// OpenAI 官方（Responses + Completions；messages 走兼容层时用不到这里）。
pub const OPENAI: Provider = Provider {
    id: "openai",
    openai_base: "https://api.openai.com",
    anthropic_base: None,
    models: &[],
    supports_responses: true,
    supports_anthropic: false,
    supports_vision: false,
    anthropic_any_forces_tools: true,
    thinking_by_default: false,
};

/// 表里所有已知厂商（顺序无意义）。
pub const KNOWN: &[&Provider] = &[&DEEPSEEK, &ANTHROPIC, &OPENAI];

/// 按 host 找厂商；未知 host（自建/中转）返回 `None`——**不猜**能力。
pub fn lookup(host: &str) -> Option<&'static Provider> {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    KNOWN
        .iter()
        .copied()
        .find(|provider| host_matches(&host, provider))
}

fn host_matches(host: &str, provider: &Provider) -> bool {
    let domain = provider
        .openai_base
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// 该 host 的**默认模型名**（文档里排第一的那个）；未知 host 返回 `None`。
///
/// 调用方不要自己写死 `deepseek-flash` 这类名字——那不在文档里（t126）。
pub fn default_model(host: &str) -> Option<&'static str> {
    lookup(host)?.models.first().copied()
}

/// Anthropic 兼容层的 `{"type":"any"}` 在这家是否真的强制调工具。
///
/// 未知 host 返回 `true`（按标准行为），由调用方在 meta 里如实标注即可。
pub fn anthropic_any_forces_tools(host: &str) -> bool {
    lookup(host).is_none_or(|provider| provider.anthropic_any_forces_tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_matches_the_documented_facts() {
        let provider = lookup("api.deepseek.com").expect("deepseek 应该在表里");
        assert_eq!(provider.id, "deepseek");
        assert_eq!(provider.openai_base, "https://api.deepseek.com");
        assert_eq!(
            provider.anthropic_base,
            Some("https://api.deepseek.com/anthropic")
        );
        // 文档只有两个模型，默认 flash
        assert_eq!(provider.models, &["deepseek-flash", "deepseek-v4-pro"]);
        assert_eq!(default_model("api.deepseek.com"), Some("deepseek-flash"));
        // 三种 API 都支持；图像仅 flash；any 不强制；默认开思考
        assert!(provider.supports_responses);
        assert!(provider.supports_anthropic);
        assert!(provider.supports_vision);
        assert!(!provider.anthropic_any_forces_tools);
        assert!(provider.thinking_by_default);
    }

    /// 文档里**不存在**的名字不能被当成默认值写回来（t126 的回归点）。
    ///
    /// 用 `concat!` 拼出旧名，避免被"全局清理旧模型名"的脚本顺手改掉。
    #[test]
    fn legacy_model_names_are_not_documented() {
        let documented = DEEPSEEK.models;
        for wrong in [
            concat!("deepseek", "-chat"),
            concat!("deepseek", "-reasoner"),
        ] {
            assert!(
                !documented.contains(&wrong),
                "{wrong} 不在官方文档的模型列表里，不能用作默认值"
            );
        }
    }

    #[test]
    fn unknown_host_is_not_guessed() {
        assert!(lookup("10.0.0.5").is_none());
        assert_eq!(default_model("10.0.0.5"), None);
        // 未知 host 的 any 行为按标准来（true），不替它下结论
        assert!(anthropic_any_forces_tools("10.0.0.5"));
        assert!(!anthropic_any_forces_tools("api.deepseek.com"));
        assert!(anthropic_any_forces_tools("api.anthropic.com"));
    }
}
