//! 快照包端点（44 号 §6 bundles/；36 号 集成契约）
//!
//! - 导出：按版本导出（MVP 仅当前版本有内容，历史版本显式拒绝不伪造）+ 裁剪视图导出（36 号 §5）；
//! - 导入：5 步校验链（schema→防篡改→符号三方一致→版本解析→闸门一），硬失败不静默（35 号 §9）；
//! - 导入预检（dry-run，不落库）+ 导入状态（MVP 同步，状态端点如实标注）。

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::api::{unix_now, AppState, ApiError, AuthContext};
use crate::auth::iso_from_unix;
use crate::bundle::{BundleImporter, BundleTests, DatasetBundle};
use crate::model::auth::Role;
use crate::store::StoreError;

#[derive(Deserialize)]
pub struct ExportQuery {
    /// 裁剪视图语法：`tag:core` / `domain:tax`（36 号 §5；不新造版本链）
    #[serde(default)]
    pub subset: Option<String>,
}

/// GET /bundles/datasets/{id}/versions/{ver}[?subset=...]
///
/// - 无 subset：按版本导出快照包；
/// - subset=tag:core：裁剪视图导出（仅保留命中条目 + 依赖收缩，view_of 指向原版本）。
///
/// MVP 仅存当前版本条目内容：`ver != current` 显式拒绝，不伪造历史内容。
pub async fn export_version(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((id, ver)): Path<(String, String)>,
    Query(query): Query<ExportQuery>,
) -> Result<Json<DatasetBundle>, ApiError> {
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
    if ds.versioning.current != ver {
        return Err(ApiError::bad_request(format!(
            "MVP 仅存当前版本 `{}` 条目内容，无法导出历史版本 `{ver}`（批次 1 快照落库后支持）",
            ds.versioning.current
        )));
    }
    let bundle = state.store.export_bundle(
        &id,
        &BundleTests::default(),
        &ctx.user_id,
        &iso_from_unix(unix_now()),
        &state.instance_id,
    )?;
    match query.subset {
        Some(spec) => trim(&bundle, &spec, &ctx.user_id),
        None => Ok(Json(bundle)),
    }
}

/// 裁剪视图：解析 `tag:core` / `domain:tax` 语法
fn trim(
    bundle: &DatasetBundle,
    spec: &str,
    by: &str,
) -> Result<Json<DatasetBundle>, ApiError> {
    let (kind, value) = spec
        .split_once(':')
        .ok_or_else(|| ApiError::bad_request("subset 语法须为 tag:xxx 或 domain:xxx"))?;
    let at = iso_from_unix(unix_now());
    let view = match kind {
        "tag" => crate::bundle::BundleTrimmer::trim_by_filter(bundle, None, &[value], by, &at),
        "domain" => crate::bundle::BundleTrimmer::trim_by_filter(bundle, Some(value), &[], by, &at),
        other => {
            return Err(ApiError::bad_request(format!(
                "不支持的裁剪维度 `{other}`（tag|domain）"
            )))
        }
    }
    .map_err(|e| ApiError::bad_request(format!("裁剪失败: {e}")))?;
    Ok(Json(view))
}

#[derive(Deserialize)]
pub struct ImportReq {
    pub bundle: DatasetBundle,
}

/// POST /bundles/import —— 导入快照包（36 号 5 步校验链；admin）
///
/// 校验失败 → 400 显式错误（不静默降级）；成功 → 同步返回导入结果。
pub async fn import_bundle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<ImportReq>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if ctx.role != Role::Admin {
        return Err(ApiError::forbidden("导入快照包需管理员角色"));
    }
    let at = iso_from_unix(unix_now());
    let result = state
        .store
        .import_bundle(&req.bundle, &ctx.tenant_id, &ctx.user_id, &at, &state.instance_id)
        .map_err(import_err)?;
    // 激活版本 = 导入后数据集的当前版本（bundle 版本链）
    let activated = state
        .store
        .list_dataset_versions(&req.bundle.dataset.dataset_id)
        .map(|v| v.current)
        .unwrap_or_else(|_| result.source_version.clone());
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "import_id": format!("imp-{}", unix_now()),
            "status": "imported",
            "bundle_id": result.bundle_id,
            "dataset_id": result.dataset_id,
            "activated_version": activated,
            "entry_count": result.entry_count,
            // 35 号 §9 硬失败：缺失服务已在校验链以显式错误拦截，成功导入即无缺失
            "missing_services": [],
        })),
    ))
}

/// POST /bundles/import/dry-run —— 导入预检（校验链全跑，不落库）
pub async fn import_dry_run(
    State(_state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<ImportReq>,
) -> Result<Json<Value>, ApiError> {
    if ctx.role != Role::Admin {
        return Err(ApiError::forbidden("导入预检需管理员角色"));
    }
    let result = BundleImporter::validate(&req.bundle).map_err(|e| {
        ApiError::bad_request(format!("导入预检未通过（不静默）: {e}"))
    })?;
    Ok(Json(serde_json::json!({
        "valid": true,
        "bundle_id": result.bundle_id,
        "dataset_id": result.dataset_id,
        "source_version": result.source_version,
        "selection_mode": format!("{:?}", result.selection_mode),
        "resolved_version": result.resolved_version,
        "entry_count": result.entry_count,
        "verdict": format!("{:?}", result.verdict),
        "missing_services": [],
    })))
}

/// GET /bundles/{bundle_id}/status —— 导入状态
///
/// MVP 为同步导入（结果在 POST /bundles/import 响应内联返回），不保留异步任务记录；
/// 本端点如实标注契约，不伪造状态。
pub async fn import_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(bundle_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let _ = (&state, &ctx);
    Ok(Json(serde_json::json!({
        "bundle_id": bundle_id,
        "status": "unknown_in_mvp",
        "note": "MVP 为同步导入：导入结果在 POST /bundles/import 响应中内联返回，不保留异步任务记录；异步状态语义待批次 1 引入任务队列后实现",
    })))
}

/// 导入错误映射：校验链失败 = 客户端 400（显式、不静默），其余走统一映射
fn import_err(e: StoreError) -> ApiError {
    match e {
        StoreError::Bundle(b) => ApiError::bad_request(format!("导入校验失败（不静默降级）: {b}")),
        other => ApiError::from(other),
    }
}
