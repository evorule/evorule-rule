//! 数据集 / 条目 / 生命周期 / 快照包端点（设计文档 §8-§11）

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::api::handlers_auth::now_iso;
use crate::api::{
    api_key_from_header, bearer_token, paginate, unix_now, ApiError, AppState, AuthContext, Page,
    PageQuery,
};
use crate::auth::iso_from_unix;
use crate::model::auth::{can, is_org_admin, Action, Role};
use crate::model::dataset::{DatasetKind, Meta, RuleDataset, Visibility};
use crate::model::entry::RuleEntry;
use crate::model::governance::{Governance, LlmGenerated};
use crate::model::lifecycle::LifecycleStatus;
use crate::model::provenance::Provenance;
use crate::model::version::{BumpKind, LawRef, VersionSelection, Versioning};

// ----------------------------------------------------------------------
// 数据集
// ----------------------------------------------------------------------

// —— 生效基准前置校验（，2026-09-02）——
// 缺口实证（四场景实测）：新建数据集 law_ref 缺省 + version_selection 缺省
// （= auto_by_effective_date）→ 发布/导出无阻拦 → 部署到执行域时才被导入校验
// 400 拒绝（"auto_by_effective_date 模式需快照包携带 law_ref.effective_from 作为
// 生效基准"）。报错诚实但暴露过晚——治理侧在创建/更新/发布期 fail-fast 前移。
// 三层闸门：
//   ① 创建：显式选择 auto 模式却缺生效基准 → 400（配置错误，创建即可见）；
//   ② 更新：PATCH 合并后显式 auto 模式缺生效基准 → 400（不允许经 PATCH 引入错误配置）；
//   ③ 发布：auto 模式（含缺省）缺生效基准 → 400（硬闸门，与执行域导入校验口径一致；
//      缺省模式草稿允许创建/编辑，但发布必拦）。

/// 是否缺失生效基准（law_ref 缺失或 effective_from 缺失均视为缺失）
fn missing_effective_basis(law_ref: &Option<LawRef>) -> bool {
    match law_ref {
        Some(lr) => lr.effective_from.is_none(),
        None => true,
    }
}

/// 生效基准缺失错误（含自诊断修复指引，遵循"系统自愈 + 用户可见"）
fn effective_basis_error(dataset_id: &str) -> ApiError {
    ApiError::bad_request(format!(
        "生效基准缺失（前置校验）：版本选择模式为 auto_by_effective_date（缺省即该模式），\
         数据集 `{dataset_id}` 需携带 law_ref.effective_from 作为生效基准，\
         否则部署到执行域时将被导入校验拒绝。\
         修复指引：PATCH /v1/datasets/{dataset_id} 补充 law_ref（至少 document_id + effective_from，\
         如 {{\"law_ref\":{{\"document_id\":\"…\",\"effective_from\":\"2026-01-01\"}}}}），\
         或将 version_selection 切换为 pinned 并指定 pinned_version"
    ))
}

/// 校验显式声明的 version_selection（创建/更新期闸门：只拦显式 auto 缺基准；
/// 缺省模式留给发布闸门，不阻断草稿工作流）
fn validate_explicit_selection(
    dataset_id: &str,
    version_selection: &Option<VersionSelection>,
    law_ref: &Option<LawRef>,
) -> Result<(), ApiError> {
    if let Some(vs) = version_selection {
        if vs.mode == crate::model::version::VersionSelectionMode::AutoByEffectiveDate
            && missing_effective_basis(law_ref)
        {
            return Err(effective_basis_error(dataset_id));
        }
    }
    Ok(())
}

/// 校验数据集发布就绪的生效基准（发布闸门：显式与缺省 auto 均拦截，口径与
/// 执行域导入校验一致——见 evorule-bundle bundle.rs validate 第 5 步）
fn validate_publish_effective_basis(
    ds: &crate::model::dataset::RuleDataset,
) -> Result<(), ApiError> {
    let mode = ds
        .version_selection
        .as_ref()
        .map(|vs| vs.mode)
        .unwrap_or(crate::model::version::VersionSelectionMode::AutoByEffectiveDate);
    if mode == crate::model::version::VersionSelectionMode::AutoByEffectiveDate
        && missing_effective_basis(&ds.law_ref)
    {
        return Err(effective_basis_error(&ds.dataset_id));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct CreateDatasetReq {
    pub dataset_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub domain: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// 法规锚（合规场景，可选）
    #[serde(default)]
    pub law_ref: Option<LawRef>,
    /// 版本选择双模式（可选；缺省 = auto_by_effective_date）
    #[serde(default)]
    pub version_selection: Option<VersionSelection>,
    /// 数据集类型（R1，可选；缺省 = rule_set。创建后不可变更）
    #[serde(default)]
    pub dataset_kind: Option<DatasetKind>,
}

pub async fn list_datasets(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(page): Query<PageQuery>,
) -> Result<Json<Page<RuleDataset>>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    // 浏览口径（交付边界收口 A，V3 反转）：本租户全部 + 他租户 Public+Published
    let ds = state.store.list_datasets_browsable(&ctx.tenant_id)?;
    Ok(paginate(ds, page.limit, page.offset))
}

/// GET /datasets/{id}/snapshots/stats —— 内容去重统计（C1：版本行 vs 去重快照）
pub async fn snapshot_stats(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    Ok(Json(state.store.snapshot_dedup_stats(&id)?))
}

pub async fn create_dataset(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateDatasetReq>,
) -> Result<(StatusCode, Json<RuleDataset>), ApiError> {
    if !can(ctx.role, Action::Create) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    // 闸门①：显式 auto 模式缺生效基准 → 创建即拒（配置错误，创建即可见）
    validate_explicit_selection(&req.dataset_id, &req.version_selection, &req.law_ref)?;
    let now = now_iso();
    let ds = RuleDataset {
        dataset_id: req.dataset_id,
        name: req.name,
        description: req.description,
        dataset_kind: req.dataset_kind.unwrap_or(DatasetKind::RuleSet),
        domain: req.domain,
        tags: req.tags,
        tenant_id: ctx.tenant_id.clone(),
        visibility: req.visibility.unwrap_or(Visibility::Private),
        lifecycle: crate::model::lifecycle::Lifecycle::default(),
        versioning: crate::model::version::Versioning::default(),
        law_ref: req.law_ref,
        version_selection: req.version_selection,
        data_dependencies: None,
        event_schemas: vec![],
        meta: Meta {
            created_at: now.clone(),
            created_by: ctx.user_id.clone(),
            updated_at: None,
            updated_by: None,
        },
    };
    state.store.create_dataset(&ds)?;
    Ok((StatusCode::CREATED, Json(ds)))
}

pub async fn get_dataset(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<RuleDataset>, ApiError> {
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    // 数据隔离（⑧）：本租户可见；跨租户 **Public+Published**（设计文档 §3 双条件）只读可见
    // （段2 P4/V1：与 search_datasets 跨租户检索口径一致；写操作仍被租户+角色拦截）
    if ds.tenant_id != ctx.tenant_id && !state.store.is_publicly_pullable(&id)? {
        return Err(ApiError::not_found("数据集不存在"));
    }
    Ok(Json(ds))
}

// ----------------------------------------------------------------------
// 生命周期迁移（设计文档 §13-3：统一 PATCH /lifecycle）+ 独立发布
// ----------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LifecycleReq {
    /// candidate | active | rejected（published 走独立发布端点）
    pub to: String,
}

pub async fn transition_lifecycle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<LifecycleReq>,
) -> Result<Json<RuleDataset>, ApiError> {
    let to = match req.to.as_str() {
        "candidate" => LifecycleStatus::Candidate,
        "active" => LifecycleStatus::Active,
        "rejected" => LifecycleStatus::Rejected,
        "published" => {
            return Err(ApiError::bad_request(
                "Published 必须走独立发布审批端点 POST /publish",
            ))
        }
        other => return Err(ApiError::bad_request(format!("非法目标状态: {other}"))),
    };
    // 状态迁移权限（设计文档 §9 定案：闸门/审批/撤销需对应角色）
    let allowed = match to {
        LifecycleStatus::Candidate => can(ctx.role, Action::Create),
        LifecycleStatus::Active => can(ctx.role, Action::Approve),
        LifecycleStatus::Rejected => is_org_admin(ctx.role),
        _ => false,
    };
    if !allowed {
        return Err(ApiError::forbidden("当前角色无权执行该状态迁移"));
    }
    // 租户归属校验（设计文档 §10-3：跨租户返回 404，防越权迁移他租户数据集）
    let owned = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if owned.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    let at = iso_from_unix(unix_now());
    state.store.transition_dataset_status(
        &id,
        to,
        &ctx.user_id,
        &format!("API 迁移 to {to:?}"),
        &at,
    )?;
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    Ok(Json(ds))
}

/// 独立发布审批（设计文档 §3 强约束）：Active → Published，发布者复用审批者 + 二次确认
///
/// 二次确认（设计文档 §9-1 / 设计文档 §10-2 定案"防误发"；设计未定义具体协议）：
/// MVP 以**显式确认字段**固化——请求体必须携带且 `confirm==true` 才执行，
/// 否则返回 400（把"弹窗二次确认"固化为接口契约，防误发；双步 token 回执后置批次 1）。
#[derive(Deserialize)]
pub struct PublishReq {
    /// 二次确认回执（防误发）：必须显式置 true，缺省视为未确认
    pub confirm: bool,
    /// 可选发布原因，记入审计 cause
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn publish(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<PublishReq>,
) -> Result<Json<RuleDataset>, ApiError> {
    if !can(ctx.role, Action::Publish) {
        return Err(ApiError::forbidden("发布需审批者及以上角色"));
    }
    // 租户归属校验（设计文档 §10-3：SQL 层 + 应用层补偿，跨租户返回 404）
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    if !req.confirm {
        return Err(ApiError::bad_request(
            "发布需二次确认：请求体须携带 confirm=true（防误发，设计文档 §9-1）",
        ));
    }
    // 闸门③：auto 模式（含缺省）缺生效基准 → 发布即拒（硬闸门，口径与
    // 执行域导入校验一致；W2.2 实证该缺口此前要到部署时才 400 暴露）
    validate_publish_effective_basis(&ds)?;
    let at = iso_from_unix(unix_now());
    let cause = req
        .reason
        .map(|r| format!("独立发布审批通过（二次确认），原因: {r}"))
        .unwrap_or_else(|| "独立发布审批通过（二次确认）".to_string());
    state
        .store
        .publish_dataset_with_cause(&id, &ctx.user_id, &at, &cause)?;
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    Ok(Json(ds))
}

// ----------------------------------------------------------------------
// 数据集元数据 / 版本 / 撤销发布（设计文档 §4 补全）
// ----------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PatchDatasetReq {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub domain: Option<Vec<String>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// 法规锚（可选；None = 不修改）
    #[serde(default)]
    pub law_ref: Option<LawRef>,
    /// 版本选择双模式（可选；None = 不修改）
    #[serde(default)]
    pub version_selection: Option<VersionSelection>,
}

/// PATCH /datasets/{id} —— 更新元数据（域/描述/标签/可见性；版本链/生命周期/依赖由专用端点管理）
pub async fn update_dataset_meta(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<PatchDatasetReq>,
) -> Result<Json<RuleDataset>, ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let mut ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    if let Some(n) = req.name {
        ds.name = n;
    }
    if let Some(d) = req.description {
        ds.description = Some(d);
    }
    if let Some(d) = req.domain {
        ds.domain = d;
    }
    if let Some(t) = req.tags {
        ds.tags = t;
    }
    if let Some(v) = req.visibility {
        ds.visibility = v;
    }
    if let Some(l) = req.law_ref {
        ds.law_ref = Some(l);
    }
    if let Some(vs) = req.version_selection {
        ds.version_selection = Some(vs);
    }
    // 闸门②：PATCH 合并后显式 auto 模式缺生效基准 → 拒绝
    //（不允许经 PATCH 引入"显式 auto 无锚"错误配置；缺省模式留给发布闸门）
    validate_explicit_selection(&ds.dataset_id, &ds.version_selection, &ds.law_ref)?;
    let at = iso_from_unix(unix_now());
    ds.meta.updated_at = Some(at);
    ds.meta.updated_by = Some(ctx.user_id.clone());
    state.store.update_dataset(&ds)?;
    Ok(Json(ds))
}

/// DELETE /datasets/{id} —— 删除数据集（仅 Draft/Rejected，admin）
pub async fn delete_dataset_meta(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("删除数据集需管理员角色"));
    }
    state.store.delete_dataset(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /datasets/{id}/versions —— 版本链（历史批次）
pub async fn list_versions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Versioning>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    Ok(Json(state.store.list_dataset_versions(&id)?))
}

/// GET /datasets/{id}/versions/{ver} —— 版本详情（MVP 仅当前版本有内容快照，诚实标注）
pub async fn get_version(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((id, ver)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    if !ds.versioning.chain.iter().any(|v| v == &ver) {
        return Err(ApiError::not_found(format!("版本 `{ver}` 不在版本链中")));
    }
    let content_available = ds.versioning.current == ver;
    Ok(Json(serde_json::json!({
        "dataset_id": id,
        "version": ver,
        "current": ds.versioning.current,
        "chain": ds.versioning.chain,
        "content_available": content_available,
        "note": if content_available {
            "当前版本，条目内容见 GET /datasets/{id}/entries"
        } else {
            "MVP 仅存当前版本条目内容；历史版本内容待批次 1 快照落库"
        }
    })))
}

#[derive(Deserialize)]
pub struct NewVersionReq {
    /// major（法规条款级升版）| patch（内部小改）
    pub kind: String,
}

/// POST /datasets/{id}/versions —— 创建新版本（既定设计决策 两级变更线）
pub async fn create_version(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<NewVersionReq>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let kind = match req.kind.as_str() {
        "major" => BumpKind::Major,
        "patch" => BumpKind::Patch,
        other => {
            return Err(ApiError::bad_request(format!(
                "非法变更线: {other}（major|patch）"
            )))
        }
    };
    let new_version =
        state
            .store
            .create_dataset_version(&id, kind, &ctx.user_id, &iso_from_unix(unix_now()))?;
    let v = state.store.list_dataset_versions(&id)?;
    Ok(Json(serde_json::json!({
        "dataset_id": id,
        "new_version": new_version,
        "current": v.current,
        "chain": v.chain,
    })))
}

/// POST /datasets/{id}/versions/{ver}/patch —— 对指定版本创建 Patch（内部小改，历史批次）
///
/// MVP 仅当前版本可补丁（历史版本内容未落库），非当前版本 → 显式拒绝不伪造。
pub async fn create_patch(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((id, ver)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    if ds.versioning.current != ver {
        return Err(ApiError::bad_request(format!(
            "仅当前版本 `{}` 可创建 Patch（历史版本内容 MVP 未落库）；请求版本 `{ver}`",
            ds.versioning.current
        )));
    }
    let new_version = state.store.create_dataset_version(
        &id,
        BumpKind::Patch,
        &ctx.user_id,
        &iso_from_unix(unix_now()),
    )?;
    let v = state.store.list_dataset_versions(&id)?;
    Ok(Json(serde_json::json!({
        "dataset_id": id,
        "base_version": ver,
        "new_version": new_version,
        "current": v.current,
        "chain": v.chain,
    })))
}

/// POST /datasets/{id}/unpublish —— 撤销发布（Published → Rejected，admin）
pub async fn unpublish(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<RuleDataset>, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("撤销发布需管理员角色"));
    }
    // 租户归属校验（设计文档 §10-3：跨租户返回 404，防越权撤销他租户发布）
    let owned = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if owned.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    state
        .store
        .unpublish_dataset(&id, &ctx.user_id, &iso_from_unix(unix_now()))?;
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    Ok(Json(ds))
}

// ----------------------------------------------------------------------
// 条目
// ----------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AddEntryReq {
    pub entry_id: String,
    /// 治理版本：必填，递增（与 store 唯一键 dataset_id+entry_id+version 协同）
    pub version: u32,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub consumed_inputs: Vec<String>,
    pub rule_body: Value,
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

/// knowledge 数据条目请求体（R4：payload + schema_ref 必填，与规则条目互斥）
#[derive(Deserialize)]
pub struct AddKnowledgeEntryReq {
    pub entry_id: String,
    pub version: u32,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 领域结构化数据本体（任意 JSON，过 schema_ref 领域 schema 强校验）
    pub payload: Value,
    /// 领域 JSON Schema 引用 URI（resolver 未命中 = 拒绝入库，D3）
    pub schema_ref: String,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    /// 知识资产化批次 A（全 Option，缺省=契约未声明态，旧调用方零破坏）：
    /// kind 谱系 / 来源信任级 / 许可证域引用 / 运行契约
    #[serde(default)]
    pub knowledge_kind: Option<String>,
    #[serde(default)]
    pub trust_level: Option<String>,
    #[serde(default)]
    pub license_ref: Option<String>,
    #[serde(default)]
    pub execution_contract: Option<evorule_bundle::ExecutionContract>,
    /// 治理补充信息（O-318 接线：REST 入账收 governance，缺省 None = 旧调用方零破坏）。
    /// llm_generated.flag=true 的条目入账一律 Draft（store 层 validate_llm_boundary_gated
    /// 既有强约束）；机器提议通路（/invoke/propose_knowledge_entry）不收本字段——
    /// 旗标由服务端强制构造，请求值不可生效。
    #[serde(default)]
    pub governance: Option<Governance>,
}

pub async fn list_entries(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(page): Query<PageQuery>,
    Query(filter): Query<ListEntriesFilter>,
) -> Result<Json<Page<Value>>, ApiError> {
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    // Q12 R4：按数据集类型分流（rule_set → 规则条目；knowledge → 数据条目）
    // B3（段B 历史批次）：filter=... 条目查询表达式（SSOT 过滤核 = evorule-bundle EntryFilter，
    // 与 bundle subset 语法同族）；过滤视图走 BundleEntry 映射，命中后回留原始 JSON（保留治理上下文）。
    let (entries, view): (Vec<Value>, Vec<crate::bundle::BundleEntry>) = match ds.dataset_kind {
        DatasetKind::RuleSet => {
            let es = state.store.list_entries(&id, None)?;
            let view = es
                .iter()
                .map(crate::bundle::BundleExporter::rule_entry_to_bundle)
                .collect();
            let raw = es
                .iter()
                .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
                .collect();
            (raw, view)
        }
        DatasetKind::Knowledge => {
            let es = state.store.list_knowledge_entries(&id, None)?;
            let view = es
                .iter()
                .map(crate::bundle::BundleExporter::knowledge_entry_to_bundle)
                .collect();
            let raw = es
                .iter()
                .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
                .collect();
            (raw, view)
        }
    };
    let entries: Vec<Value> = match filter.filter.as_deref() {
        None => entries,
        Some(spec) => {
            let ids: std::collections::BTreeSet<String> =
                crate::bundle::EntryFilter::apply(&view, spec)
                    .map_err(|e| ApiError::bad_request(format!("filter 表达式非法: {e}")))?
                    .into_iter()
                    .map(|e| e.entry_id)
                    .collect();
            entries
                .into_iter()
                .filter(|v| {
                    v.get("entry_id")
                        .and_then(|x| x.as_str())
                        .is_some_and(|s| ids.contains(s))
                })
                .collect()
        }
    };
    Ok(paginate(entries, page.limit, page.offset))
}

/// B3：条目查询表达式查询参数（与分页参数并存，Query 双提取互不干扰）
#[derive(Deserialize, Default)]
pub struct ListEntriesFilter {
    /// `tag:x` / `domain:x` / `ids:a,b` / `kind:rule|knowledge` / `q:子串`（多段以 ; 分隔，交集）
    #[serde(default)]
    pub filter: Option<String>,
}

pub async fn add_entry(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    // Q12 R4：按数据集类型分流校验（同一端点，两类条目互斥、显式报错）
    match ds.dataset_kind {
        DatasetKind::Knowledge => {
            let req: AddKnowledgeEntryReq = serde_json::from_value(body).map_err(|e| {
                ApiError::bad_request(format!(
                    "knowledge 数据集条目须为 {{entry_id, version, payload, schema_ref, ...}}: {e}"
                ))
            })?;
            let domain = req.domain.unwrap_or_else(|| {
                ds.domain
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "general".to_string())
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
            let entry = crate::model::knowledge::KnowledgeEntry {
                entry_id: req.entry_id,
                dataset_id: id.clone(),
                version: req.version,
                status: Some(LifecycleStatus::Draft),
                provenance,
                domain,
                tags: req.tags,
                payload: req.payload,
                schema_ref: req.schema_ref,
                governance: req.governance,
                knowledge_kind: req.knowledge_kind,
                trust_level: req.trust_level,
                license_ref: req.license_ref,
                execution_contract: req.execution_contract,
            };
            // 知识资产化批次 A：入账契约完整性校验（三通道同一闸，无通道旁路；§3.2）
            entry
                .validate_ingest_contract()
                .map_err(ApiError::bad_request)?;
            state.store.add_knowledge_entry(&entry)?;
            Ok((
                StatusCode::CREATED,
                Json(serde_json::to_value(&entry).unwrap_or(Value::Null)),
            ))
        }
        DatasetKind::RuleSet => {
            let req: AddEntryReq = serde_json::from_value(body).map_err(|e| {
                ApiError::bad_request(format!(
                    "rule_set 数据集条目须为 {{entry_id, version, rule_body, ...}}: {e}"
                ))
            })?;
            let domain = req.domain.unwrap_or_else(|| {
                ds.domain
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "general".to_string())
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
                dataset_id: id.clone(),
                version: req.version,
                status: Some(LifecycleStatus::Draft),
                provenance,
                domain,
                tags: req.tags,
                data_source_binding: vec![],
                consumed_inputs: req.consumed_inputs,
                rule_body: req.rule_body,
                governance: None,
            };
            state.store.add_entry(&entry)?;
            Ok((
                StatusCode::CREATED,
                Json(serde_json::to_value(&entry).unwrap_or(Value::Null)),
            ))
        }
    }
}

// ----------------------------------------------------------------------
// 快照包（导出交付；支持 JWT 或 X-Api-Key(pull)）
// ----------------------------------------------------------------------

/// 快照包拉取：优先 Bearer（登录用户，本租户或 public+Published），否则 X-Api-Key(pull scope)
pub async fn get_bundle(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<crate::bundle::DatasetBundle>, ApiError> {
    // 1) X-Api-Key（执行侧联动，设计文档 §14：pull scope）
    if let Some(k) = api_key_from_header(&headers) {
        let hash = sha256_hex(k);
        let key = state
            .store
            .get_api_key_by_hash(&hash)?
            .ok_or_else(|| ApiError::unauthorized("API Key 非法"))?;
        if key.revoked_at.is_some() {
            return Err(ApiError::unauthorized("API Key 已吊销"));
        }
        if key.scope != "pull" {
            return Err(ApiError::forbidden("API Key 无 pull 权限"));
        }
        return export_for(&state, &key.tenant_id, &id, key.key_id.as_str()).await;
    }
    // 2) Bearer（登录用户）
    if let Some(token) = bearer_token(&headers) {
        let claims = state
            .auth
            .verify_token(token, unix_now(), "access")
            .map_err(|_| ApiError::unauthorized("token 非法或已过期"))?;
        let ctx = AuthContext {
            user_id: claims.sub,
            tenant_id: claims.tenant_id,
            role: Role::parse(&claims.role).ok_or_else(|| ApiError::unauthorized("角色非法"))?,
        };
        return export_for(&state, &ctx.tenant_id, &id, &ctx.user_id).await;
    }
    Err(ApiError::unauthorized(
        "缺少认证：需 Bearer token 或 X-Api-Key",
    ))
}

// ----------------------------------------------------------------------
// 治理侧外部导入（知识资产化 A3-1：外部 bundle → 既有 knowledge 数据集，
// 导入设计 §三：一律 Draft 落账 + 强制 external:{source} 打标，RuleEngineer+）
// ----------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ImportKnowledgeReq {
    pub bundle: crate::bundle::DatasetBundle,
    /// 导入者声明的来源标识（强制打标 `external:{source}`；bundle 内字段不采信，可伪造）
    pub source: String,
}

/// 导入错误映射：校验链失败 = 客户端 400（显式、不静默），其余走统一映射
fn import_knowledge_err(e: crate::store::StoreError) -> ApiError {
    match e {
        crate::store::StoreError::Bundle(b) => {
            ApiError::bad_request(format!("导入校验失败（不静默降级）: {b}"))
        }
        other => ApiError::from(other),
    }
}

/// 共用执行核（dry-run 与正式导入同一实现，预检与入库无旁路）
async fn exec_import_knowledge(
    state: &AppState,
    ctx: &AuthContext,
    dataset_id: &str,
    req: ImportKnowledgeReq,
    dry_run: bool,
) -> Result<crate::store::KnowledgeImportResult, ApiError> {
    if !can(ctx.role, Action::Create) {
        return Err(ApiError::forbidden(
            "外部知识导入=创建 Draft 条目，需规则工程师及以上角色",
        ));
    }
    let source = req.source.trim().to_string();
    if source.is_empty() {
        return Err(ApiError::bad_request(
            "source 必填：外部导入须显式声明来源（bundle 内字段不采信，可伪造）",
        ));
    }
    let at = iso_from_unix(unix_now());
    state
        .store
        .import_knowledge_entries(dataset_id, &req.bundle, &source, &ctx.user_id, &at, dry_run)
        .map_err(import_knowledge_err)
}

/// POST /datasets/{id}/import_knowledge —— 治理侧外部导入（RuleEngineer+）：
/// 外部快照包条目导入既有 knowledge 数据集；一律 Draft 落账 + 强制 external:{source}
/// 打标（包自带 human/llm 视为冒充，400 显式拒绝）；六项校验链全跑 + 入账契约闸
/// （external 必带 license_ref）+ 凭据扫描；单事务原子，任一条失败整体回滚。
pub async fn import_knowledge(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<ImportKnowledgeReq>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let result = exec_import_knowledge(&state, &ctx, &id, req, false).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "import_id": format!("imp-{}", unix_now()),
            "status": "imported",
            "dataset_id": result.dataset_id,
            "bundle_id": result.bundle_id,
            "trust_tag": result.trust_tag,
            "imported_count": result.imported.len(),
            "entry_ids": result.imported,
        })),
    ))
}

/// POST /datasets/{id}/import_knowledge/dry-run —— 导入预检（与正式导入同一实现，
/// 全闸跑完零写入）
pub async fn import_knowledge_dry_run(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<ImportKnowledgeReq>,
) -> Result<Json<Value>, ApiError> {
    let result = exec_import_knowledge(&state, &ctx, &id, req, true).await?;
    Ok(Json(serde_json::json!({
        "valid": true,
        "dry_run": true,
        "dataset_id": result.dataset_id,
        "bundle_id": result.bundle_id,
        "trust_tag": result.trust_tag,
        "imported_count": result.imported.len(),
        "entry_ids": result.imported,
    })))
}

async fn export_for(
    state: &AppState,
    tenant_id: &str,
    dataset_id: &str,
    by: &str,
) -> Result<Json<crate::bundle::DatasetBundle>, ApiError> {
    let ds = state
        .store
        .get_dataset(dataset_id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    // 拉取条件：本租户任意状态 或 public+Published（历史批次对外双条件）
    let pullable = ds.tenant_id == tenant_id || state.store.is_publicly_pullable(dataset_id)?;
    if !pullable {
        return Err(ApiError::forbidden(
            "该数据集不可拉取（非本租户且非 public+Published）",
        ));
    }
    let bundle = state.store.export_bundle(
        dataset_id,
        // T0 决策（2026-08-24）：拉取路径无测试工作台 → 显式 verdict=fail（不默认 Pass），
        // 使 F5 执行侧导入闸门一真实生效（矛盾 B 推荐方案）。
        &crate::bundle::BundleTests::unverified(),
        by,
        &iso_from_unix(unix_now()),
        &state.instance_id,
    )?;
    Ok(Json(bundle))
}

// ----------------------------------------------------------------------
// 治理写通路（知识资产化 A2-3：服务级机器提议入账，经 server service_registry
// 桥消费；宿主唯一律——行为通道以 evorule 引擎为唯一入口，本端点即写端）
// ----------------------------------------------------------------------

/// 服务级 API Key scope：仅限本端点消费（不外溢 dataset 管理/行权面）
pub const API_KEY_SCOPE_PROPOSE: &str = "entries:propose";
/// 服务级机器行权 scope（A2-4 接线）：与 propose 同族的收窄写权——只能把
/// 机器闸六检全过的知识候选放行至 Active，不能入账、不能触 Published。
pub const API_KEY_SCOPE_TRANSITION: &str = "entries:transition";

/// 机器提议入账请求体：entry 与 POST /datasets/{id}/entries 的 knowledge 条目同构。
/// 治理强制项（请求值不可生效）：
/// - `governance` 服务端构造写死 llm_generated.flag=true（旗标不可伪造）；
/// - `trust_level` 强制 `llm`（显式传 human/external:* = 冒充，400 拒绝）；
/// - `cause` 必填，追加进 provenance 审计锚（审计链可回放）。
#[derive(Deserialize)]
pub struct ProposeKnowledgeEntryReq {
    pub dataset_id: String,
    pub entry: AddKnowledgeEntryReq,
    /// 提议事由（审计锚必填）
    pub cause: String,
    /// 来源会话锚（提取通道 A2-1/A2-2 候选落点所在会话，可审计回放）
    #[serde(default)]
    pub source_session_id: Option<String>,
}

/// POST /v1/invoke/propose_knowledge_entry —— 服务级机器提议入账（RuleEngineer 写权映射）：
/// X-Api-Key(scope=entries:propose) 认证（get_bundle pull 先例同构）→ 数据集存在 + 租户
/// 匹配 → trust 冒充拒绝 → governance 强制构造 → provenance cause 锚 → 复用既有闸链
/// （validate_ingest_contract → store.add_knowledge_entry 全闸：D3 领域 schema 强校验 +
/// validate_llm_boundary_gated + 凭据扫描），零新闸零旁路（同一 store 写核收敛）。
/// 入账一律 Draft（llm_generated=true 条目行权封于生命周期迁移闸，A2-4 接线）。
pub async fn propose_knowledge_entry(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ProposeKnowledgeEntryReq>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    // 1) 服务级 X-Api-Key（scope 收敛：仅 entries:propose 可用）
    let k = api_key_from_header(&headers)
        .ok_or_else(|| ApiError::unauthorized("缺少认证：需 X-Api-Key"))?;
    let hash = sha256_hex(k);
    let key = state
        .store
        .get_api_key_by_hash(&hash)?
        .ok_or_else(|| ApiError::unauthorized("API Key 非法"))?;
    if key.revoked_at.is_some() {
        return Err(ApiError::unauthorized("API Key 已吊销"));
    }
    if key.scope != API_KEY_SCOPE_PROPOSE {
        return Err(ApiError::forbidden("API Key 无 entries:propose 权限"));
    }
    // 服务级 key 映射 RuleEngineer 写权（仅本端点消费，key 身份入审计锚）
    let tenant_id = key.tenant_id.clone();
    let actor = format!("apikey:{}", key.key_id);
    // 2) 数据集存在 + 类型匹配 + 租户隔离（同 add_entry 口径）
    let ds = state
        .store
        .get_dataset(&body.dataset_id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    if ds.dataset_kind != DatasetKind::Knowledge {
        return Err(ApiError::bad_request("仅 knowledge 数据集支持提议入账"));
    }
    // 3) trust_level 冒充拒绝（强制 llm；与 external 导入强制打标同纪律：
    //    来源信任级是治理判定不是数据声明，调用方声明不可采信）
    if let Some(t) = &body.entry.trust_level {
        if t != "llm" {
            return Err(ApiError::bad_request(format!(
                "trust_level 冒充拒绝：机器提议通路强制 trust_level=llm，收到 {t:?}"
            )));
        }
    }
    // 4) provenance cause 锚（cause 必填 + 来源会话锚追加，审计回放可读）
    let mut source = body
        .entry
        .provenance
        .as_ref()
        .map(|p| p.source.clone())
        .unwrap_or_else(|| "机器提议（知识资产化提取通道）".into());
    source.push_str(&format!(" | propose_cause: {}", body.cause));
    if let Some(sid) = &body.source_session_id {
        source.push_str(&format!(" | source_session: {sid}"));
    }
    let provenance = match body.entry.provenance {
        Some(mut p) => {
            p.source = source;
            p
        }
        None => Provenance {
            source,
            clause: None,
            document_id: None,
            effective_from: None,
            effective_to: None,
            last_verified: None,
            verified_by: None,
        },
    };
    // 5) governance 强制构造（请求体 governance 值不可生效——旗标由服务端写死）
    let governance = Some(Governance {
        llm_generated: Some(LlmGenerated {
            flag: true,
            model: None,
            op: Some("propose_knowledge_entry".into()),
            timestamp: Some(now_iso()),
        }),
        ..Governance::default()
    });
    // 6) 组装条目（与 add_entry 同构：domain 缺省 + Draft 落账）并复用既有闸链
    let entry = crate::model::knowledge::KnowledgeEntry {
        entry_id: body.entry.entry_id,
        dataset_id: body.dataset_id,
        version: body.entry.version,
        status: Some(LifecycleStatus::Draft),
        provenance,
        domain: body.entry.domain.unwrap_or_else(|| {
            ds.domain
                .first()
                .cloned()
                .unwrap_or_else(|| "general".to_string())
        }),
        tags: body.entry.tags,
        payload: body.entry.payload,
        schema_ref: body.entry.schema_ref,
        governance,
        knowledge_kind: body.entry.knowledge_kind,
        trust_level: Some("llm".to_string()),
        license_ref: body.entry.license_ref,
        execution_contract: body.entry.execution_contract,
    };
    entry
        .validate_ingest_contract()
        .map_err(ApiError::bad_request)?;
    state.store.add_knowledge_entry(&entry)?;
    tracing::info!(
        target: "evorule::invoke",
        "propose_knowledge_entry: dataset={} entry={} v{} by {actor}",
        entry.dataset_id,
        entry.entry_id,
        entry.version
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "status": "proposed",
            "entry_id": entry.entry_id,
            "version": entry.version,
            "lifecycle": "Draft",
        })),
    ))
}

/// POST /v1/invoke/transition_entry 请求体（A2-4 机器行权）
#[derive(Deserialize)]
pub struct TransitionEntryReq {
    pub dataset_id: String,
    pub entry_id: String,
    /// 目标状态：首版仅 `active`（机器闸行权上限=Active，Published 永远人工）
    pub to: String,
    /// 审计 cause（必填，溯源锚——与 propose 同纪律）
    pub cause: String,
}

/// POST /v1/invoke/transition_entry —— 服务级机器行权（A2-4 接线，「机器行权，人工追认」
/// 最小闭环的受端）：X-Api-Key(scope=entries:transition) 认证 → 数据集存在 + 租户匹配 +
/// knowledge 域 → external 来源不给机器闸（同 machine-gate-promote 口径）→ 服务端现场跑
/// 六检执行器（不信任客户端「检查通过」声明）→ 非全过/T2 = 422 附 MachineGateReport 全文
/// （fail-visible，不静默降级，不落状态不落链）；全过按状态机合法路径逐跳机器放行
/// （Draft 起点两跳各自独立落链，gate=machine + tier 审计留痕；T1 进追认队列）。
pub async fn transition_entry(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<TransitionEntryReq>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    // 1) 服务级 X-Api-Key（scope 收敛：仅 entries:transition 可用）
    let k = api_key_from_header(&headers)
        .ok_or_else(|| ApiError::unauthorized("缺少认证：需 X-Api-Key"))?;
    let hash = sha256_hex(&k);
    let key = state
        .store
        .get_api_key_by_hash(&hash)?
        .ok_or_else(|| ApiError::unauthorized("API Key 非法"))?;
    if key.revoked_at.is_some() {
        return Err(ApiError::unauthorized("API Key 已吊销"));
    }
    if key.scope != API_KEY_SCOPE_TRANSITION {
        return Err(ApiError::forbidden("API Key 无 entries:transition 权限"));
    }
    // 服务级 key 映射机器行权身份（仅本端点消费，key 身份入审计锚）
    let tenant_id = key.tenant_id.clone();
    let actor = format!("apikey:{}", key.key_id);
    // 2) 数据集存在 + 类型匹配 + 租户隔离（同 propose 口径）
    let ds = state
        .store
        .get_dataset(&body.dataset_id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    if ds.dataset_kind != DatasetKind::Knowledge {
        return Err(ApiError::bad_request("仅 knowledge 数据集支持机器行权"));
    }
    // 3) 目标状态：首版仅 Active（机器闸行权上限=Active，结构性立宪不松动）
    if body.to != "active" {
        return Err(ApiError::bad_request(format!(
            "to `{}` 非法（首版仅 `active`；机器闸行权上限=Active，Published 永远人工）",
            body.to
        )));
    }
    if body.cause.trim().is_empty() {
        return Err(ApiError::bad_request("cause 必填（溯源锚）"));
    }
    // 4) 条目在场 + external 来源不给机器闸（来源不可信者仅可人工审批放行）
    let entry = state
        .store
        .get_latest_knowledge_entry(&body.dataset_id, &body.entry_id)?
        .ok_or_else(|| ApiError::not_found("条目不存在"))?;
    if entry
        .trust_level
        .as_deref()
        .is_some_and(|t| t.starts_with("external:"))
    {
        return Err(ApiError::forbidden(
            "external 来源条目不给机器闸：来源不可信者仅可人工审批放行",
        ));
    }
    let current = entry.status.unwrap_or(LifecycleStatus::Draft);
    // 5) 现场跑六检执行器（同 machine-gate-promote：store 探针采集事实，纯函数出报告）
    let report = crate::api::handlers_entries::run_machine_gate(
        &state,
        &body.dataset_id,
        crate::store::AnyEntry::Knowledge(entry),
    )?;
    if !report.all_passed() || report.tier == "T2" {
        return Err(ApiError::unprocessable_entity(format!(
            "机器闸未放行（{}）: {}",
            report.tier,
            report.summary()
        )));
    }
    // 6) 跳数规划（状态机合法路径 Draft→Candidate→Active；首版语义=把候选推到 Active）
    let hops: Vec<LifecycleStatus> = match current {
        LifecycleStatus::Candidate => vec![LifecycleStatus::Active],
        LifecycleStatus::Draft => vec![LifecycleStatus::Candidate, LifecycleStatus::Active],
        cur => {
            return Err(ApiError::bad_request(format!(
                "当前状态 {cur:?} 无法经机器闸行权至 Active（合法路径 Draft→Candidate→Active；Published/Rejected 走人工流程）"
            )))
        }
    };
    let cause = format!(
        "{} | machine-gate {} | actor {}",
        body.cause,
        report.summary(),
        actor
    );
    let at = now_iso();
    for hop in &hops {
        state.store.transition_knowledge_entry_status_machine(
            &body.dataset_id,
            &body.entry_id,
            *hop,
            &actor,
            &at,
            &cause,
            &report.tier,
        )?;
    }
    tracing::info!(
        target: "evorule::invoke",
        "transition_entry: dataset={} entry={} to=Active tier={} by {actor}",
        body.dataset_id,
        body.entry_id,
        report.tier
    );
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "transitioned",
            "entry_id": body.entry_id,
            "lifecycle": "Active",
            "tier": report.tier,
            "post_review_required": report.tier == "T1",
            "report": serde_json::to_value(&report).unwrap_or(Value::Null),
        })),
    ))
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}
