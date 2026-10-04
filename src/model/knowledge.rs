//! KnowledgeEntry（Q12 数据资产化 R2，方案 D）
//!
//! 与 [`RuleEntry`] 平行的数据资产条目载荷模型：
//! - `payload` = 领域结构化 JSON（零转译，任意领域本体，如 rpsm 物理仿真场景）；
//! - `schema_ref` = 领域 JSON Schema 引用 URI（领域仓资产，D3 强校验锚）；
//! - **不进 TCB**：无 transform 指令集、无服务绑定（数据条目不经 io_request 消费服务）；
//! - 生命周期/审批/发布完全复用数据集级机制（entry 级状态继承数据集，同 RuleEntry 现状）；
//! - 不可变约束同 RuleEntry：进入 Active/Published 不可原地修改，修改 = 新版本。
//!
//! 内容哈希与 RuleEntry 同源（BLAKE3，evorule-hash），去重语义一致（设计文档 §6）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::governance::Governance;
use super::lifecycle::LifecycleStatus;
use super::provenance::Provenance;
use evorule_bundle::ExecutionContract;
use evorule_hash;

/// knowledge_entries.knowledge_meta 列的 JSON 载荷（知识资产化批次 A 存储位）：
/// 四字段打包单列存储，存量行 NULL = 全 None（动态迁移零成本，同 consumed_inputs 先例）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct KnowledgeMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_contract: Option<ExecutionContract>,
}

impl KnowledgeEntry {
    /// 打包知识资产化四字段为存储列载荷（全 None → 存 NULL）
    pub fn knowledge_meta(&self) -> KnowledgeMeta {
        KnowledgeMeta {
            knowledge_kind: self.knowledge_kind.clone(),
            trust_level: self.trust_level.clone(),
            license_ref: self.license_ref.clone(),
            execution_contract: self.execution_contract.clone(),
        }
    }
}

/// 知识 kind 谱系合法值（知识资产化批次 A；总纲 §3.1 五分法 + `custom:{name}` 扩展位）
pub const KNOWLEDGE_KINDS: [&str; 5] = ["fact", "procedure", "heuristic", "narrative", "model"];

/// trust_level 合法前缀：`human` | `llm` | `external:{source}`
pub fn is_valid_trust_level(level: &str) -> bool {
    level == "human"
        || level == "llm"
        || level
            .strip_prefix("external:")
            .is_some_and(|s| !s.is_empty())
}

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
    /// 知识 kind 谱系（fact|procedure|heuristic|narrative|model|custom:{name}）；
    /// 缺省 None = 旧格式兼容（无 kind 走旧通路，不强制迁移——存量不追溯）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_kind: Option<String>,
    /// 来源信任级（human | llm | external:{source}）；缺省 None = 未声明
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
    /// 许可证域引用（trust_level=external:* 时必填——入账校验）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_ref: Option<String>,
    /// 知识运行契约（pathway/criterion_ref/consumer_allowlist/budget_class）；
    /// 缺省 None = 契约未声明（只可 Draft，不给行权——契约完整性校验）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_contract: Option<ExecutionContract>,
}

impl KnowledgeEntry {
    /// 是否已进入不可变态（Active / Published → 内容不可变；语义同 RuleEntry）
    pub fn is_frozen(&self) -> bool {
        matches!(
            self.status.unwrap_or(LifecycleStatus::Active),
            LifecycleStatus::Active | LifecycleStatus::Published
        )
    }

    /// 内容哈希（未变条目按内容哈希去重存储，既定设计决策/§10；与 RuleEntry 同源 BLAKE3）
    /// 只对 payload 做去重哈希（治理元数据与 schema_ref 引用不参与——引用变更不改数据本体）。
    /// 知识资产化四字段（kind/trust/license/contract）同属治理元数据，不参与。
    pub fn content_hash(&self) -> String {
        evorule_hash::prefixed(&evorule_hash::json_digest(&self.payload))
    }

    /// LLM 产出条目只能停留 Draft（历史批次强约束，同 RuleEntry 口径）
    pub fn is_llm_generated(&self) -> bool {
        self.governance
            .as_ref()
            .map(|g| g.is_llm_generated())
            .unwrap_or(false)
    }

    /// 契约完整性（A 批闸件，知识资产化批次 A §3.2）：
    /// kind 合法 + trust_level 合法 + external 必带 license_ref + contract 字段完整。
    /// 返回 Err(原因) = 契约不完整 → 只可 Draft（行权闸在 lifecycle 迁移处消费本方法）。
    pub fn validate_ingest_contract(&self) -> Result<(), String> {
        if let Some(kind) = &self.knowledge_kind {
            let known = KNOWLEDGE_KINDS.contains(&kind.as_str())
                || kind.strip_prefix("custom:").is_some_and(|n| !n.is_empty());
            if !known {
                return Err(format!("unknown knowledge_kind: {kind}"));
            }
            // builtin schema_ref 一致性闸（A1-1b）：schema_ref 指向内置知识壳时，
            // 后缀必须与 kind 一致（防 fact 条目挂 model 壳的错配入账）；
            // 无 kind 条目不受此约束（旧通路不追溯）。
            if let Some(builtin_kind) = self.schema_ref.strip_prefix("builtin:knowledge/") {
                if builtin_kind != kind {
                    return Err(format!(
                        "schema_ref builtin:knowledge/{builtin_kind} mismatches knowledge_kind: {kind}"
                    ));
                }
            }
        }
        if let Some(level) = &self.trust_level {
            if !is_valid_trust_level(level) {
                return Err(format!("invalid trust_level: {level}"));
            }
            if level.starts_with("external:") && self.license_ref.is_none() {
                return Err("external source requires license_ref".into());
            }
        }
        if let Some(c) = &self.execution_contract {
            if c.pathway.is_empty() {
                return Err("execution_contract.pathway must not be empty".into());
            }
            match c.pathway.as_str() {
                "direct" | "injection" | "criterion" => {}
                other => return Err(format!("invalid execution_contract.pathway: {other}")),
            }
            if c.consumer_allowlist.is_empty() {
                return Err("execution_contract.consumer_allowlist must not be empty".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entry::RuleEntry;

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
            knowledge_kind: Some("fact".into()),
            trust_level: Some("human".into()),
            license_ref: None,
            execution_contract: Some(ExecutionContract {
                pathway: "injection".into(),
                criterion_ref: None,
                consumer_allowlist: vec!["*".into()],
                budget_class: "default".into(),
            }),
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
    fn test_knowledge_legacy_json_backward_compat() {
        // 旧格式 JSON（无知识资产化四字段）反序列化 → 全 None（存量不追溯）
        let legacy = r#"{
            "entry_id": "k1", "dataset_id": "d1", "version": 1,
            "provenance": {"source": "s"},
            "domain": "d", "tags": [],
            "payload": {"v": 1},
            "schema_ref": "u"
        }"#;
        let e: KnowledgeEntry = serde_json::from_str(legacy).expect("旧格式必须可解析");
        assert!(e.knowledge_kind.is_none());
        assert!(e.trust_level.is_none());
        assert!(e.license_ref.is_none());
        assert!(e.execution_contract.is_none());
        assert!(
            e.validate_ingest_contract().is_ok(),
            "旧格式缺省=契约未声明态合法入账"
        );
    }

    #[test]
    fn test_ingest_contract_validation() {
        // kind 非法
        let mut e = sample();
        e.knowledge_kind = Some("nonsense".into());
        assert!(e.validate_ingest_contract().is_err());
        // custom: 扩展位合法
        e.knowledge_kind = Some("custom:playbook".into());
        assert!(e.validate_ingest_contract().is_ok());
        // trust_level 非法
        let mut e = sample();
        e.trust_level = Some("alien".into());
        assert!(e.validate_ingest_contract().is_err());
        // external 无 license_ref
        let mut e = sample();
        e.trust_level = Some("external:partner-x".into());
        assert!(e.validate_ingest_contract().is_err());
        e.license_ref = Some("Apache-2.0".into());
        assert!(e.validate_ingest_contract().is_ok());
        // pathway 非法
        let mut e = sample();
        e.execution_contract.as_mut().unwrap().pathway = "telepathy".into();
        assert!(e.validate_ingest_contract().is_err());
        // allowlist 空
        let mut e = sample();
        e.execution_contract.as_mut().unwrap().consumer_allowlist = vec![];
        assert!(e.validate_ingest_contract().is_err());
        // criterion 通路带判据引用
        let mut e = sample();
        e.execution_contract.as_mut().unwrap().pathway = "criterion".into();
        e.execution_contract.as_mut().unwrap().criterion_ref = Some("criterion://x/1".into());
        assert!(e.validate_ingest_contract().is_ok());
        // 无契约无 kind = 契约未声明态（旧通路），合法
        let mut e = sample();
        e.knowledge_kind = None;
        e.execution_contract = None;
        assert!(e.validate_ingest_contract().is_ok());
    }

    #[test]
    fn test_builtin_schema_ref_kind_consistency() {
        // A1-1b 一致性闸：schema_ref 指向内置知识壳时后缀必须与 kind 一致
        // POS：kind 与 builtin 后缀一致
        let mut e = sample();
        e.knowledge_kind = Some("fact".into());
        e.schema_ref = "builtin:knowledge/fact".into();
        assert!(e.validate_ingest_contract().is_ok());
        // NEG：fact 条目挂 model 壳 → 错配拒绝
        let mut e = sample();
        e.knowledge_kind = Some("fact".into());
        e.schema_ref = "builtin:knowledge/model".into();
        let err = e.validate_ingest_contract().unwrap_err();
        assert!(err.contains("mismatches knowledge_kind"), "err={err}");
        // 非 builtin URI（领域目录件）不受闸约束
        let mut e = sample();
        e.knowledge_kind = Some("fact".into());
        e.schema_ref = "https://rpsm.example/schemas/scenario/v1.0.json".into();
        assert!(e.validate_ingest_contract().is_ok());
        // 无 kind 条目挂 builtin 也不受闸（旧通路不追溯）
        let mut e = sample();
        e.schema_ref = "builtin:knowledge/fact".into();
        assert!(e.validate_ingest_contract().is_ok());
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
            knowledge_kind: None,
            trust_level: None,
            license_ref: None,
            execution_contract: None,
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
