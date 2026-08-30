//! KnowledgeEntry（Q12 数据资产化 R2，方案 D）
//!
//! 与 [`RuleEntry`] 平行的数据资产条目载荷模型：
//! - `payload` = 领域结构化 JSON（零转译，任意领域本体，如 rpsm 物理仿真场景）；
//! - `schema_ref` = 领域 JSON Schema 引用 URI（领域仓资产，D3 强校验锚）；
//! - **不进 TCB**：无 transform 指令集、无服务绑定（数据条目不经 io_request 消费服务）；
//! - 生命周期/审批/发布完全复用数据集级机制（entry 级状态继承数据集，同 RuleEntry 现状）；
//! - 不可变约束同 RuleEntry：进入 Active/Published 不可原地修改，修改 = 新版本。
//!
//! 内容哈希与 RuleEntry 同源（BLAKE3，evorule-hash），去重语义一致（33 号 §6）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::governance::Governance;
use super::lifecycle::LifecycleStatus;
use super::provenance::Provenance;
use evorule_hash;

/// 数据资产条目（knowledge 数据集专属载荷）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    /// 数据集内唯一
    pub entry_id: String,
    pub dataset_id: String,
    /// 条目治理版本：整型单调递增（同 RuleEntry）
    pub version: u32,
    /// 顶层状态：默认继承数据集，允许条目级 Draft（正在录入）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<LifecycleStatus>,
    /// 溯源（§5，必填；数据资产的出处同样可审计）
    pub provenance: Provenance,
    /// 领域（与数据集 domain 一致，供检索/裁剪）
    pub domain: String,
    /// 契约（types.ts GovernanceEntry）：必填数组 —— 空也必须输出 `[]`，不得省略
    #[serde(default)]
    pub tags: Vec<String>,
    /// 领域结构化数据本体（任意 JSON）——由 schema_ref 指向的领域 JSON Schema 强校验
    pub payload: Value,
    /// 领域 JSON Schema 引用 URI（领域仓资产，D3；resolver 未命中 = 拒绝入库）
    pub schema_ref: String,
    /// 治理补充信息（author/updater/llm_generated/lifecycle_timestamps）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governance: Option<Governance>,
}

impl KnowledgeEntry {
    /// 是否已进入不可变态（Active / Published → 内容不可变；语义同 RuleEntry）
    pub fn is_frozen(&self) -> bool {
        matches!(
            self.status.unwrap_or(LifecycleStatus::Active),
            LifecycleStatus::Active | LifecycleStatus::Published
        )
    }

    /// 内容哈希（未变条目按内容哈希去重存储，决策点③/§10；与 RuleEntry 同源 BLAKE3）
    /// 只对 payload 做去重哈希（治理元数据与 schema_ref 引用不参与——引用变更不改数据本体）。
    pub fn content_hash(&self) -> String {
        evorule_hash::prefixed(&evorule_hash::json_digest(&self.payload))
    }

    /// LLM 产出条目只能停留 Draft（37 号强约束，同 RuleEntry 口径）
    pub fn is_llm_generated(&self) -> bool {
        self.governance
            .as_ref()
            .map(|g| g.is_llm_generated())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use crate::model::entry::RuleEntry;
    use super::*;

    fn sample() -> KnowledgeEntry {
        KnowledgeEntry {
            entry_id: "rpsm-scenario-01".into(),
            dataset_id: "ds-rpsm-scenarios".into(),
            version: 1,
            status: Some(LifecycleStatus::Draft),
            provenance: Provenance {
                source: "rpsm 场景建模".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "physics".into(),
            tags: vec!["场景".into()],
            payload: serde_json::json!({
                "scenario_id": "free-fall-01",
                "bodies": [{ "id": "ball", "mass": 2.0, "p": [0.0, 10.0], "v": [0.0, 0.0] }],
                "gravity": 9.8
            }),
            schema_ref: "https://rpsm.example/schemas/scenario/v1.0.json".into(),
            governance: None,
        }
    }

    #[test]
    fn test_knowledge_entry_serde_roundtrip() {
        let e = sample();
        let json = serde_json::to_string_pretty(&e).unwrap();
        let back: KnowledgeEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn test_knowledge_content_hash_stable() {
        let a = sample();
        let mut b = sample();
        b.tags.push("额外标签".into()); // 治理元数据变化不影响内容哈希
        assert_eq!(a.content_hash(), b.content_hash());
        b.payload = serde_json::json!({"changed": true});
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn test_knowledge_is_frozen() {
        let mut e = sample();
        e.status = Some(LifecycleStatus::Active);
        assert!(e.is_frozen());
        e.status = Some(LifecycleStatus::Draft);
        assert!(!e.is_frozen());
    }

    #[test]
    fn test_knowledge_hash_algo_aligned_with_rule_entry() {
        // 与 RuleEntry 同源口径：payload/rule_body 相同 JSON → 相同哈希
        let body = serde_json::json!({"k": 1});
        let ke = KnowledgeEntry {
            entry_id: "x".into(),
            dataset_id: "d".into(),
            version: 1,
            status: None,
            provenance: Provenance {
                source: "s".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "d".into(),
            tags: vec![],
            payload: body.clone(),
            schema_ref: "u".into(),
            governance: None,
        };
        let re = RuleEntry {
            entry_id: "x".into(),
            dataset_id: "d".into(),
            version: 1,
            status: None,
            provenance: ke.provenance.clone(),
            domain: "d".into(),
            tags: vec![],
            data_source_binding: vec![],
            consumed_inputs: vec![],
            rule_body: body,
            governance: None,
        };
        assert_eq!(ke.content_hash(), re.content_hash());
    }
}
