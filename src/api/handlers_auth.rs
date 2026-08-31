//! 认证端点（44 号 §7）：register / login / refresh / me / audits

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::{AppState, AuthContext, ApiError, unix_now};
use crate::auth::iso_from_unix;
use crate::model::auth::{Role, is_org_admin};

#[derive(Deserialize)]
pub struct RegisterReq {
    pub tenant_id: String,
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub tenant_id: String,
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RefreshReq {
    pub tenant_id: String,
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct LogoutReq {
    /// 要吊销的 refresh token（登出后至 exp 拉黑，防旋转续用）
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct TokenResp {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at: i64,
    pub refresh_expires_at: i64,
    pub token_type: &'static str,
}

#[derive(Serialize)]
pub struct MeResp {
    pub user_id: String,
    pub username: String,
    pub tenant_id: String,
    pub role: String,
}

/// 注册（MVP 安全边界：公共注册固定 rule_engineer，不可注册 admin/approver）
pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterReq>,
) -> Result<(StatusCode, Json<TokenResp>), ApiError> {
    let now = unix_now();
    let user = state
        .auth
        .register(&state.store, &req.tenant_id, &req.username, &req.password, Role::RuleEngineer, now)
        .map_err(|e| match e {
            crate::auth::AuthError::UsernameTaken => {
                ApiError::conflict("用户名已存在")
            }
            crate::auth::AuthError::TenantNotFound => {
                ApiError::bad_request("租户不存在")
            }
            crate::auth::AuthError::OrgNotFound => {
                ApiError::bad_request("组织不存在")
            }
            crate::auth::AuthError::InvalidCredentials => {
                ApiError::bad_request("密码至少 8 位")
            }
            other => ApiError::internal(other.to_string()),
        })?;

    let tokens = state
        .auth
        .login(&state.store, &req.tenant_id, &req.username, &req.password, now)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let _ = user;
    Ok((
        StatusCode::CREATED,
        Json(TokenResp {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            access_expires_at: tokens.access_expires_at,
            refresh_expires_at: tokens.refresh_expires_at,
            token_type: "Bearer",
        }),
    ))
}

/// 登录
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> Result<Json<TokenResp>, ApiError> {
    let tokens = state
        .auth
        .login(&state.store, &req.tenant_id, &req.username, &req.password, unix_now())
        .map_err(|e| match e {
            crate::auth::AuthError::OrgDisabled => {
                ApiError::forbidden("组织已停用，禁止登录")
            }
            crate::auth::AuthError::OrgNotFound => {
                ApiError::forbidden("组织不存在")
            }
            crate::auth::AuthError::NotOrgMember => {
                ApiError::forbidden("该账号不是此组织成员")
            }
            _ => ApiError::unauthorized("用户名或密码错误"),
        })?;
    Ok(Json(TokenResp {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        access_expires_at: tokens.access_expires_at,
        refresh_expires_at: tokens.refresh_expires_at,
        token_type: "Bearer",
    }))
}

/// 刷新（旋转）
pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshReq>,
) -> Result<Json<TokenResp>, ApiError> {
    let tokens = state
        .auth
        .refresh(&state.store, &req.tenant_id, &req.refresh_token, unix_now())
        .map_err(|e| match e {
            crate::auth::AuthError::NotOrgMember => {
                ApiError::forbidden("该账号已不是此组织成员")
            }
            crate::auth::AuthError::OrgDisabled => {
                ApiError::forbidden("组织已停用")
            }
            _ => ApiError::unauthorized("刷新 token 非法或已过期"),
        })?;
    Ok(Json(TokenResp {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        access_expires_at: tokens.access_expires_at,
        refresh_expires_at: tokens.refresh_expires_at,
        token_type: "Bearer",
    }))
}

/// 登出（43 号 §3.3）：吊销 refresh token（拉黑至 exp），其后用该 token 刷新将失败
pub async fn logout(
    State(state): State<AppState>,
    Json(req): Json<LogoutReq>,
) -> Result<StatusCode, ApiError> {
    state
        .auth
        .logout(&state.store, &req.refresh_token, unix_now())
        .map_err(|_| ApiError::bad_request("refresh token 非法或已过期"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// 当前用户信息
pub async fn me(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<MeResp>, ApiError> {
    let user = state
        .store
        .get_user(&ctx.user_id)?
        .ok_or_else(|| ApiError::unauthorized("用户不存在"))?;
    Ok(Json(MeResp {
        user_id: user.user_id,
        username: user.username,
        tenant_id: user.tenant_id,
        role: user.role.as_str().to_string(),
    }))
}

/// 认证审计列表（仅管理员，43 号 §6）
pub async fn audits(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<crate::model::auth::AuthAudit>>, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("仅管理员可查看认证审计"));
    }
    let audits = state.store.list_auth_audits(&ctx.tenant_id, 100)?;
    Ok(Json(audits))
}

/// 生命周期审计列表（44 号 §11 `GET /audits/lifecycle`，admin；34 号 state_history 扁平输出）
pub async fn lifecycle_audits(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("仅管理员可查看生命周期审计"));
    }
    let audits = state.store.list_lifecycle_audits(&ctx.tenant_id)?;
    Ok(Json(audits))
}

/// 时间辅助（供其他 handler 复用统一时区语义）
pub fn now_iso() -> String {
    iso_from_unix(unix_now())
}
