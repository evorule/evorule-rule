//! API Key 端点（44 号 §14：MVP 最小 scope 版，执行侧拉取快照包联动）

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::api::handlers_auth::now_iso;
use crate::api::{paginate, ApiError, AppState, AuthContext, Page, PageQuery};
use crate::model::auth::{is_org_admin, ApiKey};

#[derive(Deserialize)]
pub struct CreateKeyReq {
    pub name: String,
    /// MVP 仅支持 pull（执行侧拉取快照包）
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Serialize)]
pub struct CreateKeyResp {
    pub key_id: String,
    pub name: String,
    pub scope: String,
    /// 明文 key：仅创建时返回一次，之后只存哈希
    pub key: String,
    pub created_at: String,
}

/// 管理员生成 API Key
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateKeyReq>,
) -> Result<(StatusCode, Json<CreateKeyResp>), ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("仅管理员可创建 API Key"));
    }
    let scope = req.scope.unwrap_or_else(|| "pull".to_string());
    if scope != "pull" {
        return Err(ApiError::bad_request("MVP 仅支持 scope=pull"));
    }
    let (key_id, plain) = generate_key();
    let key = ApiKey {
        key_id: key_id.clone(),
        tenant_id: ctx.tenant_id.clone(),
        name: req.name,
        scope: scope.clone(),
        key_hash: sha256_hex(&plain),
        created_at: now_iso(),
        revoked_at: None,
    };
    state.store.create_api_key(&key)?;
    Ok((
        StatusCode::CREATED,
        Json(CreateKeyResp {
            key_id,
            name: key.name,
            scope,
            key: plain,
            created_at: key.created_at,
        }),
    ))
}

#[derive(Serialize)]
pub struct KeySummary {
    pub key_id: String,
    pub name: String,
    pub scope: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

/// 管理员列出 API Key（不含哈希与明文）
pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(page): Query<PageQuery>,
) -> Result<Json<Page<KeySummary>>, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("仅管理员可查看 API Key"));
    }
    let keys = state.store.list_api_keys(&ctx.tenant_id)?;
    let out = keys
        .into_iter()
        .map(|k| KeySummary {
            key_id: k.key_id,
            name: k.name,
            scope: k.scope,
            created_at: k.created_at,
            revoked_at: k.revoked_at,
        })
        .collect();
    Ok(paginate(out, page.limit, page.offset))
}

/// 管理员吊销 API Key
pub async fn revoke(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !is_org_admin(ctx.role) {
        return Err(ApiError::forbidden("仅管理员可吊销 API Key"));
    }
    let revoked = state
        .store
        .revoke_api_key(&ctx.tenant_id, &id, &now_iso())?;
    if !revoked {
        return Err(ApiError::not_found("API Key 不存在或已吊销"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 生成随机 key（前缀 evorule_ + 32 字节 base64url）
fn generate_key() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let plain = format!("evorule_{}", URL_SAFE_NO_PAD.encode(bytes));
    let key_id = format!("key_{}", &plain[plain.len() - 16..]);
    (key_id, plain)
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}
