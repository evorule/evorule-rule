//! 回写通道事件 schema（设计文档 §6：MVP **只定型，不实现采集与闭环**）
//!
//! 生产执行结果回写 evorule-rule，触发"规则失效 → LLM 补丁（`patch_rule`，历史批次后置）→
//! 沙箱验证 → 新版本"闭环（历史批次剧本）。
//!
//! ⚠️ 本模块仅作**结构化类型约定**（设计文档 §6 / §8-3 / §9-3 定案），
//! 不提供收发/落库端点——通道与闭环实现后置（批次 2，与既定设计决策 `patch_rule` 一致）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 回写事件（执行侧 → evorule-rule）。字段严格对齐 设计文档 §6 schema。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleFailureEvent {
    /// 固定 = "rule_failure"（设计文档 §6；扩展事件类型后置）
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

/// 执行上下文（设计文档 §6 `execution_ctx`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCtx {
    /// 业务/事件生效日期（版本解析按事件日期，既定设计决策）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_date: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fact_ids: Vec<String>,
}

/// 失效详情（设计文档 §6 `failure`）
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

/// 设计文档 §6 字段级校验：事件必须命中 `event_type = "rule_failure"`
/// （MVP 只有该类型；扩展类型需演进 `event_type` 判别，逐步放开）。
pub fn validate_event(event: &RuleFailureEvent) -> Result<(), &'static str> {
    if event.event_type != "rule_failure" {
        return Err("回写事件类型暂仅支持 rule_failure（设计文档 §6）");
    }
    Ok(())
}

/// 消费器失败分类（K3 writeback 消费器，36 号批）：S-4 四类语义跨仓复用
/// （语义单源 = evo-agent `replan.rs` NodeFailureClass；跨仓不共享代码，
/// 枚举值序列化形态对齐其 serde rename_all = "snake_case"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// 粒失败：执行本身失败（超时/异常）
    Granule,
    /// 接口失败：产出违反契约面（规则=机器可执行契约，enforce 拦截归此）
    Interface,
    /// 漂移：产出偏离期望语义（verdict 与预期不符）
    Drift,
    /// 能力缺口：重切预算耗尽（发送端已标注，直通）
    CapabilityGap,
}

impl FailureClass {
    /// 序列化标签（snake_case，与 S-4 四类 serde 形态一致）
    pub fn as_str(&self) -> &'static str {
        match self {
            FailureClass::Granule => "granule",
            FailureClass::Interface => "interface",
            FailureClass::Drift => "drift",
            FailureClass::CapabilityGap => "capability_gap",
        }
    }
}

/// 失败分类映射（纯函数表驱动；同输入必同输出——K3 对表判据 J-K3-1 的 SSOT）
///
/// 映射口径（36 号档 §二.2，数据积累后校准）：
/// - `capability_gap` → CapabilityGap（agent S-4 直发自带标签，直通）；
/// - `enforce_violation` → Interface（规则=机器可执行契约，Violation=产出违反
///   契约面；S-4 四类中最贴——替代观点：视为治理合规类不入 S-4，待数据说话）；
/// - `verdict_mismatch` → Drift（产出偏离期望语义）；
/// - `timeout` / `exception` → Granule（执行本身失败）；
/// - 未知/缺 failure → Granule（保守缺省，调用方 warn 留痕）。
pub fn classify_failure(failure_type: Option<&str>) -> FailureClass {
    match failure_type {
        Some("capability_gap") => FailureClass::CapabilityGap,
        Some("enforce_violation") => FailureClass::Interface,
        Some("verdict_mismatch") => FailureClass::Drift,
        Some("timeout") | Some("exception") => FailureClass::Granule,
        _ => FailureClass::Granule,
    }
}

/// 回写事件收件行（P1-2/RS-1：T1 追认队列形态——收件即入队，按收件时间倒序可查；
/// 补丁动作等数据说话，首版只收不触发）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WritebackEventRow {
    /// 队列序号（收件自增）
    pub event_id: i64,
    pub tenant_id: String,
    pub dataset_id: String,
    pub entry_id: String,
    pub version_used: String,
    /// 失效类型冗余列（verdict_mismatch | timeout | exception | ...；查询友好）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_type: Option<String>,
    /// 事件原文（RuleFailureEvent JSON——schema 单源 [`RuleFailureEvent`]，此处不复制结构）
    pub event: Value,
    /// 收件时间（ISO-8601 UTC）
    pub received_at: String,
    /// K3 消费器（36 号批）：失败分类（S-4 四类 snake_case；NULL=旗标关未分类）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classified_class: Option<String>,
    /// 消费完成时点（ISO-8601 UTC；NULL=提议未成功，行保持未消费态可查）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<String>,
    /// 治理侧提议回执锚（entry_id@dataset；NULL=未提议）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_ref: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_failure_mapping_table() {
        // J-K3-1：failure.type 全族×预期四类（映射表 36 号档 §二.2 表驱动 SSOT）
        use FailureClass::*;
        assert_eq!(classify_failure(Some("capability_gap")), CapabilityGap);
        assert_eq!(classify_failure(Some("enforce_violation")), Interface);
        assert_eq!(classify_failure(Some("verdict_mismatch")), Drift);
        assert_eq!(classify_failure(Some("timeout")), Granule);
        assert_eq!(classify_failure(Some("exception")), Granule);
        // 未知类型 / 缺 failure → 保守缺省 Granule（调用方 warn 留痕）
        assert_eq!(classify_failure(Some("unknown_kind")), Granule);
        assert_eq!(classify_failure(None), Granule);

        // serde 形态对齐 S-4 四类（evo-agent replan.rs rename_all="snake_case"）
        assert_eq!(
            serde_json::to_value(FailureClass::CapabilityGap).unwrap(),
            serde_json::json!("capability_gap")
        );
        assert_eq!(
            serde_json::to_value(FailureClass::Interface).unwrap(),
            serde_json::json!("interface")
        );
        assert_eq!(
            serde_json::to_value(FailureClass::Drift).unwrap(),
            serde_json::json!("drift")
        );
        assert_eq!(
            serde_json::to_value(FailureClass::Granule).unwrap(),
            serde_json::json!("granule")
        );
        assert_eq!(FailureClass::CapabilityGap.as_str(), "capability_gap");
    }

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
        // 关键契约字段齐全（设计文档 §6）
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event_type"], "rule_failure");
        assert_eq!(json["version_used"], "v2.p1");
        assert_eq!(json["failure"]["type"], "verdict_mismatch");
        assert!(validate_event(&event).is_ok());

        // omit-when-None：可选字段缺省可省略（对齐历史批次字段要点）
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
