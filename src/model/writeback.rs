//! 回写通道事件 schema（36 号 §6：MVP **只定型，不实现采集与闭环**）
//!
//! 生产执行结果回写 evorule-rule，触发"规则失效 → LLM 补丁（`patch_rule`，37 号后置）→
//! 沙箱验证 → 新版本"闭环（27 号剧本）。
//!
//! ⚠️ 本模块仅作**结构化类型约定**（36 号 §6 / §8-3 / §9-3 定案），
//! 不提供收发/落库端点——通道与闭环实现后置（批次 2，与决策点⑦ `patch_rule` 一致）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 回写事件（执行侧 → evorule-rule）。字段严格对齐 36 号 §6 schema。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleFailureEvent {
    /// 固定 = "rule_failure"（36 号 §6；扩展事件类型后置）
    pub event_type: String,
    pub tenant_id: String,
    pub dataset_id: String,
    /// 执行时命中的数据集版本/补丁（如 "v2.p1"；runbook 定位依赖）
    pub version_used: String,
    pub entry_id: String,
    /// 事件发生时间（ISO-8601 UTC）
    pub occurred_at: String,
    /// 执行上下文（事件日期 + 参与事实 id，供 LLM 补丁/回溯）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_ctx: Option<ExecutionCtx>,
    /// 失效详情（type + 结构化说明 + 观测/期望值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureDetail>,
}

/// 执行上下文（36 号 §6 `execution_ctx`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCtx {
    /// 业务/事件生效日期（版本解析按事件日期，决策点③）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_date: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fact_ids: Vec<String>,
}

/// 失效详情（36 号 §6 `failure`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureDetail {
    /// verdict_mismatch | timeout | exception | ...
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
}

/// 36 号 §6 字段级校验：事件必须命中 `event_type = "rule_failure"`
/// （MVP 只有该类型；扩展类型需演进 `event_type` 判别，逐步放开）。
pub fn validate_event(event: &RuleFailureEvent) -> Result<(), &'static str> {
    if event.event_type != "rule_failure" {
        return Err("回写事件类型暂仅支持 rule_failure（36 号 §6）");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_failure_schema_roundtrip() {
        let event = RuleFailureEvent {
            event_type: "rule_failure".into(),
            tenant_id: "org-evorule".into(),
            dataset_id: "ds-tax-2024".into(),
            version_used: "v2.p1".into(),
            entry_id: "entry-tax-001".into(),
            occurred_at: "2026-08-21T13:00:00Z".into(),
            execution_ctx: Some(ExecutionCtx {
                event_date: Some("2026-08-21".into()),
                fact_ids: vec!["f1".into(), "f2".into()],
            }),
            failure: Some(FailureDetail {
                r#type: "verdict_mismatch".into(),
                detail: Some("阈值偏差".into()),
                observed: Some(serde_json::json!(0.6)),
                expected: Some(serde_json::json!("<=0.5")),
            }),
        };
        // 关键契约字段齐全（36 号 §6）
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event_type"], "rule_failure");
        assert_eq!(json["version_used"], "v2.p1");
        assert_eq!(json["failure"]["type"], "verdict_mismatch");
        assert!(validate_event(&event).is_ok());

        // omit-when-None：可选字段缺省可省略（对齐 31 号字段要点）
        let minimal = RuleFailureEvent {
            event_type: "rule_failure".into(),
            tenant_id: "org-evorule".into(),
            dataset_id: "ds-tax-2024".into(),
            version_used: "v1".into(),
            entry_id: "entry-tax-001".into(),
            occurred_at: "2026-08-21T13:00:00Z".into(),
            execution_ctx: None,
            failure: None,
        };
        let json_min = serde_json::to_string(&minimal).unwrap();
        // 按 JSON 键名精确判断 omit-when-None（不能用 contains("failure")——event_type 即含该词）
        let v: Value = serde_json::from_str(&json_min).unwrap();
        assert!(v.get("execution_ctx").is_none());
        assert!(v.get("failure").is_none());

        // 类型判别：仅 rule_failure（MVP）
        let mut wrong = minimal.clone();
        wrong.event_type = "sandbox_heartbeat".into();
        assert!(validate_event(&wrong).is_err());
    }
}