//! 数据依赖端点（44 号 §7 deps/；35 号 数据源绑定）
//!
//! 三层模型（35 号 §3）：
//! 1. 层 1 数据集级 `data_dependencies` 声明（GET/PUT `/deps/datasets/{id}`，alias `/datasets/{id}/deps`）；
//! 2. 无凭据服务模板注册（35 号 §5：端点形状 + 占位符，不含真实端点/密钥）；
//! 3. 层 2 绑定动作 `bind`（占位符填充 → 消费者可填写的 ServiceTemplate）。
//!
//! 凭据强约束：模板不存真实端点/密钥（35 号 §6 强约束，只走执行侧密钥管理）。

use std::collections::BTreeMap;

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::api::handlers_auth::now_iso;
use crate::api::{paginate, unix_now, AppState, ApiError, AuthContext, Page, PageQuery};
use crate::model::auth::{Action, can, is_org_admin};
use crate::model::dependency::{
    DataDependencies, IoContract, ServiceTemplate, ServiceTemplateRecord,
};

/// GET /deps/datasets/{id}（alias GET /datasets/{id}/deps）—— 数据集级数据依赖声明
pub async fn get_dataset_deps(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<DataDependencies>, ApiError> {
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
    Ok(Json(ds.data_dependencies.unwrap_or_default()))
}

/// PUT /deps/datasets/{id} —— 更新数据集级 data_dependencies（engineer）
pub async fn put_dataset_deps(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(deps): Json<DataDependencies>,
) -> Result<Json<DataDependencies>, ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    // C1（02 方案层1）：依赖声明的服务名必须已注册在服务目录（本租户可见范围）——
    // 把「服务名错误」从导入期提前到声明期（事前预检，不静默）。
    let catalog: std::collections::HashSet<String> = state
        .store
        .list_services(&ctx.tenant_id)?
        .into_iter()
        .map(|e| e.service_name)
        .collect();
    let unknown: Vec<&str> = deps
        .services
        .iter()
        .map(|s| s.service_name.as_str())
        .filter(|name| !catalog.contains(*name))
        .collect();
    if !unknown.is_empty() {
        return Err(ApiError::bad_request(format!(
            "依赖声明包含未在服务目录注册的服务（不静默）: {}；请先 POST /v1/services 注册后再声明",
            unknown.join(", ")
        )));
    }
    state.store.update_dataset_deps(&id, &deps, &ctx.user_id, &now_iso())?;
    Ok(Json(deps))
}

#[derive(Deserialize)]
pub struct CreateTemplateReq {
    pub service_name: String,
    /// pull | push
    pub kind: String,
    #[serde(default)]
    pub io_contract: IoContract,
    /// 允许占位符（如 "http://{host}:{port}/api/payroll"）
    pub endpoint_template: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers_template: BTreeMap<String, String>,
    #[serde(default)]
    pub placeholder_notes: BTreeMap<String, String>,
}

/// POST /deps/templates —— 注册无凭据服务模板（admin）
pub async fn create_template(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateTemplateReq>,
) -> Result<(StatusCode, Json<ServiceTemplateRecord>), ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("注册服务模板需管理员角色"));
    }
    if req.kind != "pull" && req.kind != "push" {
        return Err(ApiError::bad_request("kind 仅支持 pull|push"));
    }
    if !req.endpoint_template.contains('{') {
        return Err(ApiError::bad_request(
            "endpoint_template 应含占位符（如 {host}）；真实端点/密钥不得入库（35 号 §6）",
        ));
    }
    let record = ServiceTemplateRecord {
        template_id: format!("tpl-{}-{}", req.service_name, unix_now()),
        tenant_id: ctx.tenant_id.clone(),
        service_name: req.service_name,
        kind: req.kind,
        io_contract: req.io_contract,
        endpoint_template: req.endpoint_template,
        method: req.method,
        headers_template: req.headers_template,
        placeholder_notes: req.placeholder_notes,
        created_at: now_iso(),
        created_by: ctx.user_id.clone(),
    };
    state.store.create_service_template(&record)?;
    Ok((StatusCode::CREATED, Json(record)))
}

/// GET /deps/templates —— 服务模板列表（租户作用域）
pub async fn list_templates(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(page): Query<PageQuery>,
) -> Result<Json<Page<ServiceTemplateRecord>>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let t = state.store.list_service_templates(&ctx.tenant_id)?;
    Ok(paginate(t, page.limit, page.offset))
}

/// GET /deps/templates/{id} —— 模板详情（含参数形状 placeholder_notes）
pub async fn get_template(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(template_id): Path<String>,
) -> Result<Json<ServiceTemplateRecord>, ApiError> {
    if !can(ctx.role, Action::View) {
        return Err(ApiError::forbidden("无查看权限"));
    }
    let tpl = state
        .store
        .get_service_template(&template_id)?
        .ok_or_else(|| ApiError::not_found(format!("服务模板 `{template_id}` 不存在")))?;
    if tpl.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found(format!("服务模板 `{template_id}` 不存在")));
    }
    Ok(Json(tpl))
}

#[derive(Deserialize)]
pub struct BindTemplateReq {
    /// 占位符值：{host} -> "10.0.0.5" 等；未提供的占位符保留原样（由执行侧填写）
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

/// POST /deps/templates/{id}/bind —— 应用模板生成绑定（占位符填充，engineer）
///
/// 返回可交给执行侧 service_registry 填写的 ServiceTemplate（无凭据，35 号 §5）。
pub async fn bind_template(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(template_id): Path<String>,
    Json(req): Json<BindTemplateReq>,
) -> Result<Json<ServiceTemplate>, ApiError> {
    if !can(ctx.role, Action::Edit) {
        return Err(ApiError::forbidden("需要规则工程师及以上角色"));
    }
    let tpl = state
        .store
        .get_service_template(&template_id)?
        .ok_or_else(|| ApiError::not_found(format!("服务模板 `{template_id}` 不存在")))?;
    if tpl.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found(format!("服务模板 `{template_id}` 不存在")));
    }
    Ok(Json(tpl.bind(&req.values)))
}
