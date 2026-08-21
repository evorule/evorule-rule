//! 数据集 / 条目 / 生命周期 / 快照包端点（44 号 §8-§11）

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::api::handlers_auth::now_iso;
use crate::api::{api_key_from_header, bearer_token, unix_now, AppState, ApiError, AuthContext};
use crate::auth::iso_from_unix;
use crate::model::auth::{Action, Role, can};
use crate::model::dataset::{Meta, RuleDataset, Visibility};
use crate::model::entry::RuleEntry;
use crate::model::lifecycle::LifecycleStatus;
use crate::model::provenance::Provenance;

// ----------------------------------------------------------------------
// 数据集
// ----------------------------------------------------------------------

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
}

pub async fn list_datasets(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<RuleDataset>>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let ds = state.store.list_datasets(&ctx.tenant_id)?;
    Ok(Json(ds))
}

pub async fn create_dataset(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateDatasetReq>,
) -> Result<(StatusCode, Json<RuleDataset>), ApiError> {
    if !can(ctx.role, Action::Create) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let now = now_iso();
    let ds = RuleDataset {
        dataset_id: req.dataset_id,
        name: req.name,
        description: req.description,
        domain: req.domain,
        tags: req.tags,
        tenant_id: ctx.tenant_id.clone(),
        visibility: req.visibility.unwrap_or(Visibility::Private),
        lifecycle: crate::model::lifecycle::Lifecycle::default(),
        versioning: crate::model::version::Versioning::default(),
        law_ref: None,
        version_selection: None,
        data_dependencies: None,
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
    // 数据隔离（⑧）：仅本租户可见（public 拉取走 bundle 端点，对外双条件）
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    Ok(Json(ds))
}

// ----------------------------------------------------------------------
// 生命周期迁移（44 号 §13-3：统一 PATCH /lifecycle）+ 独立发布
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
        "published" => return Err(ApiError::bad_request("Published 必须走独立发布审批端点 POST /publish")),
        other => return Err(ApiError::bad_request(format!("非法目标状态: {other}"))),
    };
    // 状态迁移权限（34 号 §9 定案：闸门/审批/撤销需对应角色）
    let allowed = match to {
        LifecycleStatus::Candidate => can(ctx.role, Action::Create),
        LifecycleStatus::Active => can(ctx.role, Action::Approve),
        LifecycleStatus::Rejected => ctx.role == Role::Admin,
        _ => false,
    };
    if !allowed {
        return Err(ApiError::forbidden("当前角色无权执行该状态迁移"));
    }
    let at = iso_from_unix(unix_now());
    state
        .store
        .transition_dataset_status(&id, to, &ctx.user_id, &format!("API 迁移 to {to:?}"), &at)?;
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    Ok(Json(ds))
}

/// 独立发布审批（34 号 §3 强约束）：Active → Published，发布者复用审批者 + 二次确认
pub async fn publish(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<RuleDataset>, ApiError> {
    if !can(ctx.role, Action::Publish) {
        return Err(ApiError::forbidden("发布需审批者及以上角色"));
    }
    let at = iso_from_unix(unix_now());
    state
        .store
        .publish_dataset(&id, &ctx.user_id, &at, &state.instance_id)?;
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

pub async fn list_entries(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Vec<RuleEntry>>, ApiError> {
    let ds = state
        .store
        .get_dataset(&id)?
        .ok_or_else(|| ApiError::not_found("数据集不存在"))?;
    if ds.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found("数据集不存在"));
    }
    let entries = state.store.list_entries(&id, None)?;
    Ok(Json(entries))
}

pub async fn add_entry(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<AddEntryReq>,
) -> Result<(StatusCode, Json<RuleEntry>), ApiError> {
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
    Ok((StatusCode::CREATED, Json(entry)))
}

// ----------------------------------------------------------------------
// 快照包（36 号：导出交付；支持 JWT 或 X-Api-Key(pull)）
// ----------------------------------------------------------------------

/// 快照包拉取：优先 Bearer（登录用户，本租户或 public+Published），否则 X-Api-Key(pull scope)
pub async fn get_bundle(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<crate::bundle::DatasetBundle>, ApiError> {
    // 1) X-Api-Key（执行侧联动，44 号 §14：pull scope）
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
    Err(ApiError::unauthorized("缺少认证：需 Bearer token 或 X-Api-Key"))
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
    // 拉取条件：本租户任意状态 或 public+Published（34 号对外双条件）
    let pullable = ds.tenant_id == tenant_id || state.store.is_publicly_pullable(dataset_id)?;
    if !pullable {
        return Err(ApiError::forbidden("该数据集不可拉取（非本租户且非 public+Published）"));
    }
    let bundle = state
        .store
        .export_bundle(
            dataset_id,
            &crate::bundle::BundleTests::default(),
            by,
            &iso_from_unix(unix_now()),
            &state.instance_id,
        )?;
    Ok(Json(bundle))
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}
