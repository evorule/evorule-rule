//! REST API 面（44 号 正交 B）
//!
//! MVP 定案（44 号 §14，2026-08-22）：
//! - REST+JSON、v1 版本化、cursor 分页（MVP 以 limit/offset 简化实现，接口契约对齐 cursor）；
//! - 统一错误不静默降级：`{ "error": { "code", "message" } }`；
//! - 同步导入、`Idempotency-Key` 幂等（MVP 留契约，POST 重复由唯一键兜底）；
//! - lifecycle 迁移统一走 `PATCH /v1/datasets/{id}/lifecycle`；
//! - api_keys 提供最小 scope 版（pull，执行侧拉取快照包联动）。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::auth::AuthService;
use crate::model::auth::Role;
use crate::store::RuleStore;

pub mod handlers_auth;
pub mod handlers_datasets;
pub mod handlers_keys;
pub mod handlers_llm;

/// 应用状态
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<RuleStore>,
    pub auth: Arc<AuthService>,
    /// 真实实例身份（39 号：白标不掩盖，进溯源）
    pub instance_id: String,
    /// evo-agent serve 地址（37 号：LLM 命名操作代理目标）
    pub llm_base_url: String,
}

impl AppState {
    pub fn new(store: RuleStore, secret: &str, instance_id: &str, llm_base_url: &str) -> Self {
        Self {
            store: Arc::new(store),
            auth: Arc::new(AuthService::new(secret)),
            instance_id: instance_id.to_string(),
            llm_base_url: llm_base_url.to_string(),
        }
    }
}

/// 已认证上下文（由 require_auth 中间件注入）
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: String,
    pub tenant_id: String,
    pub role: Role,
}

/// 统一 API 错误（44 号 §5：不静默降级）
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: msg.into(),
        }
    }
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: msg.into(),
        }
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: msg.into(),
        }
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: msg.into(),
        }
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: msg.into(),
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: msg.into(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorInner,
}
#[derive(Serialize)]
struct ErrorInner {
    code: String,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorBody {
            error: ErrorInner {
                code: self.code.to_string(),
                message: self.message,
            },
        });
        (self.status, body).into_response()
    }
}

impl From<crate::store::StoreError> for ApiError {
    fn from(e: crate::store::StoreError) -> Self {
        match e {
            crate::store::StoreError::DatasetNotFound(id) => ApiError::not_found(format!("数据集 `{id}` 不存在")),
            crate::store::StoreError::TenantNotFound(t) => ApiError::bad_request(format!("租户 `{t}` 不存在")),
            crate::store::StoreError::UsernameTaken(u) => ApiError::conflict(format!("用户名 `{u}` 已存在")),
            crate::store::StoreError::PublishRequiresApproval { dataset } => {
                ApiError::conflict(format!("数据集 `{dataset}` 进入 Published 必须走独立发布审批"))
            }
            other => ApiError::internal(other.to_string()),
        }
    }
}

/// JWT 认证中间件：校验 `Authorization: Bearer <token>`，注入 AuthContext
pub async fn require_auth(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token(req.headers())
        .ok_or_else(|| ApiError::unauthorized("缺少 Bearer token"))?;
    let now = unix_now();
    let claims = state
        .auth
        .verify_token(token, now, "access")
        .map_err(|_| ApiError::unauthorized("token 非法或已过期"))?;
    let role = Role::parse(&claims.role).ok_or_else(|| ApiError::unauthorized("token 角色非法"))?;
    req.extensions_mut().insert(AuthContext {
        user_id: claims.sub,
        tenant_id: claims.tenant_id,
        role,
    });
    Ok(next.run(req).await)
}

/// 从请求头取 Bearer token
pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(axum::http::header::AUTHORIZATION)?;
    let value = header.to_str().ok()?;
    value.strip_prefix("Bearer ")?.trim().into()
}

/// 从请求头取 X-Api-Key
pub fn api_key_from_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-api-key")?
        .to_str()
        .ok()
        .map(|s| s.trim())
}

/// 当前 unix 秒
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 构建 API 路由（44 号 §6 端点面，MVP 骨架）
pub fn router(state: AppState) -> Router {
    let public = Router::new()
        .route("/register", post(handlers_auth::register))
        .route("/login", post(handlers_auth::login))
        .route("/refresh", post(handlers_auth::refresh));

    // 快照包拉取：`get_bundle` 自身解析 Bearer / X-Api-Key 双认证，
    // 故不挂通用 Bearer 中间件（否则执行侧 X-Api-Key 拉取会被 401 拦截）
    let bundle = Router::new().route(
        "/datasets/{id}/bundle",
        get(handlers_datasets::get_bundle),
    );

    let protected = Router::new()
        .route("/me", get(handlers_auth::me))
        .route("/audits", get(handlers_auth::audits))
        .route(
            "/datasets",
            get(handlers_datasets::list_datasets).post(handlers_datasets::create_dataset),
        )
        .route(
            "/datasets/{id}",
            get(handlers_datasets::get_dataset),
        )
        .route(
            "/datasets/{id}/lifecycle",
            patch(handlers_datasets::transition_lifecycle),
        )
        .route("/datasets/{id}/publish", post(handlers_datasets::publish))
        .route(
            "/datasets/{id}/entries",
            get(handlers_datasets::list_entries).post(handlers_datasets::add_entry),
        )
        .route("/llm/audits", get(handlers_llm::list_llm_audits))
        .route("/llm/audits/stats", get(handlers_llm::llm_audit_stats))
        .route("/llm/ops/{operation}", post(handlers_llm::run_llm_op))
        .route("/api_keys", get(handlers_keys::list).post(handlers_keys::create))
        .route("/api_keys/{id}", delete(handlers_keys::revoke))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ));

    Router::new()
        .nest("/v1/auth", public)
        .nest("/v1/auth", protected.clone())
        .nest("/v1", protected)
        .nest("/v1", bundle)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    fn build_app() -> (Router, AppState) {
        let store = RuleStore::in_memory().expect("store");
        store
            .ensure_default_tenant("tenant_a", "示例组织", "inst-001", "2026-08-22T00:00:00Z")
            .expect("tenant");
        let state = AppState::new(store, "test-secret", "inst-001", "http://127.0.0.1:9");
        let app = router(state.clone());
        (app, state)
    }

    async fn send(
        app: Router,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        let body = body.map(|b| Body::from(b.to_string()));
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let req = builder.body(body.unwrap_or_else(Body::empty)).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap_or_default();
        let value = if text.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
        };
        (status, value)
    }

    async fn register_login(app: &Router) -> String {
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/auth/register",
            None,
            Some(json!({
                "tenant_id": "tenant_a",
                "username": "alice",
                "password": "password123",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "register: {body}");
        body["access_token"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_unauthorized_rejected() {
        let (app, _state) = build_app();
        let (status, body) = send(app.clone(), "GET", "/v1/datasets", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(body["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn test_register_login_me_flow() {
        let (app, _state) = build_app();
        let token = register_login(&app).await;

        // login
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/auth/login",
            None,
            Some(json!({
                "tenant_id": "tenant_a",
                "username": "alice",
                "password": "password123",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["access_token"].is_string());

        // me
        let (status, body) = send(app.clone(), "GET", "/v1/auth/me", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["username"], "alice");
        assert_eq!(body["role"], "rule_engineer");
    }

    #[tokio::test]
    async fn test_dataset_crud_via_api() {
        let (app, _state) = build_app();
        let token = register_login(&app).await;

        // 创建数据集
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets",
            Some(&token),
            Some(json!({
                "dataset_id": "ds-tax-01",
                "name": "税务合规规则集",
                "domain": ["tax"],
                "tags": ["合规"],
                "visibility": "private",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["lifecycle"]["status"], "Draft");

        // 列表
        let (status, body) = send(app.clone(), "GET", "/v1/datasets", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body.as_array().map(|a| a.len()).unwrap_or(0), 1);

        // 生命周期：rule_engineer 不能 active（需审批者）
        let (status, body) = send(
            app.clone(),
            "PATCH",
            "/v1/datasets/ds-tax-01/lifecycle",
            Some(&token),
            Some(json!({ "to": "active" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        // candidate 可以
        let (status, body) = send(
            app.clone(),
            "PATCH",
            "/v1/datasets/ds-tax-01/lifecycle",
            Some(&token),
            Some(json!({ "to": "candidate" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["lifecycle"]["status"], "Candidate");
    }

    #[tokio::test]
    async fn test_add_entry_and_bundle_via_api_key() {
        let (app, state) = build_app();
        let token = register_login(&app).await;

        // 建数据集 + 加条目
        send(
            app.clone(),
            "POST",
            "/v1/datasets",
            Some(&token),
            Some(json!({
                "dataset_id": "ds-tax-01",
                "name": "税务合规规则集",
                "domain": ["tax"],
            })),
        )
        .await;
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/entries",
            Some(&token),
            Some(json!({
                "entry_id": "rule-01",
                "version": 1,
                "domain": "tax",
                "rule_body": { "rule_id": "tax-01", "transform": [] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        // 通过 AuthService 直接注册 admin（绕过 API 的 rule_engineer 限制，测试用）
        let now = unix_now();
        state
            .auth
            .register(&state.store, "tenant_a", "admin", "password123", Role::Admin, now)
            .expect("admin register");
        let admin_tokens = state
            .auth
            .login(&state.store, "tenant_a", "admin", "password123", now)
            .expect("admin login");

        // 生成 key
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/api_keys",
            Some(&admin_tokens.access_token),
            Some(json!({ "name": "执行侧拉取" })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let plain = body["key"].as_str().unwrap().to_string();
        assert!(plain.starts_with("evorule_"));

        // 用 X-Api-Key 拉取快照包（本租户任意状态可拉）
        let (status, body) = send(app.clone(), "GET", "/v1/datasets/ds-tax-01/bundle", None, None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}"); // 无认证拒绝

        let req = Request::builder()
            .method("GET")
            .uri("/v1/datasets/ds-tax-01/bundle")
            .header("x-api-key", &plain)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let bundle: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(bundle["dataset"]["dataset_id"], "ds-tax-01");
    }
}

