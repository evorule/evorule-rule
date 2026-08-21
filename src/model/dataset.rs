//! RuleDataset（31 号 §3）—— 数据集：治理单元 / 版本单元
//!
//! - 数据集是版本与发布最小单位（决策点②）；
//! - 租户（⑧）、生命周期（④）、版本语义（③）在数据集级挂载；
//! - `dataset_id` 租户内唯一；`lifecycle.state_history` 只增不改（审计即记忆）。

use serde::{Deserialize, Serialize};

use super::dependency::DataDependencies;
use super::lifecycle::Lifecycle;
use super::version::{LawRef, VersionSelection, Versioning};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
