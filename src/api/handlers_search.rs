//! 检索端点（44 号 §9 search/；31 号 §7 检索能力 / 33 号 版本 diff）
//!
//! 租户作用域（38 号 §3）：private 仅当前租户；public+Published 对所有人可检索（双条件，34 号 §3）。

use axum::extract::{Extension, Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::api::{paginate, ApiError, AppState, AuthContext, Page};
use crate::model::auth::{can, Action};
use crate::model::dataset::{RuleDataset, Visibility};
use crate::model::entry::RuleEntry;
use crate::model::lifecycle::LifecycleStatus;

#[derive(Deserialize)]
pub struct SearchDatasetsQuery {
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    /// 逗号分隔标签
    #[serde(default)]
    pub tags: Option<String>,
    #[serde(default)]
    pub effective_from_after: Option<String>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

/// GET /search/datasets —— 数据集检索（域/关键词/标签/生效日期/可见性组合）
pub async fn search_datasets(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(query): Query<SearchDatasetsQuery>,
) -> Result<Json<Page<RuleDataset>>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let tags: Vec<String> = query
        .tags
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let out = state.store.search_datasets(
        &ctx.tenant_id,
        query.domain.as_deref(),
        query.q.as_deref(),
        &tags,
        query.effective_from_after.as_deref(),
        query.visibility,
    )?;
    Ok(paginate(out, query.limit, query.offset))
}

#[derive(Deserialize)]
pub struct SearchEntriesQuery {
    #[serde(default)]
    pub dataset_id: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub tags: Option<String>,
    /// PascalCase：Draft/Candidate/Active/Published/Rejected
    #[serde(default)]
    pub status: Option<LifecycleStatus>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

/// GET /search/entries —— 条目检索（租户作用域）
pub async fn search_entries(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(query): Query<SearchEntriesQuery>,
) -> Result<Json<Page<RuleEntry>>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let tags: Vec<String> = query
        .tags
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let out = state.store.search_entries(
        &ctx.tenant_id,
        query.dataset_id.as_deref(),
        query.domain.as_deref(),
        query.q.as_deref(),
        &tags,
        query.status,
    )?;
    Ok(paginate(out, query.limit, query.offset))
}

#[derive(Deserialize)]
pub struct DiffQuery {
    pub from: String,
    pub to: String,
}

/// GET /search/datasets/{id}/diff?from=v1&to=v2 —— 版本 diff（33 号内容哈希）
///
/// MVP 返回结构级 diff（版本链增量 + 当前条目清单），诚实标注（批次 1 补内容级）。
pub async fn version_diff(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(query): Query<DiffQuery>,
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
    let out = state.store.version_diff(&id, &query.from, &query.to)?;
    Ok(Json(out))
}
