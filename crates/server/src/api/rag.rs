//! RAG 2.0 的 HTTP 接口（ADR-19 决策 2/3）。
//!
//! 两个端点都要求 Bearer（客户端是我们的 App），但**上报体里不含 user_id、
//! 服务端也不把它写进库里**：这里只把 `Principal` 用于审计（谁在什么时候报了 N 条），
//! 不用来关联哈希。

use axum::{
    body::Bytes,
    extract::{Extension, State},
    response::Response,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use yunong_shared::ErrorCode;

use crate::accounting::{AuditEntry, RagLimit};
use crate::api::{client_ip::ClientIp, envelope, error::AppError, request_id::ReqId};
use crate::auth::Principal;
use crate::rag::{parse_hash, RagError, ReferralItem};
use crate::state::AppState;

/// 单条 `count` 的上限（R5-F1：从 10_000 收到 100）。
///
/// 真实用户一个月引用同一篇文档超过 100 次几乎不可能；配合"月配额按 count 总和算"
/// 与库侧 `LEAST(..., 100_000)`，一次请求再也无法把某个 hash 顶到霸榜位置。
const MAX_COUNT_PER_ITEM: u32 = 100;

#[derive(Debug, Serialize)]
struct SaltBody {
    month: String,
    salt: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
struct ReferralsRequest {
    /// `YYYY-MM`。只接受当前月与上月（客户端跨月宽限）
    month: String,
    items: Vec<ItemBody>,
}

#[derive(Debug, Deserialize)]
struct ItemBody {
    hash: String,
    count: u32,
}

/// `GET /api/v1/rag/salt`（需 Bearer）→ `{month, salt, expires_at}`
pub async fn salt(
    State(state): State<AppState>,
    Extension(req_id): Extension<ReqId>,
) -> Result<Response, AppError> {
    let view = state
        .rag
        .salt()
        .await
        .map_err(|error| into_app_error(error, &req_id.0))?;
    Ok(envelope::ok(
        SaltBody {
            month: view.month,
            salt: view.salt,
            expires_at: view.expires_at,
        },
        &req_id.0,
    ))
}

/// `POST /api/v1/rag/referrals`（需 Bearer）→ `{accepted:N}`
pub async fn referrals(
    State(state): State<AppState>,
    Extension(req_id): Extension<ReqId>,
    Extension(principal): Extension<Principal>,
    Extension(client): Extension<ClientIp>,
    body: Bytes,
) -> Result<Response, AppError> {
    let request: ReferralsRequest = serde_json::from_slice(&body).map_err(|error| {
        AppError::new(
            ErrorCode::ValidationError,
            format!("请求体不是合法的上报：{error}"),
            &req_id.0,
        )
    })?;

    // 月份：只允许当前月/上月（防止往未来月份无限写）
    state
        .rag
        .check_month(&request.month)
        .map_err(|error| into_app_error(error, &req_id.0))?;

    if request.items.is_empty() {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            "items 不能为空",
            &req_id.0,
        ));
    }

    // 先做便宜的逐条校验（哈希格式、count 范围），再占配额——
    // 否则一个畸形请求也会白扣额度。
    let mut items = Vec::with_capacity(request.items.len());
    for item in &request.items {
        if item.count == 0 || item.count > MAX_COUNT_PER_ITEM {
            return Err(AppError::new(
                ErrorCode::ValidationError,
                format!("count 必须在 1..={MAX_COUNT_PER_ITEM} 之间"),
                &req_id.0,
            ));
        }
        let hash = parse_hash(&item.hash).map_err(|error| into_app_error(error, &req_id.0))?;
        items.push(ReferralItem {
            hash,
            count: item.count,
        });
    }
    // 单次条数上限（与 count 总和上限是两件事）
    if items.len() > state.rag.max_items_per_request() {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            format!("单次最多 {} 条", state.rag.max_items_per_request()),
            &req_id.0,
        ));
    }

    // R5-F1：配额按 **count 总和** 算，不按条数
    let total_count: u32 = items.iter().map(|item| item.count).sum();

    state
        .rag
        .check_quota(&request.month, &client.0, total_count)
        .map_err(|error| {
            // 配额拒绝也要留审计（否则无法解释"为什么这条客户端被限"）
            if let RagError::MonthlyQuota { limit } = error {
                state.accounting.record_audit_detached(AuditEntry::rag_deny(
                    principal.user_id,
                    &req_id.0,
                    request.month.clone(),
                    RagLimit::Monthly {
                        limit: limit as usize,
                    },
                ));
            }
            into_app_error(error, &req_id.0)
        })?;

    let accepted = state
        .rag
        .record(&request.month, &items)
        .await
        .map_err(|error| {
            state.accounting.record_audit_detached(AuditEntry::rag_deny(
                principal.user_id,
                &req_id.0,
                request.month.clone(),
                RagLimit::Failed,
            ));
            into_app_error(error, &req_id.0)
        })?;

    // R5-F8：**先落库成功、再记配额**（`record` 内部是事务，失败即回滚且不扣额度）
    state
        .rag
        .commit_quota(&request.month, &client.0, total_count);

    // 审计**只记条数与月份**：不记 hash（否则审计日志就成了浏览记录）
    state
        .accounting
        .record_audit_detached(AuditEntry::rag_report(
            principal.user_id,
            &req_id.0,
            request.month.clone(),
            accepted,
        ));

    Ok(envelope::ok(json!({ "accepted": accepted }), &req_id.0))
}

/// `RagError` → HTTP 信封。
fn into_app_error(error: RagError, request_id: &str) -> AppError {
    match error {
        RagError::BadMonth => AppError::new(
            ErrorCode::ValidationError,
            "month 必须是当前月或上月（YYYY-MM）",
            request_id,
        ),
        RagError::BadHash => AppError::new(
            ErrorCode::ValidationError,
            "hash 必须是 64 位小写十六进制",
            request_id,
        ),
        RagError::TooManyItems { limit } => AppError::new(
            ErrorCode::ValidationError,
            format!("单次最多 {limit} 条"),
            request_id,
        ),
        RagError::MonthlyQuota { limit } => AppError::new(
            ErrorCode::RateLimited,
            format!("本月累计上报已达上限 {limit} 条"),
            request_id,
        ),
        RagError::NoDatabase => AppError::new(
            ErrorCode::NotImplemented,
            "当前实例没有数据库，暂不支持哈希上报",
            request_id,
        ),
        RagError::Db(detail) => {
            tracing::warn!(error = %detail, "RAG 上报写库失败");
            AppError::new(
                ErrorCode::Internal,
                "上报暂时不可用，请稍后重试",
                request_id,
            )
        }
    }
}
