//! 条目级端点（44 号 §5 补全）：编辑 / 删除 / 提交候选 / 审批 / 历史 / 依赖
//!
//! 路由形态：顶层 `/entries/{id}`（entry_id 租户内定位，跨数据集查首个匹配）。
//! 闸门语义（34 号）：submit-candidate 需携带沙箱证据 `sandbox_report_id`（闸门一）；
//! approve 为闸门二（Candidate→Active，审批者角色）。

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use evorule_bundle::validate_rule_structure;

use crate::api::handlers_auth::now_iso;
use crate::api::{paginate, AppState, ApiError, AuthContext, Page};
use crate::model::auth::{Action, can};
use crate::model::dependency::SourceBinding;
use crate::model::entry::RuleEntry;
use crate::model::lifecycle::LifecycleStatus;
use crate::model::provenance::Provenance;
use crate::store::AnyEntry;

/// 顶层条目路由：租户内定位条目（Q12 R4：规则表与 knowledge 平行表均参与），校验租户归属
fn locate_entry(
    state: &AppState,
    tenant_id: &str,
    entry_id: &str,
) -> Result<(String, AnyEntry), ApiError> {
    state
        .store
        .find_entry_in_tenant(tenant_id, entry_id)?
        .ok_or_else(|| ApiError::not_found(format!("条目 `{entry_id}` 不存在")))
}

/// PATCH /entries/{id} —— 编辑草稿（frozen 拒绝原地修改）
///
/// Q12 R4 分流：规则条目改 rule_body/绑定；数据条目改 payload/schema_ref。
/// 字段与条目类型不符 → 显式 400（不静默忽略）。
#[derive(Deserialize)]
pub struct PatchEntryReq {
    #[serde(default)]
    pub rule_body: Option<Value>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    #[serde(default)]
    pub data_source_binding: Option<Vec<SourceBinding>>,
    #[serde(default)]
    pub consumed_inputs: Option<Vec<String>>,
    /// knowledge 条目：领域结构化数据本体
    #[serde(default)]
    pub payload: Option<Value>,
    /// knowledge 条目：领域 JSON Schema 引用
    #[serde(default)]
    pub schema_ref: Option<String>,
}

pub async fn patch_entry(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
    Json(req): Json<PatchEntryReq>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let (_dataset_id, entry) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    match entry {
        AnyEntry::Rule(mut e) => {
            if req.payload.is_some() || req.schema_ref.is_some() {
                return Err(ApiError::bad_request(
                    "rule_set 条目不接受 payload/schema_ref 字段（数据条目字段仅限 knowledge 数据集）",
                ));
            }
            if let Some(b) = req.rule_body {
                if let Err(errors) = validate_rule_structure(&b) {
                    return Err(ApiError::bad_request(format!(
                        "规则体结构校验失败: {}",
                        errors.join("; ")
                    )));
                }
                e.rule_body = b;
            }
            if let Some(t) = req.tags {
                e.tags = t;
            }
            if let Some(p) = req.provenance {
                e.provenance = p;
            }
            if let Some(b) = req.data_source_binding {
                e.data_source_binding = b;
            }
            if let Some(c) = req.consumed_inputs {
                e.consumed_inputs = c;
            }
            state.store.update_draft_entry(&e)?;
        }
        AnyEntry::Knowledge(mut e) => {
            if req.rule_body.is_some()
                || req.data_source_binding.is_some()
                || req.consumed_inputs.is_some()
            {
                return Err(ApiError::bad_request(
                    "knowledge 条目不接受 rule_body/data_source_binding/consumed_inputs 字段（规则条目字段仅限 rule_set 数据集）",
                ));
            }
            if let Some(p) = req.payload {
                e.payload = p;
            }
            if let Some(s) = req.schema_ref {
                e.schema_ref = s;
            }
            if let Some(t) = req.tags {
                e.tags = t;
            }
            if let Some(p) = req.provenance {
                e.provenance = p;
            }
            state.store.update_draft_knowledge_entry(&e)?;
        }
    }
    // 回读最新状态
    let (_, updated) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    Ok(Json(updated.to_json()))
}

/// DELETE /entries/{id} —— 删除草稿（仅 Draft；Q12 R4 分流平行表）
pub async fn delete_entry(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let (dataset_id, entry) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    match entry {
        AnyEntry::Rule(_) => state.store.delete_entry(&dataset_id, &entry_id)?,
        AnyEntry::Knowledge(_) => state.store.delete_knowledge_entry(&dataset_id, &entry_id)?,
    }
    Ok(StatusCode::NO_CONTENT)
}

/// POST /entries/{id}/submit-candidate —— 闸门一：Draft → Candidate（需沙箱 verdict=pass 证据）
#[derive(Deserialize)]
pub struct SubmitCandidateReq {
    /// 沙箱验证报告 ID（闸门一证据，34 号）
    pub sandbox_report_id: String,
}

pub async fn submit_candidate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
    Json(req): Json<SubmitCandidateReq>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let (dataset_id, _) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    transition_any(&state, &dataset_id, &entry_id, LifecycleStatus::Candidate, &ctx.user_id,
        &format!("闸门一通过，沙箱报告 {}", req.sandbox_report_id))?;
    let (_, updated) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    Ok(Json(updated.to_json()))
}

/// 条目状态迁移分流（Q12 R4：规则/知识平行表同口径迁移）
fn transition_any(
    state: &AppState,
    dataset_id: &str,
    entry_id: &str,
    to: LifecycleStatus,
    user_id: &str,
    cause: &str,
) -> Result<(), ApiError> {
    let rule_err = match state.store.transition_entry_status(
        dataset_id, entry_id, to, user_id, &now_iso(), cause,
    ) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    // 规则表未命中 → knowledge 平行表重试；规则表的其余错误（状态机/LLM 边界）直接上抛
    if !matches!(rule_err, crate::store::StoreError::EntryNotFound { .. }) {
        return Err(rule_err.into());
    }
    state.store.transition_knowledge_entry_status(
        dataset_id, entry_id, to, user_id, &now_iso(), cause,
    )?;
    Ok(())
}

/// POST /entries/{id}/approve —— 闸门二：Candidate → Active（审批者角色）
pub async fn approve(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::Approve) {
        return Err(ApiError::forbidden("审批需审批者及以上角色"));
    }
    let (dataset_id, _) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    transition_any(&state, &dataset_id, &entry_id, LifecycleStatus::Active, &ctx.user_id,
        "闸门二审批通过（Candidate→Active）")?;
    let (_, updated) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    Ok(Json(updated.to_json()))
}

/// GET /entries/{id}/history —— 条目状态迁移历史（only-append）
pub async fn history(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
) -> Result<Json<Vec<crate::model::lifecycle::StateChange>>, ApiError> {
    let (dataset_id, _) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    // Q12 R4：先查规则表历史，空则查 knowledge 平行表
    let mut hist = state.store.get_entry_state_history(&dataset_id, &entry_id)?;
    if hist.is_empty() {
        hist = state.store.get_knowledge_entry_state_history(&dataset_id, &entry_id)?;
    }
    Ok(Json(hist))
}

/// GET /entries/{id}/deps —— 条目级 data_source_binding（35 号 §3 层 2 绑定）
pub async fn deps(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
) -> Result<Json<Vec<SourceBinding>>, ApiError> {
    let (_, entry) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    match entry {
        AnyEntry::Rule(e) => Ok(Json(e.data_source_binding)),
        // 数据条目不消费服务（D1：不进 TCB，不经 io_request）；空 = 无绑定（显式语义）
        AnyEntry::Knowledge(_) => Ok(Json(vec![])),
    }
}

// ----------------------------------------------------------------------
// 顶层条目列表 / 详情 / 创建（44 号 §5：GET/POST /entries）
// ----------------------------------------------------------------------

#[derive(Deserialize)]
pub struct EntryListQuery {
    #[serde(default)]
    pub dataset_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

/// GET /entries —— 租户内条目列表（可选 ?dataset_id= 过滤；Q12 R4：含 knowledge 数据条目）
pub async fn list_entries_all(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(query): Query<EntryListQuery>,
) -> Result<Json<Page<Value>>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let rules = state.store.search_entries(
        &ctx.tenant_id,
        query.dataset_id.as_deref(),
        None,
        None,
        &[],
        None,
    )?;
    let mut items: Vec<Value> = rules
        .iter()
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    // knowledge 平行表合并（同租户；dataset_id 过滤条件同样生效）
    for ds in state.store.list_datasets(&ctx.tenant_id)? {
        if let Some(did) = &query.dataset_id {
            if ds.dataset_id != *did {
                continue;
            }
        }
        for e in state.store.list_knowledge_entries(&ds.dataset_id, None)? {
            items.push(serde_json::to_value(e).unwrap_or(Value::Null));
        }
    }
    Ok(paginate(items, query.limit, query.offset))
}

/// GET /entries/{id} —— 条目详情（租户内定位）
pub async fn get_entry(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (_, entry) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    Ok(Json(entry.to_json()))
}

/// GET /entries/{id}/versions —— 条目版本历史（C1，33 号 §6 历史可回查）
pub async fn entry_versions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let (dataset_id, entry) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    // Q12 R4 分流：摘要字段同构（version/status/content_hash），载荷类型不影响版本链视图
    let summary: Vec<Value> = match entry {
        AnyEntry::Rule(_) => state
            .store
            .list_entry_versions(&dataset_id, &entry_id)?
            .iter()
            .map(|e| {
                serde_json::json!({
                    "version": e.version,
                    "status": e.status,
                    "content_hash": e.content_hash(),
                })
            })
            .collect(),
        AnyEntry::Knowledge(_) => state
            .store
            .list_knowledge_entry_versions(&dataset_id, &entry_id)?
            .iter()
            .map(|e| {
                serde_json::json!({
                    "version": e.version,
                    "status": e.status,
                    "content_hash": e.content_hash(),
                })
            })
            .collect(),
    };
    Ok(Json(serde_json::json!({
        "dataset_id": dataset_id,
        "entry_id": entry_id,
        "versions": summary,
    })))
}

/// GET /entries/{id}/diff?from=..&to=.. —— 条目内容级 diff（C2，44 号 §9 / 33 号）
#[derive(Deserialize)]
pub struct EntryDiffQuery {
    pub from: u32,
    pub to: u32,
}

pub async fn entry_diff(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(entry_id): Path<String>,
    Query(query): Query<EntryDiffQuery>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let (dataset_id, entry) = locate_entry(&state, &ctx.tenant_id, &entry_id)?;
    // Q12 R4 分流：载荷类型不影响内容级 diff 语义（content_hash 刻定内容）
    let out = match entry {
        AnyEntry::Rule(_) => state
            .store
            .entry_content_diff(&dataset_id, &entry_id, query.from, query.to)
            .map_err(map_store_err)?,
        AnyEntry::Knowledge(_) => state
            .store
            .knowledge_entry_content_diff(&dataset_id, &entry_id, query.from, query.to)
            .map_err(map_store_err)?,
    };
    Ok(Json(out))
}

fn map_store_err(e: crate::store::StoreError) -> ApiError {
    use crate::store::StoreError as SE;
    match e {
        SE::InvalidDiffRange { from, to } => ApiError::bad_request(format!(
            "版本 diff 区间非法: from=`{from}` to=`{to}`（from 须先于 to 且版本存在）"
        )),
        SE::EntryVersionNotFound { dataset, entry, version } => ApiError::not_found(format!(
            "条目版本不存在: dataset=`{dataset}` entry=`{entry}` version=`{version}`"
        )),
        other => other.into(),
    }
}

/// POST /entries —— 顶层创建条目（默认 Draft，dataset_id 在 body 中）
#[derive(Deserialize)]
pub struct CreateEntryReq {
    pub dataset_id: String,
    pub entry_id: String,
    #[serde(default)]
    pub version: Option<u32>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub consumed_inputs: Vec<String>,
    pub rule_body: Value,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    #[serde(default)]
    pub data_source_binding: Vec<SourceBinding>,
}

pub async fn create_entry(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateEntryReq>,
) -> Result<(StatusCode, Json<RuleEntry>), ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let ds = state
        .store
        .get_dataset(&req.dataset_id)?
        .ok_or_else(|| ApiError::not_found(format!("数据集 `{}` 不存在", req.dataset_id)))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found(format!("数据集 `{}` 不存在", req.dataset_id)));
    }
    let version = req.version.unwrap_or(1);
    let domain = req.domain.unwrap_or_else(|| {
        ds.domain.first().cloned().unwrap_or_else(|| "general".to_string())
    });
    let provenance = req.provenance.unwrap_or_else(|| Provenance {
        source: "API 收录".into(),
        clause: None,
        document_id: None,
        effective_from: None,
        effective_to: None,
        last_verified: None,
        verified_by: None,
    });
    let entry = RuleEntry {
        entry_id: req.entry_id,
        dataset_id: req.dataset_id.clone(),
        version,
        status: Some(LifecycleStatus::Draft),
        provenance,
        domain,
        tags: req.tags,
        data_source_binding: req.data_source_binding,
        consumed_inputs: req.consumed_inputs,
        rule_body: req.rule_body,
        governance: None,
    };
    if let Err(errors) = validate_rule_structure(&entry.rule_body) {
        return Err(ApiError::bad_request(format!(
            "规则体结构校验失败: {}",
            errors.join("; ")
        )));
    }
    state.store.add_entry(&entry)?;
    Ok((StatusCode::CREATED, Json(entry)))
}
