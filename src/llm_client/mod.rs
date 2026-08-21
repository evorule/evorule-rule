//! LLM 服务客户端（37 号 决策点⑦：LLM 服务接口 · 消费方契约）
//!
//! 前置依赖：evo-agent serve 模式（07f E1）暴露 `POST /ops/{operation}` 命名操作端点。
//! evorule-rule 不建独立 LLM，仅作为客户端消费该契约（37 号 §2）。
//!
//! 契约要点（37 号 §4）：
//! - 命名操作：`draft_rule` / `gen_tests` / `explain_rule`（MVP 只做这三个，§3）；
//! - 请求骨架：`operation` / `model` / `request_id` / `params`；
//! - 响应骨架：`status` / `result` / `errors` / `llm_generated`（溯源，§4/§8）；
//! - 同步主路径：`status=completed` 直接返回结果，响应体预留 `task_id`（MVP 不实现异步队列，§11.1）。
//!
//! 确定性边界（§5 强约束）：LLM 产出只到 Draft。`draft_rule` 结果经本模块直接构造成
//! `status=Draft` 的候选 `RuleEntry`，并携带 `governance.llm_generated` 溯源（§4 落 draft）。

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::model::entry::RuleEntry;
use crate::model::governance::{Governance, LlmGenerated};
use crate::model::lifecycle::LifecycleStatus;
use crate::model::llm_audit::LlmOpAudit;
use crate::model::provenance::Provenance;
use crate::store::RuleStore;

/// 命名操作标识（37 号 §3）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// 起草：法规文本/需求 + 领域 → 候选规则 JSON（Draft）
    DraftRule,
    /// 测试生成：规则 JSON + 数据依赖契约 → test_cases
    GenTests,
    /// 解释：规则 JSON → 人类可读解释
    ExplainRule,
}

impl Operation {
    /// 与 evo-agent serve 端点路径一致的 URL 段
    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::DraftRule => "draft_rule",
            Operation::GenTests => "gen_tests",
            Operation::ExplainRule => "explain_rule",
        }
    }
}

/// 请求参数（各 op 共享骨架，37 号 §4）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmOpRequest {
    /// 模型标识（可插拔，由调用方请求）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 幂等 / 审计
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// 各 op 专属参数
    pub params: Value,
}

/// 响应（同步主路径，预留 task_id，37 号 §4）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmOpResponse {
    pub operation: String,
    #[serde(default)]
    pub request_id: Option<String>,
    /// MVP 预留：将来异步任务句柄（同步模式为 `completed`）
    #[serde(default)]
    pub task_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub errors: Option<String>,
    /// LLM 溯源（模型/op/时间戳）
    #[serde(default)]
    pub llm_generated: Value,
}

/// 客户端错误
#[derive(Debug, Error)]
pub enum LlmClientError {
    #[error("HTTP 调用失败: {0}")]
    Http(String),
    #[error("服务端返回错误: {0}")]
    Server(String),
    #[error("响应解析失败: {0}")]
    Parse(String),
    #[error("操作未完成: {0}")]
    Pending(String),
}

/// LLM 服务客户端（同步，对齐数据模型层；依赖 evo-agent serve）
#[derive(Debug, Clone)]
pub struct LlmClient {
    base_url: String,
}

impl LlmClient {
    /// 创建客户端。`base_url` 形如 `http://127.0.0.1:8080`（evo-agent serve 地址）。
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// 调用命名操作（同步主路径）。响应体 `status=completed` 才返回结果。
    pub fn call(&self, op: Operation, req: &LlmOpRequest) -> Result<LlmOpResponse, LlmClientError> {
        let url = format!("{}/ops/{}", self.base_url, op.as_str());
        let body = serde_json::to_string(req).map_err(|e| LlmClientError::Parse(e.to_string()))?;

        let resp = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_string(&body)
            .map_err(|e| LlmClientError::Http(e.to_string()))?;

        let body: String = resp
            .into_string()
            .map_err(|e| LlmClientError::Parse(e.to_string()))?;
        let resp: LlmOpResponse = serde_json::from_str(&body)
            .map_err(|e| LlmClientError::Parse(e.to_string()))?;

        if resp.status != "completed" {
            return Err(LlmClientError::Pending(resp.status));
        }
        if let Some(err) = &resp.errors {
            return Err(LlmClientError::Server(err.clone()));
        }
        Ok(resp)
    }

    /// 调用命名操作并落操作级审计（37 号 §8："LLM 每步可审计"）。
    ///
    /// - 无论成功/失败都记录一条 `LlmOpAudit`（含耗时、model、op、request_id）；
    /// - `result_ref`：该 op 产出的条目引用（如 entry_id），审计可溯源到条目；
    /// - 审计写入失败只告警，不掩盖主调用结果（审计是附带能力，非主链路）。
    pub fn call_audited(
        &self,
        store: &RuleStore,
        op: Operation,
        req: &LlmOpRequest,
        result_ref: Option<&str>,
    ) -> Result<LlmOpResponse, LlmClientError> {
        use std::time::Instant;
        let start = Instant::now();
        let outcome = self.call(op, req);
        let duration_ms = start.elapsed().as_millis() as u64;
        let created_at = utc_iso_now();

        let record = |status: &str, error: Option<String>| {
            store.record_llm_audit(&build_audit(
                op, req, status, duration_ms, result_ref, error, &created_at,
            ))
        };

        match &outcome {
            Ok(_) => {
                if let Err(e) = record("completed", None) {
                    tracing::warn!(request_id = ?req.request_id, error = %e, "记录 LLM 操作审计失败");
                }
            }
            Err(e) => {
                if let Err(se) = record("failed", Some(e.to_string())) {
                    tracing::warn!(request_id = ?req.request_id, error = %se, "记录 LLM 操作审计失败");
                }
            }
        }
        outcome
    }

    /// `draft_rule`：法规文本/需求 + 领域 → 候选规则 JSON（Draft，37 号 §5 强约束）
    pub fn draft_rule(
        &self,
        law_text: &str,
        domain: &str,
        model: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<LlmOpResponse, LlmClientError> {
        let req = LlmOpRequest {
            model: model.map(ToOwned::to_owned),
            request_id: request_id.map(ToOwned::to_owned),
            params: json!({
                "law_text": law_text,
                "domain": domain,
            }),
        };
        self.call(Operation::DraftRule, &req)
    }

    /// `gen_tests`：规则 JSON + 数据依赖契约 → test_cases（35 号 §4 用途②）
    pub fn gen_tests(
        &self,
        rule_json: &Value,
        data_contract: &Value,
        model: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<LlmOpResponse, LlmClientError> {
        let req = LlmOpRequest {
            model: model.map(ToOwned::to_owned),
            request_id: request_id.map(ToOwned::to_owned),
            params: json!({
                "rule": rule_json,
                "data_contract": data_contract,
            }),
        };
        self.call(Operation::GenTests, &req)
    }

    /// `explain_rule`：规则 JSON → 人类可读解释（§7 审计价值）
    pub fn explain_rule(
        &self,
        rule_json: &Value,
        model: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<LlmOpResponse, LlmClientError> {
        let req = LlmOpRequest {
            model: model.map(ToOwned::to_owned),
            request_id: request_id.map(ToOwned::to_owned),
            params: json!({ "rule": rule_json }),
        };
        self.call(Operation::ExplainRule, &req)
    }
}

/// `draft_rule` 结果 → 候选 Draft `RuleEntry`（37 号 §5 确定性边界）
///
/// LLM 产出永远只到 Draft；`governance.llm_generated` 落溯源（模型/op/时间戳）。
/// 升 Candidate/Active 必须过闸门一/闸门二（34 号），本函数不做任何状态提升。
pub fn draft_response_to_entry(
    resp: &LlmOpResponse,
    entry_id: &str,
    dataset_id: &str,
    domain: &str,
    provenance: Provenance,
) -> Result<RuleEntry, LlmClientError> {
    let rule = resp
        .result
        .get("rule")
        .ok_or_else(|| LlmClientError::Server("draft_rule 响应缺少 result.rule".to_string()))?;
    let rule = rule.clone();

    let llm_generated = llm_generated_from(resp);
    Ok(RuleEntry {
        entry_id: entry_id.to_string(),
        dataset_id: dataset_id.to_string(),
        version: 1,
        status: Some(LifecycleStatus::Draft), // 强约束：LLM 产出只到 Draft
        provenance,
        domain: domain.to_string(),
        tags: vec![],
        data_source_binding: vec![],
        consumed_inputs: vec![],
        rule_body: rule,
        governance: Some(Governance {
            llm_generated: Some(llm_generated),
            ..Governance::default()
        }),
    })
}

/// 从响应 `llm_generated` 溯源块提取 `LlmGenerated` 治理元数据。
fn llm_generated_from(resp: &LlmOpResponse) -> LlmGenerated {
    let g = &resp.llm_generated;
    LlmGenerated {
        flag: true,
        model: g.get("model").and_then(Value::as_str).map(ToOwned::to_owned),
        op: g.get("operation").and_then(Value::as_str).map(ToOwned::to_owned),
        timestamp: g.get("timestamp").and_then(Value::as_str).map(ToOwned::to_owned),
    }
}

/// 生成幂等/审计 request_id（`evorule-rule-<epoch-ns>-<counter>`）
///
/// 纳秒级时间戳 + 进程内原子序号，保证单进程内唯一（幂等/审计用）。
pub fn make_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("evorule-rule-{}-{}", nanos, n)
}

/// 构造一条 `LlmOpAudit`（纯函数，供 `call_audited` 落库；request_id 缺省时自动生成）。
pub fn build_audit(
    op: Operation,
    req: &LlmOpRequest,
    status: &str,
    duration_ms: u64,
    result_ref: Option<&str>,
    error: Option<String>,
    created_at: &str,
) -> LlmOpAudit {
    LlmOpAudit {
        request_id: req.request_id.clone().unwrap_or_else(make_request_id),
        operation: op.as_str().to_string(),
        model: req.model.clone(),
        status: status.to_string(),
        duration_ms,
        result_ref: result_ref.map(ToOwned::to_owned),
        error,
        created_at: created_at.to_string(),
    }
}

/// 生成 ISO-8601 UTC 时间戳（`YYYY-MM-DDTHH:MM:SSZ`），不引第三方时间库。
pub fn utc_iso_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_utc_secs(secs)
}

/// 纯函数：epoch 秒 → ISO-8601 UTC（可测）
fn format_utc_secs(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let secs_of_day = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, h, mi, s
    )
}

/// 天数（自 1970-01-01 起）→ 公历 (年, 月, 日)（Howard Hinnant civil_from_days 算法）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_draft_response() -> LlmOpResponse {
        LlmOpResponse {
            operation: "draft_rule".to_string(),
            request_id: Some("req-0001".to_string()),
            task_id: None,
            status: "completed".to_string(),
            result: json!({
                "rule": {
                    "rule_id": "tax-2024.001",
                    "version": "0.1.0",
                    "description": "工资总额与考勤不符时触发告警",
                    "transform": []
                }
            }),
            errors: None,
            llm_generated: json!({
                "model": "deepseek-v4",
                "operation": "draft_rule",
                "timestamp": "2026-08-22T10:00:00Z",
            }),
        }
    }

    #[test]
    fn test_operation_as_str() {
        assert_eq!(Operation::DraftRule.as_str(), "draft_rule");
        assert_eq!(Operation::GenTests.as_str(), "gen_tests");
        assert_eq!(Operation::ExplainRule.as_str(), "explain_rule");
    }

    #[test]
    fn test_draft_response_to_entry_creates_draft() {
        let resp = sample_draft_response();
        let entry = draft_response_to_entry(
            &resp,
            "tax-001-rule-01",
            "ds-tax-2024",
            "tax",
            Provenance {
                source: "《企业所得税法》".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
        )
        .unwrap();

        // 强约束：LLM 产出只到 Draft
        assert_eq!(entry.status, Some(LifecycleStatus::Draft));
        assert!(!entry.is_frozen());

        // rule_body 原样落入
        assert_eq!(
            entry.rule_body["rule_id"],
            serde_json::json!("tax-2024.001")
        );

        // governance 携带 LLM 溯源
        let g = entry.governance.as_ref().unwrap();
        assert!(g.is_llm_generated());
        let lg = g.llm_generated.as_ref().unwrap();
        assert_eq!(lg.model.as_deref(), Some("deepseek-v4"));
        assert_eq!(lg.op.as_deref(), Some("draft_rule"));
        assert_eq!(lg.timestamp.as_deref(), Some("2026-08-22T10:00:00Z"));
    }

    #[test]
    fn test_draft_response_missing_rule() {
        let mut resp = sample_draft_response();
        resp.result = json!({}); // 无 rule
        let err = draft_response_to_entry(
            &resp,
            "id",
            "ds",
            "tax",
            Provenance {
                source: "s".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, LlmClientError::Server(_)));
    }

    #[test]
    fn test_make_request_id_unique() {
        let a = make_request_id();
        let b = make_request_id();
        assert_ne!(a, b);
    }

    #[test]
    fn test_format_utc_secs() {
        assert_eq!(format_utc_secs(0), "1970-01-01T00:00:00Z");
        // 2026-08-22T00:00:00Z
        assert_eq!(format_utc_secs(1_787_356_800), "2026-08-22T00:00:00Z");
        // 跨天 + 时分秒：2026-08-22T01:01:01Z
        assert_eq!(format_utc_secs(1_787_356_800 + 3_661), "2026-08-22T01:01:01Z");
        // 2038-01-19T03:14:07Z（i32::MAX，经典边界）
        assert_eq!(format_utc_secs(2_147_483_647), "2038-01-19T03:14:07Z");
    }

    #[test]
    fn test_build_audit_completed() {
        let req = LlmOpRequest {
            model: Some("deepseek-v4".into()),
            request_id: Some("req-0001".into()),
            params: json!({"law_text": "x", "domain": "tax"}),
        };
        let a = build_audit(
            Operation::DraftRule,
            &req,
            "completed",
            1234,
            Some("ds-tax-2024/tax-001-rule-01"),
            None,
            "2026-08-22T10:00:00Z",
        );
        assert_eq!(a.request_id, "req-0001");
        assert_eq!(a.operation, "draft_rule");
        assert_eq!(a.status, "completed");
        assert_eq!(a.duration_ms, 1234);
        assert_eq!(a.result_ref.as_deref(), Some("ds-tax-2024/tax-001-rule-01"));
        assert_eq!(a.error, None);
        assert_eq!(a.created_at, "2026-08-22T10:00:00Z");
    }

    #[test]
    fn test_build_audit_failed_auto_request_id() {
        let req = LlmOpRequest {
            model: None,
            request_id: None, // 缺省 → 自动生成
            params: json!({}),
        };
        let a = build_audit(
            Operation::GenTests,
            &req,
            "failed",
            50,
            None,
            Some("LLM 调用失败".into()),
            "2026-08-22T10:00:00Z",
        );
        assert!(a.request_id.starts_with("evorule-rule-"));
        assert_eq!(a.operation, "gen_tests");
        assert_eq!(a.status, "failed");
        assert_eq!(a.error.as_deref(), Some("LLM 调用失败"));
    }

    #[test]
    fn test_call_audited_records_completed() {
        // 用内存 store 直接验证：审计构造 + 落库链路（call 本身需真实 HTTP，此处只测审计侧）。
        let store = RuleStore::in_memory().unwrap();
        let req = LlmOpRequest {
            model: Some("deepseek-v4".into()),
            request_id: Some("req-audit-1".into()),
            params: json!({"law_text": "x", "domain": "tax"}),
        };
        // 直接经 store 落库（等价于 call_audited 的 completed 分支行为）
        store
            .record_llm_audit(&build_audit(
                Operation::DraftRule,
                &req,
                "completed",
                10,
                Some("entry-1"),
                None,
                "2026-08-22T10:00:00Z",
            ))
            .unwrap();
        let got = store.get_llm_audit("req-audit-1").unwrap().unwrap();
        assert_eq!(got.status, "completed");
        assert_eq!(got.operation, "draft_rule");
        assert_eq!(got.result_ref.as_deref(), Some("entry-1"));
    }

    /// 真实 HTTP 冒烟（方案 B）：连本地 evo-agent serve（mock LLM 模式）验证跨仓契约。
    ///
    /// 前置：`evo-agent serve` 已启动且设置了 `EVO_AGENT_LLM_MOCK_CONTENT`。
    /// 运行：`cargo test -- --ignored test_live_`（默认跳过，避免 CI 无 serve 环境失败）。
    /// base_url 可用 `EVO_AGENT_TEST_BASE_URL` 覆盖（默认 127.0.0.1:8082）。
    fn live_base_url() -> String {
        std::env::var("EVO_AGENT_TEST_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8082".into())
    }

    #[test]
    #[ignore = "需要本地 evo-agent serve（mock 模式）"]
    fn test_call_live_http() {
        let client = LlmClient::new(&live_base_url());
        let req = LlmOpRequest {
            model: Some("deepseek-v4".into()),
            request_id: Some("smoke-1".into()),
            params: json!({"law_text": "x", "domain": "tax"}),
        };
        let resp = client.call(Operation::DraftRule, &req).unwrap();
        assert_eq!(resp.status, "completed");
        assert_eq!(resp.operation, "draft_rule");
        assert!(resp.result.get("rule").is_some(), "draft_rule 应产出 rule");
        assert_eq!(
            resp.llm_generated.get("model").and_then(|m| m.as_str()),
            Some("deepseek-v4")
        );
        assert!(resp.errors.is_none());
    }

    #[test]
    #[ignore = "需要本地 evo-agent serve（mock 模式）"]
    fn test_call_audited_live_records_audit() {
        let client = LlmClient::new(&live_base_url());
        let store = RuleStore::in_memory().unwrap();
        let req = LlmOpRequest {
            model: Some("deepseek-v4".into()),
            request_id: Some("smoke-audit-1".into()),
            params: json!({"law_text": "x", "domain": "tax"}),
        };
        let resp = client
            .call_audited(&store, Operation::DraftRule, &req, Some("ds-tax/smoke-rule-01"))
            .unwrap();
        assert_eq!(resp.status, "completed");
        // 审计已落库（completed + result_ref 溯源到条目）
        let got = store.get_llm_audit("smoke-audit-1").unwrap().unwrap();
        assert_eq!(got.status, "completed");
        assert_eq!(got.operation, "draft_rule");
        assert_eq!(got.result_ref.as_deref(), Some("ds-tax/smoke-rule-01"));
        assert!(got.duration_ms >= 0);
    }
}
