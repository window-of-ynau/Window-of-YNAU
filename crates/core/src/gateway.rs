//! 模型网关：两条通道一个抽象（ADR-03 / ADR-04）。
//!
//! ```text
//!                   ┌─ DirectProvider  ──▶ 厂商（BYOK，Key 不出端）
//! ModelGateway ─────┤
//!                   └─ CloudProxy      ──▶ 我们的服务端（套餐，服务端代理）
//! ```
//!
//! 本文件只定义 trait 与 BYOK 的骨架；真正的 HTTP 适配（三协议）在 W2。

use yunong_shared::{ChatRequest, Chunk, Protocol};

use crate::error::{CoreError, CoreResult};

/// 上层只依赖这个 trait，不关心走哪条通道。
pub trait ModelGateway {
    /// 同步版本：W1 先不引入 async 复杂度。
    /// W2 换成 `async fn chat_stream` 时，只需在这里加一个方法，不破坏调用方结构。
    fn chat(&self, request: &ChatRequest) -> CoreResult<Vec<Chunk>>;

    /// 这条通道是否经过我们的服务器。
    fn goes_through_cloud(&self) -> bool;
}

/// BYOK：端侧直连厂商（ADR-04）。
#[derive(Debug, Clone)]
pub struct DirectProvider {
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
}

impl DirectProvider {
    pub fn new(protocol: Protocol, base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            protocol,
            base_url: base_url.into(),
            model: model.into(),
        }
    }
}

impl ModelGateway for DirectProvider {
    fn chat(&self, _request: &ChatRequest) -> CoreResult<Vec<Chunk>> {
        // W2：按 protocol 分派到三个适配器，用 reqwest 发流式请求。
        Err(CoreError::NotImplemented(format!(
            "BYOK 直连 {:?}（{}/{}）将在 W2 实现",
            self.protocol, self.base_url, self.model
        )))
    }

    fn goes_through_cloud(&self) -> bool {
        false
    }
}

/// 套餐：经我们服务端转发（ADR-03）。端侧只持时效令牌，不持上游 Key。
#[derive(Debug, Clone)]
pub struct CloudProxy {
    pub base_url: String,
    pub access_token: String,
}

impl CloudProxy {
    pub fn new(base_url: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            access_token: access_token.into(),
        }
    }
}

impl ModelGateway for CloudProxy {
    fn chat(&self, _request: &ChatRequest) -> CoreResult<Vec<Chunk>> {
        Err(CoreError::NotImplemented(format!(
            "套餐通道（{}）将在 W2 实现",
            self.base_url
        )))
    }

    fn goes_through_cloud(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yunong_shared::{ChatMessage, ChatRequest};

    fn req() -> ChatRequest {
        ChatRequest {
            session_id: "s".into(),
            turn_id: "t".into(),
            round: 1,
            model: "deepseek-chat".into(),
            protocol: Protocol::OpenaiCompletions,
            messages: vec![ChatMessage::user("hi")],
            tools: vec![],
            max_output_tokens: 128,
            stream: true,
            estimated_input_tokens: 8,
        }
    }

    #[test]
    fn byok_does_not_touch_our_server() {
        let p = DirectProvider::new(
            Protocol::OpenaiCompletions,
            "https://api.deepseek.com",
            "deepseek-chat",
        );
        assert!(!p.goes_through_cloud());
        assert!(matches!(p.chat(&req()), Err(CoreError::NotImplemented(_))));
    }

    #[test]
    fn plan_channel_goes_through_cloud() {
        let p = CloudProxy::new("https://api.example.com", "tok");
        assert!(p.goes_through_cloud());
        assert!(matches!(p.chat(&req()), Err(CoreError::NotImplemented(_))));
    }
}
