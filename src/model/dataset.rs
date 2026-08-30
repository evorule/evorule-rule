//! RuleDataset（31 号 §3）—— 数据集：治理单元 / 版本单元
//!
//! - 数据集是版本与发布最小单位（决策点②）；
//! - 租户（⑧）、生命周期（④）、版本语义（③）在数据集级挂载；
//! - `dataset_id` 租户内唯一；`lifecycle.state_history` 只增不改（审计即记忆）。

use serde::{Deserialize, Serialize};

use super::dependency::DataDependencies;
use super::lifecycle::Lifecycle;
use super::version::{LawRef, VersionSelection, Versioning};

/// 数据集类型（Q12 数据资产化 R1，方案 D）
///
/// - `RuleSet`（默认）：规则集数据集，条目为 transform 指令集 → 进 TCB 确定性执行；
/// - `Knowledge`：知识/数据资产数据集，条目为领域结构化 payload + schema_ref
///   → 不进 TCB，供领域服务消费（治理链 5 态/双闸门/版本/发布完全复用）。
///
/// serde default = `RuleSet`：存量数据集（无 dataset_kind 列/字段）零迁移成本。
/// 类型在创建时确定，创建后不可变更（防规则/数据载荷混装同一数据集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DatasetKind {
    #[default]
    RuleSet,
    Knowledge,
}

impl DatasetKind {
    /// SQLite 列存储值（'rule_set' | 'knowledge'）
    pub fn as_str(&self) -> &'static str {
        match self {
            DatasetKind::RuleSet => "rule_set",
            DatasetKind::Knowledge => "knowledge",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rule_set" => Some(DatasetKind::RuleSet),
            "knowledge" => Some(DatasetKind::Knowledge),
            _ => None,
        }
    }
}

/// 可见性（决策点⑧）：MVP 两档（private/public），shared 后置
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Private,
    Public,
}

/// 数据集元数据
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub created_at: String,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
}

/// 数据集
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleDataset {
    /// 租户内唯一
    pub dataset_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 数据集类型（Q12 R1：rule_set = transform 规则集；knowledge = 数据资产集）
    #[serde(default)]
    pub dataset_kind: DatasetKind,
    /// 契约（types.ts GovernanceDataset）：必填数组 —— 空也必须输出 `[]`，不得省略
    #[serde(default)]
    pub domain: Vec<String>,
    /// 契约（types.ts GovernanceDataset）：必填数组 —— 空也必须输出 `[]`，不得省略
    #[serde(default)]
    pub tags: Vec<String>,
    pub tenant_id: String,
    #[serde(default)]
    pub visibility: Visibility,
    /// 生命周期（决策点④）
    #[serde(default)]
    pub lifecycle: Lifecycle,
    /// 版本链（决策点③）
    #[serde(default)]
    pub versioning: Versioning,
    /// 法规锚（合规场景）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub law_ref: Option<LawRef>,
    /// 版本选择双模式（决策点③）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_selection: Option<VersionSelection>,
    /// 数据依赖声明（决策点⑤，完整 schema 在 35 号）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dependencies: Option<DataDependencies>,
    pub meta: Meta,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dataset_serde_roundtrip() {
        let ds = RuleDataset {
            dataset_id: "ds-tax-2024".into(),
            name: "2024 年度企业所得税合规规则集".into(),
            description: Some("依据《企业所得税法》2023 修订版结构化形成的合规规则集".into()),
            domain: vec!["tax".into(), "corporate_income_tax".into()],
            tags: vec!["合规".into(), "税务".into()],
            tenant_id: "org-evorule".into(),
            visibility: Visibility::Public,
            dataset_kind: DatasetKind::RuleSet,
            lifecycle: Lifecycle::default(),
            versioning: Versioning::default(),
            law_ref: None,
            version_selection: None,
            data_dependencies: None,
            meta: Meta {
                created_at: "2026-07-01T08:00:00Z".into(),
                created_by: "knowledge-eng-01".into(),
                updated_at: None,
                updated_by: None,
            },
        };
        let json = serde_json::to_string_pretty(&ds).unwrap();
        let back: RuleDataset = serde_json::from_str(&json).unwrap();
        assert_eq!(ds, back);
    }
}
