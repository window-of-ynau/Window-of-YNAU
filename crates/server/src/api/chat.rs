//! 对话代理。
//!
//! W1 状态：**已接好契约、校验与错误信封**，流式转发留到 W2。
//! 这样做的原因是：客户端（Flutter）可以现在就对着真实契约开发，
//! 不必等后端把 SSE 写完。

use axum::{
    body::Bytes,
    extract::{Extension, State},
    response::Response,
};
use yunong_shared::{ChatRequest, ErrorCode};

use crate::api::error::AppError;
use crate::api::request_id::ReqId;
use crate::state::AppState;

pub async fn chat_stream(
    State(state): State<AppState>,
    Extension(req_id): Extension<ReqId>,
    body: Bytes,
) -> Result<Response, AppError> {
    // 手写解析而不是用 `Json<ChatRequest>`：axum 默认的 422 响应体不是我们的错误信封。
    let request: ChatRequest = serde_json::from_slice(&body).map_err(|e| {
        AppError::new(
            ErrorCode::ValidationError,
            format!("请求体不是合法的 ChatRequest：{e}"),
            &req_id.0,
        )
    })?;

    request
        .validate()
        .map_err(|(code, msg)| AppError::new(code, msg, &req_id.0))?;

    // 服务端再守一道：配置可收紧上限，不信任客户端自报。
    if request.estimated_input_tokens > state.config.max_input_tokens {
        return Err(AppError::new(
            ErrorCode::MessageTooLarge,
            format!(
                "输入约 {} tokens，超过服务端上限 {}",
                request.estimated_input_tokens, state.config.max_input_tokens
            ),
            &req_id.0,
        ));
    }

    tracing::info!(
        request_id = %req_id.0,
        session_id = %request.session_id,
        turn_id = %request.turn_id,
        round = request.round,
        model = %request.model,
        "收到对话请求（流式转将在 W2 实现）"
    );

    // W2：并发闸门 → 配额 → Auto Review → 上游流式转发 → 抽样记账。
    Err(AppError::new(
        ErrorCode::NotImplemented,
        "流式转发尚未实现（W2）。当前接口已具备：契约校验、错误信封、请求 ID、上限保护",
        &req_id.0,
    ))
}
