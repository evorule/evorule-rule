//! 回写通道收件端点（P1-2/RS-1，补齐路线图批次）：POST /v1/writeback/rule_failure
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
use std::time::Duration;

use crate::api::handlers_auth::now_iso;
use crate::api::{api_key_from_header, ApiError, AppState, AuthContext};
use crate::model::auth::{can, Action};
use crate::model::writeback::{classify_failure, validate_event, FailureClass, RuleFailureEvent};
use axum::extract::Extension;

/// 服务级回写 scope：仅限本端点消费（执行侧 → evorule-rule 单向收件）
pub const API_KEY_SCOPE_WRITEBACK: &str = "writeback:rule_failure";

/// K3 writeback 消费器配置（36 号批）：治理 propose 出口三要素。
/// 凭据仅 env 不落盘（沿发送端 EVORULE_WRITEBACK_* 惯例）。
#[derive(Debug, Clone)]
pub struct K3ProposeConfig {
    /// 治理服务基址（尾 `/` 归一）；propose 路径固定拼
    /// `/api/services/knowledge-propose/invoke`（evo-agent client 同款调用形态）
    pub base_url: String,
    /// X-Api-Key（治理侧 scoped key，entries:propose 族）
    pub api_key: String,
    /// 候选入账目标数据集（knowledge 类）
    pub dataset: String,
}

/// env 解析（AppState::new 一次读取；URL 空/空白 = off）
pub fn read_k3_env_config() -> Option<K3ProposeConfig> {
    build_k3_config(
        std::env::var("EVORULE_K3_PROPOSE_URL").unwrap_or_default(),
        std::env::var("EVORULE_K3_PROPOSE_KEY").unwrap_or_default(),
        std::env::var("EVORULE_K3_PROPOSE_DATASET").unwrap_or_default(),
    )
}

/// 纯函数解析（单测锚点）；URL 空/空白 = None（off）
fn build_k3_config(url: String, api_key: String, dataset: String) -> Option<K3ProposeConfig> {
    let url = url.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return None;
    }
    Some(K3ProposeConfig {
        base_url: url,
        api_key,
        dataset,
    })
}

/// 确定性 entry_id slug（fnv1a64 over `k3|{tenant}|{dataset}|{event_id}`，
/// evo-agent sediment 同款实现；同事件重提议得同 ID = 幂等锚，治理侧
/// 同 entry_id 重提议不产生重复条目）
fn k3_propose_entry_id(tenant_id: &str, dataset_id: &str, event_id: i64) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in format!("k3|{tenant_id}|{dataset_id}|{event_id}").as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("kc-{:016x}", h)
}

/// 候选 entry payload（36 号档 §二.3）：kind=fact（builtin 壳 required=
/// ["statement"]，失败事实归 fact 类知识）；tags 三段定位（消费器/分类/原始类型）；
/// confidence=0.6 规则驱动缺省。statement 36 号档定式：规则 {entry_id} 于
/// {version_used} {failure_type}：{detail}。
fn k3_candidate_entry(
    event: &RuleFailureEvent,
    failure_type: &str,
    class: FailureClass,
    entry_id: &str,
) -> serde_json::Value {
    let detail = event
        .failure
        .as_ref()
        .and_then(|f| f.detail.clone())
        .unwrap_or_default();
    serde_json::json!({
        "entry_id": entry_id,
        "version": 1,
        "tags": ["k3-writeback", class.as_str(), failure_type],
        "payload": {
            "statement": format!(
                "规则 {} 于 {} {}：{}",
                event.entry_id, event.version_used, failure_type, detail
            ),
            "title": format!(
                "K3 writeback: {} @ {}/{}",
                failure_type, event.dataset_id, event.entry_id
            ),
            "confidence": 0.6,
        },
        "schema_ref": "builtin:knowledge/fact",
    })
}

/// 异步消费（收件 201 后 tokio::spawn，不阻塞响应——同 writeback_forward spawn 先例）：
/// 分类已同步落账（classified_class 列），此处完成治理 propose——ureq 同步客户端
/// 走 spawn_blocking；成功补 consumption 两列（幂等），治理拒绝/网络失败 warn 留痕
/// 行保持未消费（fail-soft 红线：绝不影响收件 201 与审计落账）。
/// 治理语义红线：只 propose 入 Draft，不做 auto_transition（沿 35 号批）。
fn spawn_k3_propose(
    store: &std::sync::Arc<crate::store::RuleStore>,
    event_id: i64,
    event: RuleFailureEvent,
    failure_type: String,
    class: FailureClass,
    cfg: K3ProposeConfig,
) {
    let store = store.clone();
    tokio::spawn(async move {
        let entry_id = k3_propose_entry_id(&event.tenant_id, &event.dataset_id, event_id);
        let entry = k3_candidate_entry(&event, &failure_type, class, &entry_id);
        let cause = format!(
            "k3 writeback consumer: failure event {event_id} (type {failure_type}) classified as {} on dataset {}",
            class.as_str(),
            event.dataset_id
        );
        let body = serde_json::json!({
            "dataset_id": cfg.dataset,
            "entry": entry,
            "cause": cause,
            "source_session_id": Option::<String>::None,
        });
        let url = format!("{}/api/services/knowledge-propose/invoke", cfg.base_url);
        let dataset = cfg.dataset.clone();
        let api_key = cfg.api_key.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            ureq::post(&url)
                .timeout(Duration::from_secs(5))
                .set("X-Api-Key", &api_key)
                .send_json(body)
                .map(|r| r.status())
        })
        .await;
        match outcome {
            Ok(Ok(status)) => {
                // 2xx = 候选入账 Draft；回执锚 entry_id@dataset（幂等：同 entry_id 重提议不重复）
                let proposed_ref = format!("{entry_id}@{dataset}");
                let consumed_at = now_iso();
                match store.mark_writeback_consumed(event_id, &proposed_ref, &consumed_at) {
                    Ok(true) => tracing::info!(
                        target: "evorule::writeback",
                        event_id,
                        entry_id = %entry_id,
                        status,
                        "K3 消费：失败事件已分类并提议入治理 Draft"
                    ),
                    Ok(false) => {
                        tracing::debug!(event_id, "K3 消费：事件已消费过（幂等跳过）");
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            event_id,
                            "K3 消费：consumption 落账失败（提议已入账，账面待补）"
                        );
                    }
                }
            }
            Ok(Err(ureq::Error::Status(code, _))) => {
                tracing::warn!(
                    target: "evorule::writeback",
                    event_id,
                    status = code,
                    "K3 消费：治理 propose 被拒（fail-soft，行保持未消费态可查）"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "evorule::writeback",
                    error = %e,
                    event_id,
                    "K3 消费：治理 propose 网络失败（fail-soft，行保持未消费态可查）"
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "evorule::writeback",
                    error = %e,
                    event_id,
                    "K3 消费：propose 任务 join 失败（fail-soft，行保持未消费态可查）"
                );
            }
        }
    });
}

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
    // 4) K3 消费分类（36 号批）：旗标 off = 零分类零列写零外呼（行为与现状
    //    逐字节一致）；on = 同步分类落账 + spawn 异步治理 propose
    let failure_type = event.failure.as_ref().map(|f| f.r#type.clone());
    let k3 = state.k3_propose.clone();
    let class = k3
        .as_ref()
        .map(|_| classify_failure(failure_type.as_deref()));
    let classified_class = class.as_ref().map(|c| c.as_str().to_string());
    // 5) 单事务落账：队列行（T1 追认队列形态）+ 审计面行（llm_op_audit）
    let received_at = now_iso();
    let event_json = serde_json::to_string(&event)
        .map_err(|_| ApiError::unprocessable_entity("事件序列化失败"))?;
    let event_id = state.store.record_writeback_event(
        &key.tenant_id,
        &event.dataset_id,
        &event.entry_id,
        &event.version_used,
        failure_type.as_deref(),
        &event_json,
        &received_at,
        classified_class.as_deref(),
    )?;
    tracing::info!(
        target: "evorule::writeback",
        event_id,
        dataset_id = %event.dataset_id,
        entry_id = %event.entry_id,
        "回写事件收件入队（消费形态随 K3 旗标：off=只收不触发 / on=分类+提议）"
    );
    // 6) 消费出口（off = "not_triggered" 与现状逐字节一致；on = 分类已落账、
    //    propose 已调度 → "classify_only"，提议结果见队列行 consumption 两列）
    let patch_action = match (k3, class) {
        (Some(cfg), Some(class)) => {
            spawn_k3_propose(
                &state.store,
                event_id,
                event,
                failure_type.unwrap_or_else(|| "unspecified".to_string()),
                class,
                cfg,
            );
            "classify_only"
        }
        _ => "not_triggered",
    };
    Ok((
        StatusCode::CREATED,
        Json(WritebackReceipt {
            received: true,
            event_id,
            status: "queued",
            patch_action,
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
    use super::{build_k3_config, K3ProposeConfig};
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

    /// K3 测试底座（36 号批）：in-memory store + 租户/组织 + Admin + writeback
    /// scoped key。返回 (app, state, admin_token, plain_key)。
    async fn setup_app_with_writeback_key() -> (axum::Router, AppState, String, String) {
        let store = RuleStore::in_memory().expect("store");
        store
            .ensure_default_tenant("tenant_a", "示例组织", "inst-001", "2026-08-22T00:00:00Z")
            .expect("tenant");
        store
            .ensure_default_org("tenant_a", "示例组织", "2026-08-22T00:00:00Z")
            .expect("org");
        let state = AppState::new(store, "test-secret", "inst-001", "http://127.0.0.1:9");
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
        let app = router(state.clone());
        let (_, key_body) = send(
            app.clone(),
            "POST",
            "/v1/api_keys",
            Some(&token),
            None,
            Some(json!({ "name": "wb", "scope": "writeback:rule_failure" })),
        )
        .await;
        let plain_key = key_body["key"].as_str().expect("plain key").to_string();
        (app, state, token, plain_key)
    }

    /// K3 配置解析纯函数：URL 归一 + 空白 off（J-K3-3 off 前提）
    #[test]
    fn test_k3_config_build() {
        assert!(build_k3_config(String::new(), "k".into(), "d".into()).is_none());
        assert!(build_k3_config("   ".into(), "k".into(), "d".into()).is_none());
        let cfg = build_k3_config(
            "http://gov:18081/".into(),
            "gov-key".into(),
            "k3-failures".into(),
        )
        .expect("config on");
        assert_eq!(cfg.base_url, "http://gov:18081");
        assert_eq!(cfg.api_key, "gov-key");
        assert_eq!(cfg.dataset, "k3-failures");
    }

    /// J-K3-2 K3-E2E：mockito 治理 mock——收件 201 → 同步分类落账 → 异步
    /// propose → consumption 两列补账（三列齐）→ propose 调用形态四要素。
    #[tokio::test]
    async fn test_k3_consumer_propose_end_to_end() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        // propose 调用形态四要素以匹配器承载（AND 语义）：api key 头 + body
        // （dataset/entry 三列定位 tags/statement 定式/schema_ref/confidence）
        // + cause 英文锚 + entry_id 形态——任一不匹配即 404 → 轮询超时测试红
        let m = server
            .mock("POST", "/api/services/knowledge-propose/invoke")
            .match_header("x-api-key", "gov-key")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "dataset_id": "k3-failures",
                "entry": {
                    "version": 1,
                    "schema_ref": "builtin:knowledge/fact",
                    "tags": ["k3-writeback", "drift", "verdict_mismatch"],
                    "payload": { "confidence": 0.6 },
                },
            })))
            .match_body(Matcher::Regex(
                "规则 entry-tax-001 于 v2.p1 verdict_mismatch".to_string(),
            ))
            .match_body(Matcher::Regex(
                "k3 writeback consumer: failure event \\d+ \\(type verdict_mismatch\\) classified as drift".to_string(),
            ))
            .match_body(Matcher::Regex(
                "\"entry_id\"\\s*:\\s*\"kc-[0-9a-f]{16}\"".to_string(),
            ))
            .with_status(200)
            .with_body(r#"{"lifecycle":"Draft","entry_id":"kc-mock"}"#)
            .create_async()
            .await;

        let (_base_app, mut state, _token, plain_key) = setup_app_with_writeback_key().await;
        state.k3_propose = Some(K3ProposeConfig {
            base_url: server.url(),
            api_key: "gov-key".into(),
            dataset: "k3-failures".into(),
        });
        let app = router(state.clone()); // 带K3配置重挂（底座 app 仅用于签 key）

        // 收件 → 201 + classify_only（分类已落账、propose 已调度）
        let (status, receipt) = send(
            app,
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
        assert_eq!(receipt["patch_action"], "classify_only");

        // 轮询等异步 propose 落账（mark 落账 = spawn 任务完成信号）
        let mut row = None;
        for _ in 0..100 {
            let q = state
                .store
                .list_writeback_events("tenant_a", 10)
                .expect("queue");
            if q.first().map(|r| r.consumed_at.is_some()).unwrap_or(false) {
                row = q.into_iter().next();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let row = row.expect("propose 应在超时内落账（三列齐）");
        // 三列落账（J-K3-2 判据）：verdict_mismatch → Drift
        assert_eq!(row.classified_class.as_deref(), Some("drift"));
        assert!(row.consumed_at.is_some(), "consumed_at 应落账");
        let proposed_ref = row.proposed_ref.expect("proposed_ref 应落账");
        assert!(
            proposed_ref.starts_with("kc-") && proposed_ref.ends_with("@k3-failures"),
            "回执锚形如 kc-{{fnv}}@{{dataset}}: {proposed_ref}"
        );

        // propose 调用形态四要素已由 mock 匹配器承载（assert 0 命中即 panic）
        m.assert_async().await;
    }

    /// J-K3-3 fail-soft：治理 403 → 收件 201 照常 + 行保持未消费
    /// （classified_class 已落账，consumption 两列 NULL）。
    #[tokio::test]
    async fn test_k3_consumer_fail_soft_keeps_row_unconsumed() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", "/api/services/knowledge-propose/invoke")
            .with_status(403)
            .create_async()
            .await;

        let (_base_app, mut state, _token, plain_key) = setup_app_with_writeback_key().await;
        state.k3_propose = Some(K3ProposeConfig {
            base_url: server.url(),
            api_key: "gov-key".into(),
            dataset: "k3-failures".into(),
        });
        let app = router(state.clone()); // 带K3配置重挂

        let (status, receipt) = send(
            app,
            "POST",
            "/v1/writeback/rule_failure",
            None,
            Some(&plain_key),
            Some(event_json("tenant_a")),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{receipt}");
        assert_eq!(receipt["patch_action"], "classify_only");

        // 等 propose 403 路径走完（mockito 无命中数查询 API：固定等待 + 结尾
        // assert 验证 mock 已命中——0 命中即 panic，propose 未发出=测试红）
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        m.assert_async().await;
        let q = state
            .store
            .list_writeback_events("tenant_a", 10)
            .expect("queue");
        let row = q.first().expect("收件行存在");
        assert_eq!(row.classified_class.as_deref(), Some("drift"), "分类已落账");
        assert!(row.consumed_at.is_none(), "治理拒绝 → 行保持未消费");
        assert!(row.proposed_ref.is_none(), "治理拒绝 → 无提议回执锚");
    }

    /// J-K3-3 off 逐字节一致：不配 URL → receipt 四键形态与消费器上线前同、
    /// 零外呼（无 mock server）、三列全 NULL。
    #[tokio::test]
    async fn test_k3_consumer_off_byte_identical_receipt() {
        let (app, mut state, _token, plain_key) = setup_app_with_writeback_key().await;
        state.k3_propose = None; // 显式 off（env 未配时 new 已为 None，双保险）

        let (status, receipt) = send(
            app,
            "POST",
            "/v1/writeback/rule_failure",
            None,
            Some(&plain_key),
            Some(event_json("tenant_a")),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{receipt}");
        // receipt 形态与现状逐字节一致：恰四键 + not_triggered
        let obj = receipt.as_object().expect("receipt object");
        assert_eq!(obj.len(), 4, "键集与消费器上线前一致: {receipt}");
        assert_eq!(receipt["received"], true);
        assert_eq!(receipt["status"], "queued");
        assert_eq!(receipt["patch_action"], "not_triggered", "off=只收不触发");

        // 零列写：三列全 NULL（零外呼由「无 mock server」结构保证）
        let q = state
            .store
            .list_writeback_events("tenant_a", 10)
            .expect("queue");
        let row = q.first().expect("收件行存在");
        assert!(row.classified_class.is_none());
        assert!(row.consumed_at.is_none());
        assert!(row.proposed_ref.is_none());
    }
}
