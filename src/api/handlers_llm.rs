//! LLM 端点（44 号 §12）：审计查询 / 命名操作代理（37 号）

use std::time::Instant;

use axum::extract::{Extension, Path, Query, State};
use axum::Json;
use serde_json::Value;

use crate::api::handlers_auth::now_iso;
use crate::api::{paginate, ApiError, AppState, AuthContext, Page, PageQuery};
use crate::llm_client::{LlmClient, LlmOpRequest, Operation};
use crate::model::auth::{can, is_org_admin, Action};
use crate::model::llm_audit::{LlmAuditFilter, LlmAuditStats, LlmOpAudit};

/// 解析路径中的操作名
fn parse_operation(s: &str) -> Option<Operation> {
    match s {
        "draft_rule" => Some(Operation::DraftRule),
        "gen_tests" => Some(Operation::GenTests),
        "explain_rule" => Some(Operation::ExplainRule),
        _ => None,
    }
}

/// LLM 操作审计列表（仅管理员，37 号 §8）
pub async fn list_llm_audits(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(page): Query<PageQuery>,
) -> Result<Json<Page<LlmOpAudit>>, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("仅管理员可查看 LLM 审计"));
    }
    let audits = state
        .store
        .list_llm_audits_filtered(&LlmAuditFilter::default())?;
    Ok(paginate(audits, page.limit, page.offset))
}

/// LLM 操作审计统计（仅管理员，37 号 §8）
pub async fn llm_audit_stats(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<LlmAuditStats>, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("仅管理员可查看 LLM 审计统计"));
    }
    let stats = state.store.llm_audit_stats()?;
    Ok(Json(stats))
}

/// LLM 命名操作代理（37 号）：转发 evo-agent serve `/ops/{op}`，并落操作级审计（37 号 §8）
pub async fn run_llm_op(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(operation): Path<String>,
    Json(req): Json<LlmOpRequest>,
) -> Result<Json<crate::llm_client::LlmOpResponse>, ApiError> {
    if !can(ctx.role, Action::Create) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let op = parse_operation(&operation)
        .ok_or_else(|| ApiError::bad_request(format!("非法操作: {operation}")))?;

    let client = LlmClient::new(&state.llm_base_url);
    let started = Instant::now();
    let call = client.call(op, &req);
    let duration_ms = started.elapsed().as_millis() as u64;

    // 操作级审计：成功/失败都落库（37 号 §8）
    let audit = match &call {
        Ok(resp) => LlmOpAudit {
            request_id: resp
                .request_id
                .clone()
                .unwrap_or_else(crate::llm_client::make_request_id),
            operation: op.as_str().to_string(),
            model: req.model.clone(),
            status: "completed".to_string(),
            duration_ms,
            result_ref: resp
                .result
                .get("entry_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            error: resp.errors.clone(),
            created_at: now_iso(),
        },
        Err(e) => LlmOpAudit {
            request_id: crate::llm_client::make_request_id(),
            operation: op.as_str().to_string(),
            model: req.model.clone(),
            status: "failed".to_string(),
            duration_ms,
            result_ref: None,
            error: Some(e.to_string()),
            created_at: now_iso(),
        },
    };
    if let Err(audit_err) = state.store.record_llm_audit(&audit) {
        tracing::warn!("LLM 操作审计落库失败: {audit_err}");
    }

    call.map(Json)
        .map_err(|e| ApiError::internal(e.to_string()))
}
