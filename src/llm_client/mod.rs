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
use crate::model::provenance::Provenance;

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
}
