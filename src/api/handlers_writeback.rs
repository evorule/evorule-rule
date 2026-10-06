//! 回写通道收件端点（P1-2/RS-1，11 号路线图）：POST /v1/writeback/rule_failure
//!
//! 设计文档 §6 回写闭环的**收件下半程**：执行侧（evorule-server）→ evorule-rule。
//! MVP **只收不触发**——收件落账两笔（writeback_events 队列 + llm_op_audit 审计面）
//! 后即返回，「规则失效 → LLM 补丁（patch_rule）→ 沙箱验证 → 新版本」闭环后置
//! （失败数据先积累，补丁动作等数据说话）。
//!
//! 认证：服务级 X-Api-Key(scope=writeback:rule_failure) 自管（同 invoke 先例，
//! 不挂通用 Bearer 中间件）；事件 schema 单源 [`crate::model::writeback::RuleFailureEvent`]。

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;

use crate::api::handlers_auth::now_iso;
use crate::api::{api_key_from_header, ApiError, AppState, AuthContext};
use crate::model::auth::{can, Action};
use crate::model::writeback::{validate_event, RuleFailureEvent};
use axum::extract::Extension;

/// 服务级回写 scope：仅限本端点消费（执行侧 → evorule-rule 单向收件）
pub const API_KEY_SCOPE_WRITEBACK: &str = "writeback:rule_failure";

/// 收件响应体
#[derive(Debug, Clone, serde::Serialize)]
pub struct WritebackReceipt {
    pub received: bool,
    pub event_id: i64,
    /// 队列状态（收件即入队）
    pub status: &'static str,
    /// 闭环状态如实标注（首版只收不触发）
    pub patch_action: &'static str,
}

/// POST /v1/writeback/rule_failure —— 回写事件收件。
/// X-Api-Key(scope=writeback:rule_failure) 认证 → 事件类型白名单（MVP 仅
/// rule_failure）→ 租户匹配（key 租户即事件租户，声明租户不可越权）→
/// 单事务落两笔（队列 + 审计面，审计可查 GET /llm/audits 与 /audits/llm）。
pub async fn receive_rule_failure(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(event): Json<RuleFailureEvent>,
) -> Result<(StatusCode, Json<WritebackReceipt>), ApiError> {
    // 1) 服务级 X-Api-Key（scope 收敛：仅 writeback:rule_failure 可用）
    let k = api_key_from_header(&headers)
        .ok_or_else(|| ApiError::unauthorized("缺少认证：需 X-Api-Key"))?;
    let hash = sha256_hex(k);
    let key = state
        .store
        .get_api_key_by_hash(&hash)?
        .ok_or_else(|| ApiError::unauthorized("API Key 非法"))?;
    if key.revoked_at.is_some() {
        return Err(ApiError::unauthorized("API Key 已吊销"));
    }
    if key.scope != API_KEY_SCOPE_WRITEBACK {
        return Err(ApiError::forbidden(
            "API Key 无 writeback:rule_failure 权限",
        ));
    }
    // 2) 事件类型白名单（MVP 仅 rule_failure；扩展类型演进判别后放开）
    validate_event(&event).map_err(ApiError::unprocessable_entity)?;
    // 3) 租户匹配（执行侧 key 随租户签发；事件声明租户与 key 租户不一致=越权拒收）
    if event.tenant_id != key.tenant_id {
        return Err(ApiError::forbidden("事件 tenant_id 与 API Key 租户不匹配"));
    }
    // 4) 单事务落账：队列行（T1 追认队列形态）+ 审计面行（llm_op_audit）
    let received_at = now_iso();
    let event_json = serde_json::to_string(&event)
        .map_err(|_| ApiError::unprocessable_entity("事件序列化失败"))?;
    let failure_type = event.failure.as_ref().map(|f| f.r#type.clone());
    let event_id = state.store.record_writeback_event(
        &key.tenant_id,
        &event.dataset_id,
        &event.entry_id,
        &event.version_used,
        failure_type.as_deref(),
        &event_json,
        &received_at,
    )?;
    tracing::info!(
        target: "evorule::writeback",
        event_id,
        dataset_id = %event.dataset_id,
        entry_id = %event.entry_id,
        "回写事件收件入队（只收不触发）"
    );
    Ok((
        StatusCode::CREATED,
        Json(WritebackReceipt {
            received: true,
            event_id,
            status: "queued",
            patch_action: "not_triggered",
        }),
    ))
}

/// GET /v1/writeback/rule_failure 查询参数
#[derive(Deserialize)]
pub struct WritebackQueueQuery {
    /// 返回上限（默认 100）
    #[serde(default)]
    pub limit: Option<usize>,
}

/// GET /v1/writeback/rule_failure —— 回写队列查看（T1 追认队列形态，租户作用域，
/// 按收件时间倒序；审批者及以上角色，同 machine-gate 追认队列口径）。
pub async fn list_rule_failure_queue(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    axum::extract::Query(q): axum::extract::Query<WritebackQueueQuery>,
) -> Result<Json<Vec<crate::model::writeback::WritebackEventRow>>, ApiError> {
    if !can(ctx.role, Action::Approve) {
        return Err(ApiError::forbidden("回写队列查看需审批者及以上角色"));
    }
    let limit = q.limit.unwrap_or(100).min(1000);
    Ok(Json(
        state.store.list_writeback_events(&ctx.tenant_id, limit)?,
    ))
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use crate::api::{router, AppState};
    use crate::store::RuleStore;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    async fn send(
        app: axum::Router,
        method: &str,
        uri: &str,
        token: Option<&str>,
        api_key: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        if let Some(k) = api_key {
            builder = builder.header("x-api-key", k);
        }
        if let Some(b) = &body {
            builder = builder.header("content-type", "application/json");
            let req = builder.body(Body::from(b.to_string())).unwrap();
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(bytes.to_vec()).unwrap_or_default();
            return (status, serde_json::from_str(&text).unwrap_or_default());
        }
        let req = builder.body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap_or_default();
        (status, serde_json::from_str(&text).unwrap_or_default())
    }

    fn event_json(tenant: &str) -> serde_json::Value {
        json!({
            "event_type": "rule_failure",
            "tenant_id": tenant,
            "dataset_id": "ds-tax-2024",
            "version_used": "v2.p1",
            "entry_id": "entry-tax-001",
            "occurred_at": "2026-10-07T00:00:00Z",
            "execution_ctx": { "event_date": "2026-10-07", "fact_ids": ["f1", "f2"] },
            "failure": {
                "type": "verdict_mismatch",
                "detail": "阈值偏差",
                "observed": 0.6,
                "expected": "<=0.5"
            }
        })
    }

    /// 端到端：注册管理员 → 发 writeback scope key → 收件（201+queued）→
    /// 队列可查（租户作用域）→ 审计面落账（GET /llm/audits 含 wb- 记录）。
    #[tokio::test]
    async fn test_writeback_receipt_end_to_end() {
        let store = RuleStore::in_memory().expect("store");
        store
            .ensure_default_tenant("tenant_a", "示例组织", "inst-001", "2026-08-22T00:00:00Z")
            .expect("tenant");
        store
            .ensure_default_org("tenant_a", "示例组织", "2026-08-22T00:00:00Z")
            .expect("org");
        let state = AppState::new(store, "test-secret", "inst-001", "http://127.0.0.1:9");
        let app = router(state.clone());

        // 管理员（store 直注 Admin 角色，同 api::mod tests admin_token 先例）
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        state
            .auth
            .register(
                &state.store,
                "tenant_a",
                "admin",
                "password123",
                crate::model::auth::Role::Admin,
                now,
            )
            .expect("register admin");
        let login = state
            .auth
            .login(&state.store, "tenant_a", "admin", "password123", now)
            .expect("login admin");
        let token = login.access_token;
        let (_, key_body) = send(
            app.clone(),
            "POST",
            "/v1/api_keys",
            Some(&token),
            None,
            Some(json!({ "name": "wb", "scope": "writeback:rule_failure" })),
        )
        .await;
        assert_eq!(key_body["scope"], "writeback:rule_failure", "{key_body}");
        let plain_key = key_body["key"].as_str().expect("plain key").to_string();

        // 收件：schema 逐字段回放（model::writeback 定型结构）→ 201 queued
        let (status, receipt) = send(
            app.clone(),
            "POST",
            "/v1/writeback/rule_failure",
            None,
            Some(&plain_key),
            Some(event_json("tenant_a")),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{receipt}");
        assert_eq!(receipt["received"], true);
        assert_eq!(receipt["status"], "queued");
        assert_eq!(receipt["patch_action"], "not_triggered", "首版只收不触发");

        // 队列可查（T1 追认队列形态，租户作用域）
        let (status, queue) = send(
            app.clone(),
            "GET",
            "/v1/writeback/rule_failure",
            Some(&token),
            None,
            None,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{queue}");
        assert_eq!(queue.as_array().expect("queue").len(), 1);
        assert_eq!(queue[0]["entry_id"], "entry-tax-001");
        assert_eq!(queue[0]["failure_type"], "verdict_mismatch");
        assert_eq!(queue[0]["event"]["event_type"], "rule_failure");

        // 审计面落账可查（llm_audit 族）
        let (_, audits) = send(
            app.clone(),
            "GET",
            "/v1/llm/audits",
            Some(&token),
            None,
            None,
        )
        .await;
        let items = audits["items"].as_array().expect("audits items");
        assert!(
            items
                .iter()
                .any(|a| a["operation"] == "writeback_rule_failure"
                    && a["status"] == "received"
                    && a["request_id"]
                        .as_str()
                        .unwrap_or_default()
                        .starts_with("wb-")),
            "审计面须含 writeback_rule_failure 记录: {audits}"
        );

        // 越权拒收：事件声明租户 ≠ key 租户
        let (status, err) = send(
            app.clone(),
            "POST",
            "/v1/writeback/rule_failure",
            None,
            Some(&plain_key),
            Some(event_json("tenant_b")),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{err}");

        // 类型白名单：MVP 仅 rule_failure
        let mut wrong = event_json("tenant_a");
        wrong["event_type"] = json!("sandbox_heartbeat");
        let (status, err) = send(
            app.clone(),
            "POST",
            "/v1/writeback/rule_failure",
            None,
            Some(&plain_key),
            Some(wrong),
        )
        .await;
        assert_eq!(
            status,
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "{err}"
        );

        // scope 收敛：pull key 不能收件
        let (_, key2) = send(
            app.clone(),
            "POST",
            "/v1/api_keys",
            Some(&token),
            None,
            Some(json!({ "name": "pull", "scope": "pull" })),
        )
        .await;
        let pull_key = key2["key"].as_str().expect("plain").to_string();
        let (status, err) = send(
            app.clone(),
            "POST",
            "/v1/writeback/rule_failure",
            None,
            Some(&pull_key),
            Some(event_json("tenant_a")),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{err}");

        // 队列仍只有首条（拒收不落账）
        let (_, queue) = send(
            app,
            "GET",
            "/v1/writeback/rule_failure",
            Some(&token),
            None,
            None,
        )
        .await;
        assert_eq!(queue.as_array().expect("queue").len(), 1);
    }
}
