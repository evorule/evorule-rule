//! REST API 面（44 号 正交 B）
//!
//! MVP 定案（44 号 §14，2026-08-22）：
//! - REST+JSON、v1 版本化；分页 `{ items, next_cursor }` + `limit`/`offset`（44 号 §3.3，
//!   60 号 P1-B3 已落地：路由层对租户作用域结果集内存分页，SQL pushdown 归批次 2）；
//! - 统一错误不静默降级：`{ "error": { "code", "message" } }`；
//! - 同步导入、`Idempotency-Key` 幂等（44 号 §14；60 号 P1-B4 落地，见各写端点）；
//! - lifecycle 迁移统一走 `PATCH /v1/datasets/{id}/lifecycle`；
//! - api_keys 提供最小 scope 版（pull，执行侧拉取快照包联动）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Extension, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::AuthService;
use crate::model::auth::Role;
use crate::store::RuleStore;

pub mod handlers_auth;
pub mod handlers_bundle;
pub mod handlers_datasets;
pub mod handlers_deps;
pub mod handlers_entries;
pub mod handlers_keys;
pub mod handlers_llm;
pub mod handlers_search;
pub mod handlers_services;

/// 活跃存储后端（45 号批次1 配置化双后端 · 最小接线）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// SQLite（默认，MVP 活跃引擎）
    Sqlite,
    /// PostgreSQL（仅 `--features postgres` 且成功建池/迁移后）
    Postgres,
}

impl BackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Sqlite => "sqlite",
            BackendKind::Postgres => "postgres",
        }
    }
}

/// 应用状态
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<RuleStore>,
    pub auth: Arc<AuthService>,
    /// 真实实例身份（39 号：白标不掩盖，进溯源）
    pub instance_id: String,
    /// evo-agent serve 地址（37 号：LLM 命名操作代理目标）
    pub llm_base_url: String,
    /// 当前活跃存储后端（45 号 最小接线；SQLite 默认，PG 需 feature+URL）
    pub backend: BackendKind,
    /// PG 启动自检结果描述（SQLite 下为 None；PG 模式如实报告，不伪造）
    pub pg_smoke: Option<String>,
    /// Idempotency-Key 幂等缓存（44 号 §14 / 60 号 P1-B4，单实例内存版）
    idem: Arc<Mutex<HashMap<String, IdemEntry>>>,
}

impl AppState {
    pub fn new(store: RuleStore, secret: &str, instance_id: &str, llm_base_url: &str) -> Self {
        Self {
            store: Arc::new(store),
            auth: Arc::new(AuthService::new(secret)),
            instance_id: instance_id.to_string(),
            llm_base_url: llm_base_url.to_string(),
            backend: BackendKind::Sqlite,
            pg_smoke: None,
            idem: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 设置后端与 PG 自检结果（启动 bootstrap 时调用；如实记录，单实例持久）。
    pub fn set_backend(&mut self, kind: BackendKind, pg_smoke: Option<String>) {
        self.backend = kind;
        self.pg_smoke = pg_smoke;
    }

    /// 配置化双后端启动靴（45 号 最小接线 · 不伪造）：
    /// - 默认 SQLite 活跃（MVP 引擎）；
    /// - `--features postgres` 且 `DATABASE_URL` 可用时，运行 `PgStore::smoke_check()`
    ///   （建池+迁移+最小 CRUD 往返）作为启动门控；
    /// - 冒烟成功 → 标注 Postgres + 探针详情；失败或未启用 → 回落到 SQLite 但如实记录原因。
    /// 返回诊断描述（供启动日志与 `/v1/admin/backend`）。
    pub async fn bootstrap_backend(self) -> Self {
        #[cfg(feature = "postgres")]
        {
            // `mut` 仅 PG 分支需要（set_backend 取 &mut self），避免默认构建 unused_mut 告警
            let mut s = self;
            let has_url = std::env::var("DATABASE_URL").is_ok();
            if !has_url {
                s.set_backend(BackendKind::Sqlite, None);
                // 注：未设 URL 时不伪造；PG 未实际启用，SQLite 继续活跃
                return s;
            }
            match crate::store::pg::PgStore::smoke_check().await {
                Ok(diag) => {
                    s.set_backend(BackendKind::Postgres, Some(diag));
                }
                Err(e) => {
                    // PG 连接/迁移失败：回落 SQLite，并如实记录失败原因（不 panic、不伪造）
                    s.set_backend(
                        BackendKind::Sqlite,
                        Some(format!("POSTGRES_PROBE_ERROR={e} → 回落到 SQLite，PG 未激活")),
                    );
                }
            }
            s
        }
        #[cfg(not(feature = "postgres"))]
        {
            // 未编译 postgres feature 时永不启用 PG；保留一次真实 await 避免 unused_async（语义无害）
            noop_await().await;
            self
        }
    }
}

/// 无副作用的一次 await（仅默认构建下保持 async 语义；不作任何实事）。
#[cfg(not(feature = "postgres"))]
async fn noop_await() {}

/// 幂等缓存条目（44 号 §14；数据源：写请求 + 响应；同 key 同负载返回缓存，不同负载 409）
struct IdemEntry {
    /// 首次请求体（Bytes 等值比较判定负载是否一致）
    req_body: Bytes,
    /// Pending=在途（并发同 key 防重入）；Done=已缓存完整响应
    state: IdemState,
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    expires_at: i64,
}

#[derive(Clone, Copy, PartialEq)]
enum IdemState {
    Pending,
    Done,
}

const IDEM_TTL_SECS: i64 = 86400; // 24h

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
            crate::store::StoreError::EntryNotFound { dataset, entry } => {
                ApiError::not_found(format!("条目 `{dataset}/{entry}` 不存在"))
            }
            crate::store::StoreError::EntryFrozen { dataset, entry } => {
                ApiError::conflict(format!("条目 `{dataset}/{entry}` 已冻结（Active/Published），不可原地修改"))
            }
            crate::store::StoreError::EntryExists { dataset, entry, version } => {
                ApiError::conflict(format!("条目 `{dataset}/{entry}` 已存在（version={version}）"))
            }
            crate::store::StoreError::IllegalTransition { from, to } => {
                ApiError::conflict(format!("非法状态迁移: {from:?} → {to:?}"))
            }
            crate::store::StoreError::DatasetNotDeletable { dataset, status } => {
                ApiError::conflict(format!("数据集 `{dataset}` 当前状态 `{status:?}` 不可删除（仅 Draft/Rejected）"))
            }
            crate::store::StoreError::EntryNotDeletable { dataset, entry, status } => {
                ApiError::conflict(format!("条目 `{dataset}/{entry}` 当前状态 `{status:?}` 不可删除（仅 Draft）"))
            }
            crate::store::StoreError::TemplateNotFound(t) => {
                ApiError::not_found(format!("服务模板 `{t}` 不存在"))
            }
            crate::store::StoreError::InvalidDiffRange { from, to } => {
                ApiError::bad_request(format!("版本 diff 区间非法: from=`{from}` to=`{to}`（需均存在于版本链且 from 先于 to）"))
            }
            crate::store::StoreError::Validation(e) => ApiError::bad_request(e.to_string()),
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
    // 登出后被拉黑的 token 拒绝访问（43 号 §3.3 jti 黑名单）
    // 查询黑名单遇存储错误不静默放行，显式报 500（不掩盖鉴权不确定性）
    let revoked = state
        .auth
        .is_blacklisted(&state.store, &claims, now)
        .map_err(|e| {
            tracing::warn!("黑名单查询失败，拒绝放行: {e}");
            ApiError::internal("鉴权状态不可用")
        })?;
    if revoked {
        return Err(ApiError::unauthorized("token 已失效（登出）"));
    }
    let role = Role::parse(&claims.role).ok_or_else(|| ApiError::unauthorized("token 角色非法"))?;
    req.extensions_mut().insert(AuthContext {
        user_id: claims.sub,
        tenant_id: claims.tenant_id,
        role,
    });
    Ok(next.run(req).await)
}

/// Idempotency-Key 幂等中间件（44 号 §14 / 60 号 P1-B4，单实例内存版）。
///
/// 仅对可能产生副作用的非幂等方法启用；需在 require_auth **之后**运行以取租户作用域。
/// - 无 `Idempotency-Key`：透传，不启用；
/// - 同 key 同负载：返回首次缓存的完整响应（重试不重复生效）；
/// - 同 key 不同负载：`409`（复用 key 改变负载）；
/// - 并发同 key 在途：`409`（防双写）。
pub async fn idempotency(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if matches!(
        req.method(),
        &Method::GET | &Method::HEAD | &Method::OPTIONS
    ) {
        return Ok(next.run(req).await);
    }
    let Some(key_header) = req
        .headers()
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
    else {
        return Ok(next.run(req).await);
    };
    if key_header.is_empty() {
        return Ok(next.run(req).await);
    }
    let Some(ctx) = req.extensions().get::<AuthContext>().cloned() else {
        // 未完成认证（正常不应发生于受保护路由）：透传，不施加幂等语义
        return Ok(next.run(req).await);
    };
    let cache_key = format!("{}:{}", ctx.tenant_id, key_header);

    // 拆请求体并保留 parts，供下游 next 复用
    let (parts, body) = req.into_parts();
    let req_body = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| ApiError::bad_request("读取请求体失败"))?;

    let now = unix_now();
    {
        let mut map = state.idem.lock().unwrap();
        map.retain(|_, e| e.expires_at > now); // 机会式清理过期项
        if let Some(entry) = map.get(&cache_key) {
            if entry.state == IdemState::Pending {
                return Err(ApiError::conflict("该 Idempotency-Key 正在处理中（并发重复请求）"));
            }
            if entry.req_body == req_body {
                let mut rp = Response::new(()).into_parts().0;
                rp.status = entry.status;
                rp.headers = entry.headers.clone();
                return Ok(Response::from_parts(rp, entry.body.clone().into()));
            }
            return Err(ApiError::conflict("Idempotency-Key 复用但请求体不同"));
        }
        // 首次：先登记在途，防止并发同 key 双写
        map.insert(
            cache_key.clone(),
            IdemEntry {
                req_body: req_body.clone(),
                state: IdemState::Pending,
                status: StatusCode::INTERNAL_SERVER_ERROR,
                headers: HeaderMap::new(),
                body: Bytes::new(),
                expires_at: now + IDEM_TTL_SECS,
            },
        );
    }

    // 还原请求并交给下游
    let req = Request::from_parts(parts, req_body.into());
    let resp = next.run(req).await;
    let (rparts, rbody) = resp.into_parts();
    let rbytes = match axum::body::to_bytes(rbody, usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("幂等缓存收集响应失败: {e}");
            return Err(ApiError::internal("读取响应体失败"));
        }
    };
    {
        let mut map = state.idem.lock().unwrap();
        if let Some(entry) = map.get_mut(&cache_key) {
            entry.state = IdemState::Done;
            entry.status = rparts.status;
            entry.headers = rparts.headers.clone();
            entry.body = rbytes.clone();
            entry.expires_at = unix_now() + IDEM_TTL_SECS;
        }
    }
    Ok(Response::from_parts(rparts, rbytes.into()))
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

/// 分页请求参数（44 号 §3.3，60 号 P1-B3 落地）
#[derive(Debug, Deserialize, Default)]
pub struct PageQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

pub const DEFAULT_PAGE_LIMIT: usize = 20;
pub const MAX_PAGE_LIMIT: usize = 100;

/// 分页响应封装 `{ items, next_cursor }`（44 号 §3.3）
#[derive(Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// 对租户作用域结果集应用 limit/offset 分页并计算 `next_cursor`（下一偏移，无更多则 null）。
/// 现于路由层做内存分页（结果集已按租户 SQL 过滤）；SQL 层 limit/offset pushdown 归批次 2。
pub fn paginate<T>(items: Vec<T>, limit: Option<usize>, offset: Option<usize>) -> Json<Page<T>> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
    let offset = offset.unwrap_or(0);
    let total = items.len();
    let window_end = offset.saturating_add(limit);
    let next_cursor = if window_end < total {
        Some(window_end.to_string())
    } else {
        None
    };
    Json(Page {
        items: items.into_iter().skip(offset).take(limit).collect(),
        next_cursor,
    })
}

/// 当前 unix 秒
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// GET /v1/admin/backend —— 存储后端自检（45 号 最小接线；如实报告活跃后端 + PG 探针，不伪造）
async fn admin_backend(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Value>, ApiError> {
    use crate::model::auth::{Action, can};
    // 运维诊断端点：要求管理员角色（四角色递进最高级）
    if !can(ctx.role, Action::Admin) {
        return Err(ApiError::forbidden("需要管理员角色查看后端自检"));
    }
    let mut out = serde_json::json!({
        "backend": state.backend.as_str(),
        "instance_id": state.instance_id,
        "note": "SQLite 为默认 MVP 活跃引擎；Postgres 仅 --features postgres 且 DATABASE_URL 可用时激活。此处如实报告，不伪造已切换。",
    });
    // PG 探针结果（有则附，无则标注未启用）
    if let Some(smoke) = &state.pg_smoke {
        out["postgres_probe"] = serde_json::json!({ "connected": true, "detail": smoke });
    } else {
        out["postgres_probe"] = serde_json::json!({ "connected": false });
    }
    Ok(Json(out))
}

/// 构建 API 路由（44 号 §6 端点面，MVP 骨架）
pub fn router(state: AppState) -> Router {
    let public = Router::new()
        .route("/register", post(handlers_auth::register))
        .route("/login", post(handlers_auth::login))
        .route("/refresh", post(handlers_auth::refresh))
        .route("/logout", post(handlers_auth::logout));

    // 快照包拉取：`get_bundle` 自身解析 Bearer / X-Api-Key 双认证，
    // 故不挂通用 Bearer 中间件（否则执行侧 X-Api-Key 拉取会被 401 拦截）
    let bundle = Router::new().route(
        "/datasets/{id}/bundle",
        get(handlers_datasets::get_bundle),
    );

    let protected = Router::new()
        .route("/me", get(handlers_auth::me))
        .route("/admin/backend", get(admin_backend))
        .route("/audits", get(handlers_auth::audits))
        .route("/audits/auth", get(handlers_auth::audits))
        .route("/audits/lifecycle", get(handlers_auth::lifecycle_audits))
        .route("/audits/llm", get(handlers_llm::list_llm_audits))
        .route(
            "/datasets",
            get(handlers_datasets::list_datasets).post(handlers_datasets::create_dataset),
        )
        .route(
            "/datasets/{id}",
            get(handlers_datasets::get_dataset)
                .patch(handlers_datasets::update_dataset_meta)
                .delete(handlers_datasets::delete_dataset_meta),
        )
        .route("/datasets/{id}/deps", get(handlers_deps::get_dataset_deps))
        .route(
            "/datasets/{id}/versions",
            get(handlers_datasets::list_versions).post(handlers_datasets::create_version),
        )
        .route(
            "/datasets/{id}/versions/{ver}",
            get(handlers_datasets::get_version),
        )
        .route(
            "/datasets/{id}/versions/{ver}/patch",
            post(handlers_datasets::create_patch),
        )
        .route(
            "/datasets/{id}/lifecycle",
            patch(handlers_datasets::transition_lifecycle),
        )
        .route("/datasets/{id}/publish", post(handlers_datasets::publish))
        .route("/datasets/{id}/unpublish", post(handlers_datasets::unpublish))
        .route(
            "/datasets/{id}/entries",
            get(handlers_datasets::list_entries).post(handlers_datasets::add_entry),
        )
        .route(
            "/entries",
            get(handlers_entries::list_entries_all).post(handlers_entries::create_entry),
        )
        .route(
            "/entries/{id}",
            get(handlers_entries::get_entry)
                .patch(handlers_entries::patch_entry)
                .delete(handlers_entries::delete_entry),
        )
        .route(
            "/entries/{id}/submit-candidate",
            post(handlers_entries::submit_candidate),
        )
        .route("/entries/{id}/approve", post(handlers_entries::approve))
        .route("/entries/{id}/history", get(handlers_entries::history))
        .route("/entries/{id}/deps", get(handlers_entries::deps))
        .route("/entries/{id}/versions", get(handlers_entries::entry_versions))
        .route("/entries/{id}/diff", get(handlers_entries::entry_diff))
        .route("/datasets/{id}/snapshots/stats", get(handlers_datasets::snapshot_stats))
        .route(
            "/deps/datasets/{id}",
            get(handlers_deps::get_dataset_deps).put(handlers_deps::put_dataset_deps),
        )
        .route(
            "/deps/templates",
            get(handlers_deps::list_templates).post(handlers_deps::create_template),
        )
        .route(
            "/deps/templates/{id}",
            get(handlers_deps::get_template),
        )
        .route(
            "/deps/templates/{id}/bind",
            post(handlers_deps::bind_template),
        )
        .route(
            "/services",
            get(handlers_services::list_services).post(handlers_services::create_service),
        )
        .route("/services/{name}", get(handlers_services::get_service).put(handlers_services::update_service))
        .route(
            "/search/datasets",
            get(handlers_search::search_datasets),
        )
        .route(
            "/search/datasets/{id}/diff",
            get(handlers_search::version_diff),
        )
        .route("/search/entries", get(handlers_search::search_entries))
        .route(
            "/bundles/datasets/{id}/versions/{ver}",
            get(handlers_bundle::export_version),
        )
        // T0 决策（2026-08-24）：带真实闸门一证据的导出（body 承载 tests 数组）
        .route("/bundles/export", post(handlers_bundle::export_with_tests))
        .route("/bundles/import", post(handlers_bundle::import_bundle))
        .route(
            "/bundles/import/dry-run",
            post(handlers_bundle::import_dry_run),
        )
        .route(
            "/bundles/{bundle_id}/status",
            get(handlers_bundle::import_status),
        )
        .route("/llm/audits", get(handlers_llm::list_llm_audits))
        .route("/llm/audits/stats", get(handlers_llm::llm_audit_stats))
        .route("/llm/ops/{operation}", post(handlers_llm::run_llm_op))
        .route("/api_keys", get(handlers_keys::list).post(handlers_keys::create))
        .route("/api_keys/{id}", delete(handlers_keys::revoke))
        // 幂等层置于 require_auth 之后运行（取证注入的 AuthContext 租户作用域）
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            idempotency,
        ))
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
    async fn test_idempotency_key_envelope() {
        let (app, _state) = build_app();
        let token = register_login(&app).await;
        let body_json = json!({ "dataset_id": "ds-idem-1", "name": "幂等数据集" });
        let body_bytes = axum::body::to_bytes(Body::from(body_json.to_string()), usize::MAX)
            .await
            .unwrap();

        // 带 Idempotency-Key 首次 POST（44 号 §14 幂等重试不重复生效）
        async fn post_with_key(
            app: &Router,
            token: &str,
            key: &str,
            body: Bytes,
        ) -> (StatusCode, serde_json::Value) {
            let req = Request::builder()
                .method("POST")
                .uri("/v1/datasets")
                .header("authorization", format!("Bearer {token}"))
                .header("Idempotency-Key", key)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(bytes.to_vec()).unwrap_or_default();
            let value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            (status, value)
        }

        // 首次：201
        let (status, body) = post_with_key(&app, &token, "key-1", body_bytes.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["dataset_id"], "ds-idem-1");

        // 同 key 同负载重放：返回缓存的 201（不因数据集已存在而 409）
        let (status, body) = post_with_key(&app, &token, "key-1", body_bytes.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "重放应返回缓存响应: {body}");
        assert_eq!(body["dataset_id"], "ds-idem-1");

        // 同 key 不同负载：409 拒绝（key 不可复用换负载）
        let (status, body) = post_with_key(
            &app,
            &token,
            "key-1",
            axum::body::to_bytes(
                Body::from(json!({ "dataset_id": "ds-idem-1", "name": "改名" }).to_string()),
                usize::MAX,
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "换负载需 409: {body}");
        assert_eq!(body["error"]["message"], "Idempotency-Key 复用但请求体不同");
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
    async fn test_logout_revokes_refresh_via_api() {
        let (app, _state) = build_app();

        // 注册（返回 access+refresh_token）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/auth/register",
            None,
            Some(json!({
                "tenant_id": "tenant_a",
                "username": "eve",
                "password": "password123",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let refresh = body["refresh_token"].as_str().unwrap().to_string();

        // 登出前 refresh 可用
        let (status, _) = send(
            app.clone(),
            "POST",
            "/v1/auth/refresh",
            None,
            Some(json!({ "tenant_id": "tenant_a", "refresh_token": refresh })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // 登出 → 204
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/auth/logout",
            None,
            Some(json!({ "refresh_token": refresh })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        // 登出后同 refresh 刷新 → 401
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/auth/refresh",
            None,
            Some(json!({ "tenant_id": "tenant_a", "refresh_token": refresh })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
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
        assert_eq!(body["items"].as_array().map(|a| a.len()).unwrap_or(0), 1);

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
    async fn test_dataset_lawref_and_version_selection_roundtrip() {
        let (app, _state) = build_app();
        let token = register_login(&app).await;

        // 创建带 law_ref + version_selection 的数据集（T6 偏差修复：导出 bundle 不再缺字段）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets",
            Some(&token),
            Some(json!({
                "dataset_id": "ds-law-01",
                "name": "法规锚数据集",
                "domain": ["compliance"],
                "tags": ["法规"],
                "visibility": "private",
                "law_ref": {
                    "document_id": "com.yuanze.robot.quality_control",
                    "law_version": "1.0.0",
                    "effective_from": "2024-01-01"
                },
                "version_selection": { "mode": "auto_by_effective_date" }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["law_ref"]["document_id"], "com.yuanze.robot.quality_control");
        assert_eq!(body["law_ref"]["effective_from"], "2024-01-01");
        assert_eq!(body["version_selection"]["mode"], "auto_by_effective_date");

        // 加一条合法最小规则体，保证导出非空
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-law-01/entries",
            Some(&token),
            Some(json!({
                "dataset_id": "ds-law-01",
                "entry_id": "rule-demo",
                "version": 1,
                "domain": "compliance",
                "tags": [],
                "rule_body": [{"type": "set", "params": {"attr": "x", "operation": "set", "value": 1}}]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        // 导出 bundle 验证携带 law_ref + version_selection（无需 resign_bundle 改签补字段）
        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/bundles/datasets/ds-law-01/versions/v1",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["dataset"]["law_ref"]["document_id"],
            "com.yuanze.robot.quality_control"
        );
        assert_eq!(body["dataset"]["law_ref"]["effective_from"], "2024-01-01");
        assert_eq!(
            body["dataset"]["version_selection"]["mode"],
            "auto_by_effective_date"
        );

        // PATCH 更新 law_ref / version_selection（None = 不修改）
        let (status, body) = send(
            app.clone(),
            "PATCH",
            "/v1/datasets/ds-law-01",
            Some(&token),
            Some(json!({
                "version_selection": {
                    "mode": "pinned",
                    "pinned_version": "v2",
                    "pinned_include_patch": false
                }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["version_selection"]["mode"], "pinned");
        assert_eq!(body["version_selection"]["pinned_version"], "v2");
        // law_ref 未被触碰（PATCH 语义：None = 不修改）
        assert_eq!(
            body["law_ref"]["document_id"],
            "com.yuanze.robot.quality_control"
        );
    }

    #[tokio::test]
    async fn test_list_pagination_envelope() {
        let (app, _state) = build_app();
        let token = register_login(&app).await;

        // 建 3 个数据集，验证 { items, next_cursor } 分页封装（44 号 §3.3）
        for i in 0..3 {
            let (status, body) = send(
                app.clone(),
                "POST",
                "/v1/datasets",
                Some(&token),
                Some(json!({ "dataset_id": format!("ds-pg-{i}"), "name": format!("pg{i}") })),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }

        // 首页 limit=2：items 2 条，next_cursor=2
        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/datasets?limit=2",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"].as_array().map(|a| a.len()).unwrap_or(0), 2);
        assert_eq!(body["next_cursor"], "2");

        // 第二页 offset=2：剩余 1 条，无更多 → next_cursor 为 null
        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/datasets?offset=2",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"].as_array().map(|a| a.len()).unwrap_or(0), 1);
        assert!(body["next_cursor"].is_null());
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
                "rule_body": { "rule_id": "tax-01", "transform": [{"type": "set", "params": {"attr": "x", "operation": "set", "value": 1}}] }
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
        // T0 决策（2026-08-24 矛盾 B）：执行侧拉取无测试工作台 → 显式 verdict=fail（不默认 Pass）
        assert_eq!(bundle["tests"]["verdict"], "fail", "T0: get_bundle 无证据必须显式 fail");

        // T0 决策（2026-08-24 矛盾 A）：GET export_version 无 tests → 显式 verdict=fail（不默认 Pass）
        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/bundles/datasets/ds-tax-01/versions/v1",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["tests"]["verdict"], "fail", "T0: GET 导出无证据必须显式 fail");

        // T0 决策（2026-08-24 矛盾 A）：POST /bundles/export 携带真实 tests → verdict 如实反映（pass）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/bundles/export",
            Some(&token),
            Some(json!({
                "dataset_id": "ds-tax-01",
                "version": "v1",
                "tests": { "verdict": "pass", "subset": ["rule-01"], "fixtures": [] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["tests"]["verdict"], "pass", "T0: 带证据导出 verdict 如实反映沙箱结果");

        // T0 决策：带证据导出若 verdict=fail 也必须如实带出（不静默改 Pass）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/bundles/export",
            Some(&token),
            Some(json!({
                "dataset_id": "ds-tax-01",
                "version": "v1",
                "tests": { "verdict": "fail", "subset": ["rule-01"], "fixtures": [] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["tests"]["verdict"], "fail", "T0: 带证据导出 verdict=fail 如实带出");
    }

    /// 测试辅助：直接注册 admin 并返回 access token
    async fn admin_token(state: &AppState) -> String {
        let now = unix_now();
        state
            .auth
            .register(&state.store, "tenant_a", "admin", "password123", Role::Admin, now)
            .expect("register admin");
        let t = state
            .auth
            .login(&state.store, "tenant_a", "admin", "password123", now)
            .expect("login admin");
        t.access_token
    }

    /// 测试辅助：用规则工程师建一个含 tax 条目的数据集
    async fn seed_dataset(app: &Router, token: &str, dataset_id: &str) {
        let (status, _) = send(
            app.clone(),
            "POST",
            "/v1/datasets",
            Some(token),
            Some(json!({
                "dataset_id": dataset_id,
                "name": "税务合规规则集",
                "domain": ["tax"],
                "tags": ["合规"],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = send(
            app.clone(),
            "POST",
            &format!("/v1/datasets/{dataset_id}/entries"),
            Some(token),
            Some(json!({
                "entry_id": "rule-01",
                "version": 1,
                "domain": "tax",
                "rule_body": { "rule_id": "tax-01", "transform": [{"type": "set", "params": {"attr": "x", "operation": "set", "value": 1}}] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_entries_top_level_endpoints() {
        let (app, _state) = build_app();
        let token = register_login(&app).await;
        seed_dataset(&app, &token, "ds-tax-01").await;

        // GET /entries（租户内全部）
        let (status, body) = send(app.clone(), "GET", "/v1/entries", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"].as_array().map(|a| a.len()).unwrap_or(0), 1);

        // GET /entries/{id}（详情）
        let (status, body) = send(app.clone(), "GET", "/v1/entries/rule-01", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entry_id"], "rule-01");

        // POST /entries（顶层创建草稿）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/entries",
            Some(&token),
            Some(json!({
                "dataset_id": "ds-tax-01",
                "entry_id": "rule-02",
                "domain": "tax",
                "rule_body": { "rule_id": "tax-02", "transform": [{"type": "set", "params": {"attr": "x", "operation": "set", "value": 1}}] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["status"], "Draft");

        // 条目级生命周期：submit-candidate（需沙箱证据）→ approve（需审批者）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/entries/rule-02/submit-candidate",
            Some(&token),
            Some(json!({ "sandbox_report_id": "sbox-001" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "Candidate");

        // 审批需审批者角色 → rule_engineer 被拒
        let (status, body) = send(app.clone(), "POST", "/v1/entries/rule-02/approve", Some(&token), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        // history（only-append）
        let (status, body) = send(app.clone(), "GET", "/v1/entries/rule-02/history", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body.as_array().map(|a| a.len()).unwrap_or(0), 1);
    }

    #[tokio::test]
    async fn test_entry_versions_and_content_diff() {
        let (app, _state) = build_app();
        let token = register_login(&app).await;
        seed_dataset(&app, &token, "ds-tax-02").await;

        // 建一个条目 v1
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-02/entries",
            Some(&token),
            Some(json!({
                "entry_id": "rule-a",
                "version": 1,
                "rule_body": { "rule_id": "a", "description": "初版", "transform": [{"type": "set", "params": {"attr": "x", "operation": "set", "value": 1}}] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "v1 create status={status} body={body}");

        // v2：变化内容
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/entries",
            Some(&token),
            Some(json!({
                "dataset_id": "ds-tax-02",
                "entry_id": "rule-a",
                "version": 2,
                "rule_body": { "rule_id": "a", "description": "改版", "transform": [{"type": "set", "params": {"attr": "x", "operation": "set", "value": 1}}] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        // 版本历史（C1）
        let (status, body) = send(app.clone(), "GET", "/v1/entries/rule-a/versions", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["versions"].as_array().map(|a| a.len()).unwrap_or(0), 2);

        // 内容级 diff（C2）：v1→v2 变化，含 description
        let (status, body) = send(app.clone(), "GET", "/v1/entries/rule-a/diff?from=1&to=2", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["changed"], true);

        // 内容去重统计（C1）
        let (status, body) = send(app.clone(), "GET", "/v1/datasets/ds-tax-02/snapshots/stats", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entry_version_rows"], 3, "seed rule-01 + rule-a v1 + rule-a v2");
        assert!(body["distinct_snapshots"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_dataset_versions_and_metadata() {
        let (app, state) = build_app();
        let token = register_login(&app).await;
        seed_dataset(&app, &token, "ds-tax-01").await;

        // PATCH 元数据
        let (status, body) = send(
            app.clone(),
            "PATCH",
            "/v1/datasets/ds-tax-01",
            Some(&token),
            Some(json!({ "description": "更新后的描述", "tags": ["合规", "税务"] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["description"], "更新后的描述");

        // 版本列表
        let (status, body) = send(app.clone(), "GET", "/v1/datasets/ds-tax-01/versions", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["current"], "v1");

        // 升版 major
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/versions",
            Some(&token),
            Some(json!({ "kind": "major" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["new_version"], "v2");
        assert_eq!(body["current"], "v2");

        // 版本详情：历史版本内容不可得（诚实标注）
        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/datasets/ds-tax-01/versions/v1",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["content_available"], false);

        // 仅当前版本可 patch；历史版本 → 400
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/versions/v1/patch",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/versions/v2/patch",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["new_version"], "v2.p1");

        // DELETE 数据集需 admin（rule_engineer 被拒）
        let (status, body) = send(app.clone(), "DELETE", "/v1/datasets/ds-tax-01", Some(&token), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        // 升到 Candidate（不可删），admin 删除 → CONFLICT
        send(
            app.clone(),
            "PATCH",
            "/v1/datasets/ds-tax-01/lifecycle",
            Some(&token),
            Some(json!({ "to": "candidate" })),
        )
        .await;
        let admin = admin_token(&state).await;
        let (status, body) = send(app.clone(), "DELETE", "/v1/datasets/ds-tax-01", Some(&admin), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}"); // Candidate 态不可删
    }

    #[tokio::test]
    async fn test_publish_requires_second_confirm_and_tenant_guard() {
        let (app, state) = build_app();
        let token = register_login(&app).await; // rule_engineer
        seed_dataset(&app, &token, "ds-tax-01").await;
        let admin = admin_token(&state).await; // 审批者+二次确认（发布）

        // 升到 Candidate（engineer）→ Active（审批者）
        for (who, to) in [(&token, "candidate"), (&admin, "active")] {
            let (status, body) = send(
                app.clone(),
                "PATCH",
                "/v1/datasets/ds-tax-01/lifecycle",
                Some(who),
                Some(json!({ "to": to })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }

        // 二次确认：缺字段（结构拒绝 422）/ confirm=false（业务拒绝 400）
        let (status, _) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/publish",
            Some(&admin),
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY); // 缺 confirm 字段，axum 结构拒绝
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/publish",
            Some(&admin),
            Some(json!({ "confirm": false })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "bad_request");
        // 确认后未被发布（状态仍 Active）
        let (status, body) = send(app.clone(), "GET", "/v1/datasets/ds-tax-01", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["lifecycle"]["status"], "Active");

        // 带 confirm=true → 发布成功 Published
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/publish",
            Some(&admin),
            Some(json!({ "confirm": true, "reason": "季度合规部验证通过" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["lifecycle"]["status"], "Published");
        let hist = body["lifecycle"]["state_history"].as_array().unwrap();
        let published_cause = hist
            .iter()
            .find(|h| h["to"].as_str() == Some("Published"))
            .and_then(|h| h["cause"].as_str())
            .unwrap_or_default();
        assert!(published_cause.contains("二次确认"), "cause: {published_cause}");

        // 管理端撤销发布 → Published → Rejected（34 号 §2/§4，非 Active）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/unpublish",
            Some(&admin),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["lifecycle"]["status"], "Rejected");

        // 租户隔离（38 号 §10-3）：他租户 admin 无法迁移/发布/撤销本租户数据集，均返回 404
        state
            .store
            .ensure_default_tenant("tenant_b", "另一组织", "inst-002", "2026-08-22T00:00:00Z")
            .expect("tenant_b");
        state
            .auth
            .register(&state.store, "tenant_b", "badmin", "password123", Role::Admin, unix_now())
            .expect("register b_admin");
        let other_admin = state
            .auth
            .login(&state.store, "tenant_b", "badmin", "password123", unix_now())
            .expect("login b_admin")
            .access_token;

        // 生命周期迁移：tenant_b admin → 404
        let (status, body) = send(
            app.clone(),
            "PATCH",
            "/v1/datasets/ds-tax-01/lifecycle",
            Some(&other_admin),
            Some(json!({ "to": "candidate" })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

        // 发布：tenant_b admin → 404
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/publish",
            Some(&other_admin),
            Some(json!({ "confirm": true })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

        // 撤销：tenant_b admin → 404
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/unpublish",
            Some(&other_admin),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }

    #[tokio::test]
    async fn test_deps_templates_search_and_diff() {
        let (app, state) = build_app();
        let token = register_login(&app).await;
        seed_dataset(&app, &token, "ds-tax-01").await;

        // C1（02 方案层1）：依赖声明服务名必须已注册在服务目录 —— 先注册 payroll_svc
        let admin = admin_token(&state).await;
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/services",
            Some(&admin),
            Some(json!({
                "service_name": "payroll_svc",
                "version": "1.0.0",
                "sensitive": false
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        // PUT 数据集依赖（engineer）
        let (status, body) = send(
            app.clone(),
            "PUT",
            "/v1/deps/datasets/ds-tax-01",
            Some(&token),
            Some(json!({
                "inputs": [{ "name": "payroll_event", "empty_allowed": false }],
                "services": [{ "service_name": "payroll_svc", "sensitive": false }]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["services"][0]["service_name"] == "payroll_svc");

        // GET 数据集依赖（alias 端点）
        let (status, body) = send(app.clone(), "GET", "/v1/deps/datasets/ds-tax-01", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["services"].as_array().map(|a| a.len()).unwrap_or(0), 1);

        // 注册服务模板需 admin
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/deps/templates",
            Some(&token),
            Some(json!({
                "service_name": "payroll_svc",
                "kind": "pull",
                "endpoint_template": "http://{host}:{port}/api/payroll",
                "placeholder_notes": { "host": "客户内网主机" }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/deps/templates",
            Some(&admin),
            Some(json!({
                "service_name": "payroll_svc",
                "kind": "pull",
                "endpoint_template": "http://{host}:{port}/api/payroll",
                "method": "POST",
                "placeholder_notes": { "host": "客户内网主机", "port": "服务端口" }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let template_id = body["template_id"].as_str().unwrap().to_string();

        // 模板列表/详情
        let (status, body) = send(app.clone(), "GET", "/v1/deps/templates", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"].as_array().map(|a| a.len()).unwrap_or(0), 1);

        let (status, body) = send(
            app.clone(),
            "GET",
            &format!("/v1/deps/templates/{template_id}"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["service_name"], "payroll_svc");

        // bind 占位符填充（engineer）
        let (status, body) = send(
            app.clone(),
            "POST",
            &format!("/v1/deps/templates/{template_id}/bind"),
            Some(&token),
            Some(json!({ "values": { "host": "10.0.0.5", "port": "8080" } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["url"], "http://10.0.0.5:8080/api/payroll");
        assert_eq!(body["method"], "POST");

        // 检索：数据集 / 条目
        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/search/datasets?domain=tax&q=合规",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"].as_array().map(|a| a.len()).unwrap_or(0), 1);

        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/search/entries?dataset_id=ds-tax-01&q=tax-01",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"].as_array().map(|a| a.len()).unwrap_or(0), 1);

        // 版本 diff（先升一版）
        send(
            app.clone(),
            "POST",
            "/v1/datasets/ds-tax-01/versions",
            Some(&token),
            Some(json!({ "kind": "major" })),
        )
        .await;
        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/search/datasets/ds-tax-01/diff?from=v1&to=v2",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["added_versions"][0], "v2");
        // 内容归因（45 号批次1 / C 类闭合）：升版 v1→v2 条目未变 → v1 留档哈希与当前 v2 归因相同 ⇒ unchanged。
        let attr = &body["content_attribution"];
        assert!(attr.is_object(), "缺少内容归因：{body}");
        assert_eq!(
            body["current_entry_count"].as_u64().unwrap(),
            attr["unchanged"].as_array().map(|a| a.len()).unwrap_or(0) as u64,
            "升版未改条目 ⇒ 全部归 unchanged：{body}"
        );
        assert_eq!(attr["added"].as_array().map(|a| a.len()).unwrap_or(0), 0, "{body}");
        assert_eq!(attr["removed"].as_array().map(|a| a.len()).unwrap_or(0), 0, "{body}");
    }

    #[tokio::test]
    async fn test_service_catalog_and_deps_declaration_validation() {
        // 02 方案 C1/C2：服务目录 CRUD + 依赖声明服务名事前预检（不静默）
        let (app, state) = build_app();
        let token = register_login(&app).await; // rule_engineer
        seed_dataset(&app, &token, "ds-tax-01").await;

        // 普通工程师无权限注册服务（D13：服务由服务公司/官方维护）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/services",
            Some(&token),
            Some(json!({ "service_name": "payroll_svc" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        // 依赖声明未注册服务 → 400 显式拒绝（C1 事前预检）
        let (status, body) = send(
            app.clone(),
            "PUT",
            "/v1/deps/datasets/ds-tax-01",
            Some(&token),
            Some(json!({ "inputs": [], "services": [{ "service_name": "ghost_svc" }] })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("未在服务目录注册"),
            "应显式提示注册路径: {body}"
        );

        // 平台官方目录 seed（模拟 main.rs 启动预置 7 原生服务）
        let seeded = state
            .store
            .seed_official_services_if_empty("2026-08-25T00:00:00Z")
            .unwrap();
        assert_eq!(seeded, 7, "官方原生服务应预置 7 个");

        let admin = admin_token(&state).await;

        // GET /v1/services：平台官方 7 + 本租户自定义
        let (status, body) = send(app.clone(), "GET", "/v1/services", Some(&admin), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let list = body.as_array().expect("services 应为数组");
        assert!(list.len() >= 7, "官方目录至少 7 个: {body}");
        assert!(
            list.iter().any(|s| s["service_name"] == "llm_advisor" && s["sensitive"] == true),
            "llm_advisor 应标记 sensitive=true（C6）"
        );

        // 租户注册新服务（sensitive 由目录权威标记）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/services",
            Some(&admin),
            Some(json!({
                "service_name": "payroll_svc",
                "version": "1.2.0",
                "sensitive": true,
                "binding_hint": "registry"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["scope"], "tenant:tenant_a");
        assert_eq!(body["version"], "1.2.0");

        // 详情 + 更新
        let (status, body) = send(
            app.clone(),
            "GET",
            "/v1/services/payroll_svc",
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["service_name"], "payroll_svc");
        let (status, body) = send(
            app.clone(),
            "PUT",
            "/v1/services/payroll_svc",
            Some(&admin),
            Some(json!({ "service_name": "payroll_svc", "version": "1.3.0", "sensitive": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["version"], "1.3.0");

        // 已注册服务（平台官方 llm_advisor + 租户 payroll_svc）可声明
        let (status, body) = send(
            app.clone(),
            "PUT",
            "/v1/deps/datasets/ds-tax-01",
            Some(&token),
            Some(json!({
                "inputs": [],
                "services": [
                    { "service_name": "llm_advisor", "sensitive": true },
                    { "service_name": "payroll_svc", "sensitive": true }
                ]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["services"].as_array().map(|a| a.len()).unwrap_or(0), 2);

        // bundle 导出应携带服务契约补齐（C3/C4/C6：version/io_contract/sensitive 从目录下沉）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/bundles/export",
            Some(&admin),
            Some(json!({
                "dataset_id": "ds-tax-01",
                "version": "v1",
                "tests": { "verdict": "pass", "subset": [], "fixtures": [] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let services = body["data_dependencies"]["services"].as_array().unwrap();
        let payroll = services
            .iter()
            .find(|s| s["service_name"] == "payroll_svc")
            .expect("导出应含 payroll_svc");
        assert_eq!(payroll["version"], "1.3.0", "服务版本应从目录补齐（C4）");
        assert_eq!(payroll["sensitive"], true, "敏感标记以目录为权威（C6）");
    }

    #[tokio::test]
    async fn test_bundle_import_via_admin() {
        let (app, state) = build_app();
        let token = register_login(&app).await;
        let admin = admin_token(&state).await;

        // 经 store 直接构造含服务依赖的数据集与条目（供导出合法快照包）
        let ds = crate::model::dataset::RuleDataset {
            dataset_id: "ds-pay-01".into(),
            name: "工资合规".into(),
            description: None,
            domain: vec!["payroll".into()],
            tags: vec!["合规".into()],
            tenant_id: "tenant_a".into(),
            visibility: crate::model::dataset::Visibility::Private,
            lifecycle: crate::model::lifecycle::Lifecycle::default(),
            versioning: crate::model::version::Versioning::default(),
            law_ref: Some(crate::model::version::LawRef {
                document_id: "law-001".into(),
                law_version: None,
                effective_from: Some("2026-01-01".into()),
                effective_to: None,
            }),
            version_selection: None,
            data_dependencies: Some(crate::model::dependency::DataDependencies {
                inputs: vec![],
                services: vec![crate::model::dependency::ServiceDecl {
                    service_name: "payroll_svc".into(),
                    version: None,
                    io_contract: None,
                    sensitive: false,
                    description: None,
                    template: None,
                }],
            }),
            meta: crate::model::dataset::Meta {
                created_at: "2026-08-01T00:00:00Z".into(),
                created_by: "eng".into(),
                updated_at: None,
                updated_by: None,
            },
        };
        state.store.create_dataset(&ds).unwrap();
        let entry = crate::model::entry::RuleEntry {
            entry_id: "e-pay-1".into(),
            dataset_id: "ds-pay-01".into(),
            version: 1,
            status: Some(crate::model::lifecycle::LifecycleStatus::Active),
            provenance: crate::model::provenance::Provenance {
                source: "测试".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "payroll".into(),
            tags: vec![],
            data_source_binding: vec![crate::model::dependency::SourceBinding {
                rule_ref: "transform[0]".into(),
                service_name: "payroll_svc".into(),
            }],
            consumed_inputs: vec![],
            rule_body: serde_json::json!({
                "rule_id": "e-pay-1",
                "transform": [{ "type": "io_request", "params": { "service_name": "payroll_svc" } }]
            }),
            governance: None,
        };
        state.store.add_entry(&entry).unwrap();

        let bundle = state
            .store
            .export_bundle("ds-pay-01", &crate::bundle::BundleTests::default(), "eng", "2026-08-02T00:00:00Z", "inst-001")
            .unwrap();

        // dry-run 预检（不落库）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/bundles/import/dry-run",
            Some(&admin),
            Some(serde_json::json!({ "bundle": bundle })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["valid"], true);
        assert_eq!(body["entry_count"], 1);

        // 正式导入（admin）
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/bundles/import",
            Some(&admin),
            Some(serde_json::json!({ "bundle": bundle })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["status"], "imported");
        assert_eq!(body["activated_version"], "v1");

        // 导入后可拉取
        let (status, body) = send(app.clone(), "GET", "/v1/datasets/ds-pay-01", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data_dependencies"]["services"][0]["service_name"], "payroll_svc");
    }
}

