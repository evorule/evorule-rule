//! RuleEntry（31 号 §4，重点，同时回答基线 D-1）
//!
//! - `rule_body` = evorule 原生 JSON（零转译，锚定 10_role13_demo.json 结构）；
//! - 版本双维度分离：治理层 `version`（整型单调递增）vs `rule_body.version`（evorule 自身语义化版本）；
//! - 条目级 `status` 默认继承数据集，仅"正在起草的新规则"可为 Draft；
//! - 不可变约束：进入 Active/Published 的条目不可原地修改，修改 = 新版本（快照模式）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::dependency::SourceBinding;
use super::governance::Governance;
use super::lifecycle::LifecycleStatus;
use super::provenance::Provenance;
use evorule_hash;

/// 规则条目
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleEntry {
    /// 数据集内唯一
    pub entry_id: String,
    pub dataset_id: String,
    /// 条目治理版本：整型单调递增（非语义化，法规版本是自然锚）
    pub version: u32,
    /// 顶层状态：默认继承数据集，允许条目级 Draft（正在起草）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<LifecycleStatus>,
    /// 溯源（§5，必填）
    pub provenance: Provenance,
    /// 领域（与数据集 domain 一致，供检索/裁剪）
    pub domain: String,
    /// 契约（types.ts GovernanceEntry）：必填数组 —— 空也必须输出 `[]`，不得省略
    #[serde(default)]
    pub tags: Vec<String>,
    /// 规则体→服务 绑定映射（§6）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_source_binding: Vec<SourceBinding>,
    /// 本条目消费的推入式输入符号（数据集 `data_dependencies.inputs` 的子集；35 号 §4）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumed_inputs: Vec<String>,
    /// evorule 原生 JSON，零转译（锚定 10_role13_demo.json）
    pub rule_body: Value,
    /// 治理补充信息（author/updater/llm_generated/lifecycle_timestamps）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governance: Option<Governance>,
}

impl RuleEntry {
    /// 是否已进入不可变态（Active / Published → 内容不可变）
    pub fn is_frozen(&self) -> bool {
        matches!(
            self.status.unwrap_or(LifecycleStatus::Active),
            LifecycleStatus::Active | LifecycleStatus::Published
        )
    }

    /// 内容哈希（未变条目按内容哈希去重存储，决策点③/§10）
    /// 统一为 BLAKE3（blake3 crate），与 evorule-reactor 审计链同源；`blake3:` 前缀自描述。
    pub fn content_hash(&self) -> String {
        // 只对可执行体做去重哈希（治理元数据不参与）
        evorule_hash::prefixed(&evorule_hash::json_digest(&self.rule_body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> RuleEntry {
        RuleEntry {
            entry_id: "tax-001-rule-01".into(),
            dataset_id: "ds-tax-2024".into(),
            version: 3,
            status: Some(LifecycleStatus::Active),
            provenance: Provenance {
                source: "《企业所得税法》".into(),
                clause: Some("第 X 条第 Y 款".into()),
                document_id: Some("gov-tax-2023-001".into()),
                effective_from: Some("2024-01-01".into()),
                effective_to: None,
                last_verified: Some("2026-08-01".into()),
                verified_by: Some("auditor-01".into()),
            },
            domain: "tax".into(),
            tags: vec!["企业所得税".into()],
            data_source_binding: vec![],
            consumed_inputs: vec![],
            rule_body: serde_json::json!({
                "rule_id": "tax-2024.001",
                "version": "0.1.0",
                "description": "工资总额与考勤不符时触发告警",
                "transform": []
            }),
            governance: None,
        }
    }

    #[test]
    fn test_entry_serde_roundtrip() {
        let e = sample_entry();
        let json = serde_json::to_string_pretty(&e).unwrap();
        let back: RuleEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn test_is_frozen() {
        let mut e = sample_entry();
        assert!(e.is_frozen()); // Active
        e.status = Some(LifecycleStatus::Draft);
        assert!(!e.is_frozen());
    }

    #[test]
    fn test_content_hash_stable() {
        let a = sample_entry();
        let mut b = sample_entry();
        b.tags.push("额外标签".into()); // 治理元数据变化不影响内容哈希
        assert_eq!(a.content_hash(), b.content_hash());
        b.rule_body = serde_json::json!({"rule_id": "changed"});
        assert_ne!(a.content_hash(), b.content_hash());
    }
}
