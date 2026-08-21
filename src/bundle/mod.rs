//! 快照包消费层（决策点⑥ · 36 号）：导出 / 导入校验 / 裁剪
//!
//! - **DatasetBundle（快照包）**：资产 ↔ 执行解耦的唯一传输形态（单文件 JSON），只读产物；
//! - **导出**（evorule-rule 侧）：数据集当前版本 + 条目（最新版）→ 快照包，含全包内容哈希防篡改；
//! - **导入校验**（evorule-server 侧前置）：schema → 防篡改 → 符号三方一致 → 版本解析 → 闸门一证据；
//!   落 workspace 与热加载属执行侧（27 号真热加载），本层返回校验通过的运行配置；
//! - **裁剪**（服务公司）：裁剪 = 原版本视图，**不新造版本链**（决策点②），依赖声明随裁剪收缩；
//! - **回写通道 MVP 不实现**：只定 schema（36 号 §6），不实现采集与闭环。
//!
//! 零转译：`entries[].rule_body` 原样 = evorule-server 可执行规则 JSON。

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::model::dataset::RuleDataset;
use crate::model::dependency::{DataDependencies, SourceBinding};
use crate::model::entry::RuleEntry;
use crate::model::provenance::Provenance;
use crate::model::version::{
    LawRef, VersionError, VersionSelection, VersionSelectionMode, Versioning,
};
use crate::resolve::{ResolveError, VersionResolver};
use crate::validate::Validator;

/// 当前支持的快照包 schema 版本
pub const BUNDLE_SCHEMA_VERSION: &str = "1.0";

/// 快照包错误（36 号 §3：失败显式报错，不静默降级）
#[derive(Debug, Error, PartialEq)]
pub enum BundleError {
    #[error("不支持的快照包 schema 版本 `{found}`（当前支持 {BUNDLE_SCHEMA_VERSION}）")]
    UnsupportedSchema { found: String },

    #[error("内容哈希不匹配：包内 `{recorded}` ≠ 实际 `{actual}`（包可能被篡改）")]
    ContentHashMismatch { recorded: String, actual: String },

    #[error("绑定服务 `{service}` 未在数据集 data_dependencies.services 中声明")]
    ServiceNotDeclared { service: String },

    #[error("绑定服务 `{service}` 未在 rule_body 的 io_request 中出现（规则体无此符号引用）")]
    ServiceNotInRuleBody { service: String },

    #[error("auto_by_effective_date 模式需快照包携带 law_ref.effective_from 作为生效基准")]
    MissingEffectiveBase,

    #[error("沙箱验证未通过（tests.verdict={verdict:?}），拒绝导入（闸门一证据）")]
    TestsNotPassed { verdict: TestVerdict },

    #[error("裁剪结果为空（所选条件无匹配条目）")]
    EmptyView,

    #[error("版本解析错误: {0}")]
    Resolve(#[from] ResolveError),

    #[error("版本错误: {0}")]
    Version(#[from] VersionError),
}

/// 沙箱验证结果（闸门一证据，决策点④）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TestVerdict {
    #[default]
    Pass,
    Fail,
}

impl fmt::Display for TestVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestVerdict::Pass => write!(f, "pass"),
            TestVerdict::Fail => write!(f, "fail"),
        }
    }
}

/// 测试证据（36 号 §2：测试用例 + 沙箱验证结果随包携带，导入侧可复核）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BundleTests {
    /// 测试用例引用
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subset: Vec<String>,
    /// 夹具引用
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixtures: Vec<String>,
    /// 沙箱验证结果（pass 才能导入）
    #[serde(default)]
    pub verdict: TestVerdict,
}

/// 裁剪视图引用（36 号 §5：不新造版本链，指向原版本）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRef {
    pub original_dataset_id: String,
    /// 被引用的原版本（= 导出时的 source_version）
    pub view_of_version: String,
}

/// 快照包数据集元数据（36 号 §2 dataset 段）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleDatasetMeta {
    pub dataset_id: String,
    pub name: String,
    pub tenant_id: String,
    /// 真实发布者身份（白标不掩盖，决策点⑨）
    pub instance_id: String,
    pub versioning: Versioning,
    /// 内嵌版本选择配置（决策点③），导入侧合并为运行配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_selection: Option<VersionSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub law_ref: Option<LawRef>,
    /// 裁剪视图引用（非裁剪包为 None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_of: Option<ViewRef>,
}

/// 快照包条目（36 号 §2 entries 段，rule_body 原生 JSON）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleEntry {
    pub entry_id: String,
    /// evorule 原生 JSON，零转译
    pub rule_body: serde_json::Value,
    /// 溯源不丢出处
    pub provenance: Provenance,
    pub domain: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 条目级依赖（裁剪后收缩）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<SourceBinding>,
}

/// 导出审计（36 号 §2 audit 段）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleAudit {
    pub exported_at: String,
    pub exported_by: String,
    /// 导出时的数据集版本（= 发布单位，决策点②）
    pub source_version: String,
    /// 全包哈希（sha256，防篡改）
    #[serde(default)]
    pub content_hash: String,
}

/// 快照包（单文件 JSON，36 号 §2）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetBundle {
    /// 快照包自身版本（演进用）
    pub bundle_schema_version: String,
    pub bundle_id: String,
    pub dataset: BundleDatasetMeta,
    pub entries: Vec<BundleEntry>,
    /// 完整数据依赖声明（决策点⑤；裁剪视图收缩）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dependencies: Option<DataDependencies>,
    pub tests: BundleTests,
    pub audit: BundleAudit,
}

impl DatasetBundle {
    /// 全包内容哈希（sha256:hex）：对排除 `audit.content_hash` 本身的规范化 JSON 计算。
    /// 序列化字段顺序固定（struct 声明序）+ Value 键有序（serde_json 默认 BTreeMap）→ 确定性。
    pub fn compute_content_hash(&self) -> String {
        let mut canonical = self.clone();
        canonical.audit.content_hash.clear();
        let json = serde_json::to_string(&canonical).expect("快照包必然可序列化");
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        format!("sha256:{:x}", hasher.finalize())
    }

    /// 防篡改校验：包内记录的哈希与实际重算是否一致
    pub fn verify_content_hash(&self) -> Result<(), BundleError> {
        let actual = self.compute_content_hash();
        if self.audit.content_hash == actual {
            Ok(())
        } else {
            Err(BundleError::ContentHashMismatch {
                recorded: self.audit.content_hash.clone(),
                actual,
            })
        }
    }
}

/// 导出（evorule-rule 侧，36 号 §2）
pub struct BundleExporter;

impl BundleExporter {
    /// 由数据集当前版本 + 条目（最新版）导出快照包（只读产物）。
    ///
    /// - `tests`：沙箱验证证据（闸门一产出），由调用方如实提供（导出不伪造 verdict）；
    /// - `instance_id`：真实发布者身份（决策点⑨）；
    /// - `source_version = versioning.current`（发布单位 = 数据集版本，决策点②）。
    pub fn export(
        dataset: &RuleDataset,
        entries: &[RuleEntry],
        tests: &BundleTests,
        by: &str,
        at: &str,
        instance_id: &str,
    ) -> DatasetBundle {
        let source_version = dataset.versioning.current.clone();
        let mut bundle = DatasetBundle {
            bundle_schema_version: BUNDLE_SCHEMA_VERSION.into(),
            bundle_id: format!("bundle-{}-{}", dataset.dataset_id, source_version.replace('.', "")),
            dataset: BundleDatasetMeta {
                dataset_id: dataset.dataset_id.clone(),
                name: dataset.name.clone(),
                tenant_id: dataset.tenant_id.clone(),
                instance_id: instance_id.into(),
                versioning: dataset.versioning.clone(),
                version_selection: dataset.version_selection.clone(),
                law_ref: dataset.law_ref.clone(),
                view_of: None,
            },
            entries: entries
                .iter()
                .map(|e| BundleEntry {
                    entry_id: e.entry_id.clone(),
                    rule_body: e.rule_body.clone(),
                    provenance: e.provenance.clone(),
                    domain: e.domain.clone(),
                    tags: e.tags.clone(),
                    dependencies: e.data_source_binding.clone(),
                })
                .collect(),
            data_dependencies: dataset.data_dependencies.clone(),
            tests: tests.clone(),
            audit: BundleAudit {
                exported_at: at.into(),
                exported_by: by.into(),
                source_version,
                content_hash: String::new(),
            },
        };
        let hash = bundle.compute_content_hash();
        bundle.audit.content_hash = hash;
        bundle
    }
}

/// 导入校验结果（36 号 §3：校验通过后的运行配置；落 workspace 与热加载在执行侧）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    pub bundle_id: String,
    pub dataset_id: String,
    pub source_version: String,
    pub selection_mode: VersionSelectionMode,
    /// pinned 已解析出版本；auto 为运行时按事件日期解析（None，33 号）
    pub resolved_version: Option<String>,
    pub entry_count: usize,
    pub verdict: TestVerdict,
}

/// 导入校验（执行侧前置，36 号 §3 流程 1-4 + 闸门一证据）
pub struct BundleImporter;

impl BundleImporter {
    pub fn validate(bundle: &DatasetBundle) -> Result<ImportResult, BundleError> {
        // 1 schema 校验
        if bundle.bundle_schema_version != BUNDLE_SCHEMA_VERSION {
            return Err(BundleError::UnsupportedSchema {
                found: bundle.bundle_schema_version.clone(),
            });
        }
        // 2 防篡改（全包哈希）
        bundle.verify_content_hash()?;
        // 3 版本链完整性（防损坏版本链被导入）
        bundle.dataset.versioning.validate()?;
        // 4 符号三方一致（31 号 §9-3）：rule_body ≡ 条目 dependencies ≡ data_dependencies
        let declared: Vec<String> = bundle
            .data_dependencies
            .as_ref()
            .map(|d| d.services.iter().map(|s| s.service_name.clone()).collect())
            .unwrap_or_default();
        for entry in &bundle.entries {
            let body_services = Validator::io_services_from_rule_body(&entry.rule_body);
            for dep in &entry.dependencies {
                if !declared.contains(&dep.service_name) {
                    return Err(BundleError::ServiceNotDeclared {
                        service: dep.service_name.clone(),
                    });
                }
                if !body_services.contains(&dep.service_name) {
                    return Err(BundleError::ServiceNotInRuleBody {
                        service: dep.service_name.clone(),
                    });
                }
            }
        }
        // 5 版本解析（内嵌 version_selection 合并为运行配置；不可解析 → 显式错误）
        let chain = &bundle.dataset.versioning.chain;
        let selection = bundle.dataset.version_selection.as_ref();
        let mode = selection
            .map(|s| s.mode)
            .unwrap_or(VersionSelectionMode::AutoByEffectiveDate);
        let resolved_version = match selection {
            Some(sel) if sel.mode == VersionSelectionMode::Pinned => {
                Some(VersionResolver::resolve_pinned(sel, chain)?)
            }
            _ => {
                // auto：运行时按事件日期解析（33 号）；导入侧校验生效基准存在
                if bundle
                    .dataset
                    .law_ref
                    .as_ref()
                    .and_then(|l| l.effective_from.as_ref())
                    .is_none()
                {
                    return Err(BundleError::MissingEffectiveBase);
                }
                None
            }
        };
        // 6 闸门一证据：verdict=pass 才能导入（不静默降级）
        if bundle.tests.verdict != TestVerdict::Pass {
            return Err(BundleError::TestsNotPassed {
                verdict: bundle.tests.verdict,
            });
        }
        Ok(ImportResult {
            bundle_id: bundle.bundle_id.clone(),
            dataset_id: bundle.dataset.dataset_id.clone(),
            source_version: bundle.audit.source_version.clone(),
            selection_mode: mode,
            resolved_version,
            entry_count: bundle.entries.len(),
            verdict: bundle.tests.verdict,
        })
    }
}

/// 裁剪（服务公司，36 号 §5）：裁剪 = 原版本视图，不新造版本链
pub struct BundleTrimmer;

impl BundleTrimmer {
    /// 按条目 ID 精确裁剪
    pub fn trim_by_ids(
        bundle: &DatasetBundle,
        keep_ids: &[String],
        by: &str,
        at: &str,
    ) -> Result<DatasetBundle, BundleError> {
        let entries: Vec<BundleEntry> = bundle
            .entries
            .iter()
            .filter(|e| keep_ids.iter().any(|id| id == &e.entry_id))
            .cloned()
            .collect();
        if entries.is_empty() {
            return Err(BundleError::EmptyView);
        }
        Self::build_view(bundle, entries, by, at)
    }

    /// 按领域/标签过滤裁剪（标签命中任一即可）
    pub fn trim_by_filter(
        bundle: &DatasetBundle,
        domain: Option<&str>,
        tags: &[&str],
        by: &str,
        at: &str,
    ) -> Result<DatasetBundle, BundleError> {
        let entries: Vec<BundleEntry> = bundle
            .entries
            .iter()
            .filter(|e| {
                domain.map(|d| e.domain == d).unwrap_or(true)
                    && (tags.is_empty() || tags.iter().any(|t| e.tags.iter().any(|et| et == t)))
            })
            .cloned()
            .collect();
        if entries.is_empty() {
            return Err(BundleError::EmptyView);
        }
        Self::build_view(bundle, entries, by, at)
    }

    /// 构造视图：依赖收缩 + 视图引用 + 审计重算（36 号 §5）
    fn build_view(
        bundle: &DatasetBundle,
        entries: Vec<BundleEntry>,
        by: &str,
        at: &str,
    ) -> Result<DatasetBundle, BundleError> {
        // 依赖收缩：只留被裁规则实际用到的服务；引用未声明服务 → 显式报错（不静默丢弃）
        let used: HashSet<&str> = entries
            .iter()
            .flat_map(|e| e.dependencies.iter().map(|d| d.service_name.as_str()))
            .collect();
        let mut dd = bundle.data_dependencies.clone().unwrap_or_default();
        for svc in &used {
            if !dd.has_service(svc) {
                return Err(BundleError::ServiceNotDeclared {
                    service: svc.to_string(),
                });
            }
        }
        dd.services.retain(|s| used.contains(s.service_name.as_str()));

        let mut view = bundle.clone();
        view.bundle_id = format!("{}_view", bundle.bundle_id);
        view.dataset.view_of = Some(ViewRef {
            original_dataset_id: bundle.dataset.dataset_id.clone(),
            view_of_version: bundle.audit.source_version.clone(),
        });
        view.entries = entries;
        view.data_dependencies = if dd.services.is_empty() && dd.inputs.is_empty() {
            None
        } else {
            Some(dd)
        };
        // 审计重算（新导出者 + 新哈希）
        view.audit.exported_at = at.into();
        view.audit.exported_by = by.into();
        let hash = view.compute_content_hash();
        view.audit.content_hash = hash;
        Ok(view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::dependency::{InputDecl, ServiceDecl};
    use crate::model::lifecycle::Lifecycle;
    use crate::model::version::{BumpKind, VersionSelection, Versioning};

    fn dataset_with_service() -> RuleDataset {
        RuleDataset {
            dataset_id: "ds-tax-2024".into(),
            name: "2024 年度企业所得税合规规则集".into(),
            description: None,
            domain: vec!["tax".into()],
            tags: vec!["合规".into()],
            tenant_id: "org-evorule".into(),
            visibility: crate::model::dataset::Visibility::Public,
            lifecycle: Lifecycle::default(),
            versioning: Versioning::default(),
            law_ref: Some(LawRef {
                document_id: "gov-tax-2023-001".into(),
                law_version: Some("2023 修订版".into()),
                effective_from: Some("2024-01-01".into()),
                effective_to: None,
            }),
            version_selection: Some(VersionSelection {
                mode: VersionSelectionMode::AutoByEffectiveDate,
                pinned_version: None,
                pinned_include_patch: None,
            }),
            data_dependencies: Some(DataDependencies {
                inputs: vec![InputDecl {
                    name: "payroll_event".into(),
                    schema: None,
                    description: None,
                    empty_allowed: false,
                }],
                services: vec![ServiceDecl {
                    service_name: "payroll_svc".into(),
                    io_contract: None,
                    sensitive: false,
                    description: None,
                    template: None,
                }],
            }),
            meta: crate::model::dataset::Meta {
                created_at: "2026-07-01T08:00:00Z".into(),
                created_by: "eng".into(),
                updated_at: None,
                updated_by: None,
            },
        }
    }

    fn entry_with_service(entry_id: &str, domain: &str) -> RuleEntry {
        RuleEntry {
            entry_id: entry_id.into(),
            dataset_id: "ds-tax-2024".into(),
            version: 1,
            status: None,
            provenance: Provenance {
                source: "《企业所得税法》".into(),
                clause: None,
                document_id: None,
                effective_from: Some("2024-01-01".into()),
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: domain.into(),
            tags: vec![domain.into()],
            data_source_binding: vec![SourceBinding {
                rule_ref: "transform[0]".into(),
                service_name: "payroll_svc".into(),
            }],
            consumed_inputs: vec![],
            rule_body: serde_json::json!({
                "rule_id": entry_id,
                "version": "0.1.0",
                "transform": [
                    { "type": "io_request", "params": { "service_name": "payroll_svc" } }
                ]
            }),
            governance: None,
        }
    }

    fn passed_tests() -> BundleTests {
        BundleTests {
            subset: vec!["case-1".into()],
            fixtures: vec!["fx-payroll-1".into()],
            verdict: TestVerdict::Pass,
        }
    }

    fn exported_bundle() -> DatasetBundle {
        let ds = dataset_with_service();
        let entries = vec![
            entry_with_service("entry-tax-001", "tax"),
            entry_with_service("entry-labor-002", "labor"),
        ];
        BundleExporter::export(&ds, &entries, &passed_tests(), "publisher-01", "2026-08-21T12:00:00Z", "org-evorule")
    }

    /// 测试辅助：模拟"按当前内容重新签名导出"（使结构校验前的哈希校验通过）
    fn resign(b: &mut DatasetBundle) {
        let h = b.compute_content_hash();
        b.audit.content_hash = h;
    }

    #[test]
    fn test_export_bundle_fields_and_hash() {
        let bundle = exported_bundle();
        assert_eq!(bundle.bundle_schema_version, "1.0");
        assert_eq!(bundle.audit.source_version, "v1");
        assert_eq!(bundle.dataset.instance_id, "org-evorule");
        assert_eq!(bundle.entries.len(), 2);
        assert!(bundle.audit.content_hash.starts_with("sha256:"));
        // 防篡改校验通过；改一处内容 → 校验失败
        bundle.verify_content_hash().unwrap();
        let mut tampered = bundle.clone();
        tampered.entries[0].rule_body =
            serde_json::json!({"rule_id": "hacked"});
        assert!(matches!(
            tampered.verify_content_hash(),
            Err(BundleError::ContentHashMismatch { .. })
        ));
    }

    #[test]
    fn test_import_valid_bundle() {
        let bundle = exported_bundle();
        let r = BundleImporter::validate(&bundle).unwrap();
        assert_eq!(r.dataset_id, "ds-tax-2024");
        assert_eq!(r.entry_count, 2);
        assert_eq!(r.selection_mode, VersionSelectionMode::AutoByEffectiveDate);
        assert_eq!(r.resolved_version, None); // auto 运行时解析
        assert_eq!(r.verdict, TestVerdict::Pass);
    }

    #[test]
    fn test_import_rejects_unsupported_schema() {
        let mut b = exported_bundle();
        b.bundle_schema_version = "2.0".into();
        match BundleImporter::validate(&b) {
            Err(BundleError::UnsupportedSchema { found }) => assert_eq!(found, "2.0"),
            _ => panic!("expected UnsupportedSchema"),
        }
    }

    #[test]
    fn test_import_rejects_fail_verdict() {
        let mut b = exported_bundle();
        b.tests.verdict = TestVerdict::Fail;
        resign(&mut b);
        let err = BundleImporter::validate(&b).unwrap_err();
        assert!(matches!(err, BundleError::TestsNotPassed { .. }));
    }

    #[test]
    fn test_import_rejects_undeclared_service() {
        let bundle = exported_bundle();
        // 构造：条目依赖未在 data_dependencies 声明的服务
        let mut b = bundle.clone();
        b.entries[0].dependencies[0].service_name = "ghost_svc".into();
        resign(&mut b);
        match BundleImporter::validate(&b) {
            Err(BundleError::ServiceNotDeclared { service }) => assert_eq!(service, "ghost_svc"),
            _ => panic!("expected ServiceNotDeclared"),
        }
    }

    #[test]
    fn test_import_rejects_service_not_in_rule_body() {
        let bundle = exported_bundle();
        let mut b = bundle.clone();
        b.entries[0].rule_body = serde_json::json!({
            "rule_id": "entry-tax-001",
            "transform": []
        });
        resign(&mut b);
        match BundleImporter::validate(&b) {
            Err(BundleError::ServiceNotInRuleBody { service }) => assert_eq!(service, "payroll_svc"),
            _ => panic!("expected ServiceNotInRuleBody"),
        }
    }

    #[test]
    fn test_import_pinned_resolves() {
        let mut b = exported_bundle();
        // 升一个版本并设 pinned
        b.dataset.versioning = b.dataset.versioning.bump(BumpKind::Patch).unwrap(); // v1.p1
        b.dataset.version_selection = Some(VersionSelection {
            mode: VersionSelectionMode::Pinned,
            pinned_version: Some("v1".into()),
            pinned_include_patch: None,
        });
        resign(&mut b);
        let r = BundleImporter::validate(&b).unwrap();
        assert_eq!(r.resolved_version.as_deref(), Some("v1.p1")); // 同主版本最新 Patch
        assert_eq!(r.selection_mode, VersionSelectionMode::Pinned);
    }

    #[test]
    fn test_import_auto_requires_effective_base() {
        let mut b = exported_bundle();
        b.dataset.law_ref = None;
        resign(&mut b);
        let err = BundleImporter::validate(&b).unwrap_err();
        assert!(matches!(err, BundleError::MissingEffectiveBase));
    }

    #[test]
    fn test_trim_by_ids_is_view() {
        let bundle = exported_bundle();
        let view = BundleTrimmer::trim_by_ids(&bundle, &["entry-tax-001".into()], "si-company", "2026-08-21T14:00:00Z")
            .unwrap();
        // 只留 1 条
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].entry_id, "entry-tax-001");
        // 视图引用：指向原数据集与版本，不新造版本号
        let v = view.dataset.view_of.as_ref().unwrap();
        assert_eq!(v.original_dataset_id, "ds-tax-2024");
        assert_eq!(v.view_of_version, "v1");
        assert_eq!(view.dataset.versioning, bundle.dataset.versioning); // 版本链不变
        assert_eq!(view.audit.source_version, "v1");
        // 依赖收缩：payroll_svc 仍在使用；inputs 保留
        assert!(view.data_dependencies.as_ref().unwrap().has_service("payroll_svc"));
        // 审计重算：新导出者 + 新哈希
        assert_eq!(view.audit.exported_by, "si-company");
        view.verify_content_hash().unwrap();
        // 视图仍是合法可导入包
        BundleImporter::validate(&view).unwrap();
    }

    #[test]
    fn test_trim_by_filter_domain() {
        let bundle = exported_bundle();
        let view = BundleTrimmer::trim_by_filter(&bundle, Some("labor"), &[], "si-company", "t").unwrap();
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].domain, "labor");
    }

    #[test]
    fn test_trim_empty_rejected() {
        let bundle = exported_bundle();
        let err = BundleTrimmer::trim_by_ids(&bundle, &["nope".into()], "x", "t").unwrap_err();
        assert!(matches!(err, BundleError::EmptyView));
        let err = BundleTrimmer::trim_by_filter(&bundle, Some("nope"), &[], "x", "t").unwrap_err();
        assert!(matches!(err, BundleError::EmptyView));
    }
}
