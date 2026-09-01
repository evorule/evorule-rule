//! 服务目录端点（02 方案 C2：服务名/契约治理侧 SSOT）
//!
//! - `GET /services`：列表（平台官方 scope=platform + 本租户自定义）
//! - `POST /services`：注册服务（admin；服务由服务公司/官方维护，D13）
//! - `GET /services/{name}`：详情
//! - `PUT /services/{name}`：更新（admin）
//!
//! 边界：目录存「服务描述」不存端点/凭据（凭据永不入库，35 号 §6）。

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::api::handlers_auth::now_iso;
use crate::api::{ApiError, AppState, AuthContext};
use crate::model::auth::{can, is_org_admin, Action};
use crate::model::dependency::IoContract;
use crate::model::service_catalog::{BindingHint, ServiceCatalogEntry};

fn default_version() -> String {
    "1.0.0".into()
}

#[derive(Deserialize)]
pub struct CreateServiceReq {
    pub service_name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub io_contract: Option<IoContract>,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub binding_hint: BindingHint,
}

/// GET /services —— 服务目录列表（平台官方 + 本租户）
pub async fn list_services(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<ServiceCatalogEntry>>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    Ok(Json(state.store.list_services(&ctx.tenant_id)?))
}

/// POST /services —— 注册服务（admin；服务公司/官方维护，D13）
pub async fn create_service(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateServiceReq>,
) -> Result<(StatusCode, Json<ServiceCatalogEntry>), ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden(
            "注册服务需管理员角色（服务由服务公司/官方维护，D13）",
        ));
    }
    let now = now_iso();
    let entry = ServiceCatalogEntry {
        service_name: req.service_name,
        version: req.version,
        description: req.description,
        io_contract: req.io_contract,
        sensitive: req.sensitive,
        binding_hint: req.binding_hint,
        managed_by: format!("org:{}", ctx.tenant_id),
        scope: format!("tenant:{}", ctx.tenant_id),
        created_at: now,
        updated_at: None,
    };
    state.store.upsert_service(&entry)?;
    Ok((StatusCode::CREATED, Json(entry)))
}

/// GET /services/{name} —— 服务详情
pub async fn get_service(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(name): Path<String>,
) -> Result<Json<ServiceCatalogEntry>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let e = state
        .store
        .get_service(&name)?
        .ok_or_else(|| ApiError::not_found(format!("服务 `{name}` 不在目录中")))?;
    // 仅平台或本租户可见
    if e.scope != "platform" && e.scope != format!("tenant:{}", ctx.tenant_id) {
        return Err(ApiError::not_found(format!("服务 `{name}` 不在目录中")));
    }
    Ok(Json(e))
}

/// PUT /services/{name} —— 更新服务（admin）
pub async fn update_service(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(name): Path<String>,
    Json(req): Json<CreateServiceReq>,
) -> Result<Json<ServiceCatalogEntry>, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("更新服务需管理员角色"));
    }
    let existing = state
        .store
        .get_service(&name)?
        .ok_or_else(|| ApiError::not_found(format!("服务 `{name}` 不在目录中")))?;
    if existing.scope != "platform" && existing.scope != format!("tenant:{}", ctx.tenant_id) {
        return Err(ApiError::not_found(format!("服务 `{name}` 不在目录中")));
    }
    let now = now_iso();
    let updated = ServiceCatalogEntry {
        service_name: name,
        version: req.version,
        description: req.description,
        io_contract: req.io_contract,
        sensitive: req.sensitive,
        binding_hint: req.binding_hint,
        managed_by: existing.managed_by,
        scope: existing.scope,
        created_at: existing.created_at,
        updated_at: Some(now),
    };
    state.store.upsert_service(&updated)?;
    Ok(Json(updated))
}
