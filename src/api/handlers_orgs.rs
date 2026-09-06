//! 组织管理端点（数据治理攻坚 B1 双层租户，2026-08-31 用户裁定）
//!
//! 平台层：POST/GET /v1/orgs（仅 PlatformAdmin）；
//! 成员层：POST/GET /v1/orgs/{org_id}/members（PlatformAdmin 或该 org 的 admin）。
//! 语义说明：wire 字段沿用 `tenant_id`（= org id，零迁移）；成员指派入 auth 审计
//! （action=assign_role，「授权变更入审计链」同口径）。

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, AppState, AuthContext};
use crate::model::auth::{can, is_org_admin, Action, Org, Role, UserOrg};

#[derive(Deserialize)]
pub struct CreateOrgReq {
    pub org_id: String,
    pub name: String,
}

#[derive(Deserialize)]
pub struct AddMemberReq {
    pub user_id: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct MemberResp {
    pub org_id: String,
    pub user_id: String,
    pub role: String,
    pub created_at: String,
}

/// 创建组织（仅平台管理员）
pub async fn create_org(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateOrgReq>,
) -> Result<(StatusCode, Json<Org>), ApiError> {
    if !can(ctx.role, Action::ManageOrgs) {
        return Err(ApiError::forbidden("仅平台管理员可创建组织"));
    }
    let org_id = req.org_id.trim().to_string();
    if org_id.is_empty() || req.name.trim().is_empty() {
        return Err(ApiError::bad_request("org_id 与 name 均不可为空"));
    }
    let org = Org {
        org_id: org_id.clone(),
        name: req.name.trim().to_string(),
        disabled: false,
        created_at: crate::api::handlers_auth::now_iso(),
    };
    // 双层落地一致性：orgs 与 tenants 必须同建——登录前置校验查 tenants 表
    // （缺 tenants 行 → TenantNotFound → 401"用户名或密码错误"，平台创建的组织
    // 永远无法登录；总验收 E2E 实测发现的 B1 缺陷，2026-08-31 修复）
    state.store.ensure_default_tenant(
        &org.org_id,
        &org.name,
        &state.instance_id,
        &org.created_at,
    )?;
    state.store.create_org(&org)?;
    state.auth.record_audit(
        &state.store,
        &ctx.tenant_id,
        Some(&ctx.user_id),
        "create_org",
        "success",
        Some(&format!("org_id={}", org.org_id)),
        crate::api::handlers_auth::now_iso(),
    );
    Ok((StatusCode::CREATED, Json(org)))
}

/// 组织列表：平台管理员看全部；其他用户看自己的成员关系所在 org
pub async fn list_orgs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<Org>>, ApiError> {
    if can(ctx.role, Action::ManageOrgs) {
        return Ok(Json(state.store.list_orgs()?));
    }
    let mine = state.store.list_user_orgs(&ctx.user_id)?;
    let mut orgs = Vec::with_capacity(mine.len());
    for m in mine {
        if let Some(o) = state.store.get_org(&m.org_id)? {
            orgs.push(o);
        }
    }
    Ok(Json(orgs))
}

/// 成员指派（平台管理员，或该 org 的管理层）
pub async fn add_member(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(org_id): Path<String>,
    Json(req): Json<AddMemberReq>,
) -> Result<(StatusCode, Json<MemberResp>), ApiError> {
    authorize_member_admin(&state, &ctx, &org_id).await?;
    let role = Role::parse(&req.role)
        .ok_or_else(|| ApiError::bad_request(format!("角色 `{}` 非法", req.role)))?;
    let created_at = crate::api::handlers_auth::now_iso();
    state
        .store
        .upsert_user_org_role(&org_id, &req.user_id, role, &created_at)?;
    // 授权变更入审计（同口径）
    state.auth.record_audit(
        &state.store,
        &org_id,
        Some(&ctx.user_id),
        "assign_role",
        "success",
        Some(&format!("target_user={} role={}", req.user_id, req.role)),
        created_at.clone(),
    );
    Ok((
        StatusCode::CREATED,
        Json(MemberResp {
            org_id,
            user_id: req.user_id,
            role: role.as_str().to_string(),
            created_at,
        }),
    ))
}

/// 成员列表（平台管理员，或该 org 的管理层）
pub async fn list_members(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(org_id): Path<String>,
) -> Result<Json<Vec<MemberResp>>, ApiError> {
    authorize_member_admin(&state, &ctx, &org_id).await?;
    let members = state.store.list_user_orgs_in_org(&org_id)?;
    Ok(Json(
        members
            .into_iter()
            .map(|m: UserOrg| MemberResp {
                org_id: m.org_id,
                user_id: m.user_id,
                role: m.role.as_str().to_string(),
                created_at: m.created_at,
            })
            .collect(),
    ))
}

/// 成员管理门控：平台管理员，或登录 org 即目标 org 且具备管理层角色
async fn authorize_member_admin(
    _state: &AppState,
    ctx: &AuthContext,
    org_id: &str,
) -> Result<(), ApiError> {
    let platform = can(ctx.role, Action::ManageOrgs);
    let own_org_admin = ctx.tenant_id == org_id && is_org_admin(ctx.role);
    if !platform && !own_org_admin {
        return Err(ApiError::forbidden("仅平台管理员或该组织管理层可管理成员"));
    }
    Ok(())
}
