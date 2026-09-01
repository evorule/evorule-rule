//! 快照包治理侧集成（决策点⑥ · 36 号）
//!
//! **T1 决策（2026-08-24）**：快照包类型 + 6 项校验链 + 裁剪逻辑 + 版本/依赖/溯源模型
//! 唯一来源已迁至 `evorule-bundle`（SSOT），本模块 re-export 并在治理侧保留
//! **`BundleExporter`**（依赖治理侧 RuleDataset/RuleEntry 的导出映射）。
//!
//! 治理侧校验专属（LLM 边界/状态机迁移/凭据扫描）在 validate/store，不在此处。

pub use evorule_bundle::{
    BundleAudit, BundleDatasetMeta, BundleEntry, BundleError, BundleImporter, BundleTests,
    BundleTrimmer, DatasetBundle, DomainSchemaResolver, EntryFilter, EntryKind, ImportResult,
    TestVerdict, ViewRef, BUNDLE_SCHEMA_VERSION,
};

use crate::model::dataset::RuleDataset;
use crate::model::entry::RuleEntry;
use crate::model::service_catalog::ServiceCatalogEntry;

/// 导出（evorule-rule 侧，36 号 §2）—— 治理侧映射：RuleDataset/RuleEntry → DatasetBundle
pub struct BundleExporter;

impl BundleExporter {
    /// 由数据集当前版本 + 条目（最新版）导出快照包（只读产物）。
    ///
    /// - `tests`：沙箱验证证据（闸门一产出），由调用方如实提供（导出不伪造 verdict）；
    /// - `instance_id`：真实发布者身份（决策点⑨）；
    /// - `source_version = versioning.current`（发布单位 = 数据集版本，决策点②）；
    /// - `catalog`：服务目录（name → 条目）。C3/C4/C6 服务契约 SSOT 下沉：数据集声明的
    ///   服务若缺 version/io_contract/description 从目录补齐；`sensitive` 以目录为权威
    ///   （防止声明降级敏感标记，C6）。补齐在 content_hash 计算之前完成，保证哈希覆盖最终契约。
    pub fn export(
        dataset: &RuleDataset,
        entries: &[RuleEntry],
        tests: &BundleTests,
        by: &str,
        at: &str,
        instance_id: &str,
        catalog: &std::collections::BTreeMap<String, ServiceCatalogEntry>,
    ) -> DatasetBundle {
        let bundle_entries = entries.iter().map(Self::rule_entry_to_bundle).collect();
        Self::finish(dataset, bundle_entries, tests, by, at, instance_id, catalog)
    }

    /// RuleEntry → BundleEntry 映射（导出与 B3 条目查询过滤共用同一视图，防口径漂移）
    pub fn rule_entry_to_bundle(e: &RuleEntry) -> BundleEntry {
        BundleEntry {
            entry_id: e.entry_id.clone(),
            entry_kind: EntryKind::Rule,
            rule_body: e.rule_body.clone(),
            schema_ref: None,
            provenance: e.provenance.clone(),
            domain: e.domain.clone(),
            tags: e.tags.clone(),
            dependencies: e.data_source_binding.clone(),
        }
    }

    /// KnowledgeEntry → BundleEntry 映射（同上，Q12 数据资产条目视图）
    pub fn knowledge_entry_to_bundle(e: &crate::model::knowledge::KnowledgeEntry) -> BundleEntry {
        BundleEntry {
            entry_id: e.entry_id.clone(),
            entry_kind: EntryKind::Knowledge,
            rule_body: e.payload.clone(),
            schema_ref: Some(e.schema_ref.clone()),
            provenance: e.provenance.clone(),
            domain: e.domain.clone(),
            tags: e.tags.clone(),
            dependencies: vec![],
        }
    }

    /// knowledge 数据集导出（Q12 数据资产化 R5）：数据条目 → `entry_kind=Knowledge` bundle。
    /// `rule_body` 字段承载领域 payload（零转译条目体，见 evorule-bundle `EntryKind`）。
    pub fn export_knowledge(
        dataset: &RuleDataset,
        entries: &[crate::model::knowledge::KnowledgeEntry],
        tests: &BundleTests,
        by: &str,
        at: &str,
        instance_id: &str,
        catalog: &std::collections::BTreeMap<String, ServiceCatalogEntry>,
    ) -> DatasetBundle {
        let bundle_entries = entries
            .iter()
            .map(Self::knowledge_entry_to_bundle)
            .collect();
        Self::finish(dataset, bundle_entries, tests, by, at, instance_id, catalog)
    }

    /// 公共收尾：服务契约下沉补齐 + 哈希覆盖（规则/数据导出共用，防逻辑漂移）
    fn finish(
        dataset: &RuleDataset,
        entries: Vec<BundleEntry>,
        tests: &BundleTests,
        by: &str,
        at: &str,
        instance_id: &str,
        catalog: &std::collections::BTreeMap<String, ServiceCatalogEntry>,
    ) -> DatasetBundle {
        let source_version = dataset.versioning.current.clone();
        // C3/C4/C6：服务契约 SSOT 下沉（见 doc 注释）
        let enriched_deps = dataset.data_dependencies.as_ref().map(|dd| {
            let mut dd = dd.clone();
            for s in &mut dd.services {
                if let Some(e) = catalog.get(&s.service_name) {
                    if s.version.is_none() {
                        s.version = Some(e.version.clone());
                    }
                    if s.io_contract.is_none() {
                        s.io_contract = e.io_contract.clone();
                    }
                    if s.description.is_none() {
                        s.description = e.description.clone();
                    }
                    s.sensitive = e.sensitive;
                }
            }
            dd
        });
        let mut bundle = DatasetBundle {
            bundle_schema_version: BUNDLE_SCHEMA_VERSION.into(),
            bundle_id: format!(
                "bundle-{}-{}",
                dataset.dataset_id,
                source_version.replace('.', "")
            ),
            dataset: BundleDatasetMeta {
                dataset_id: dataset.dataset_id.clone(),
                name: dataset.name.clone(),
                tenant_id: dataset.tenant_id.clone(),
                instance_id: instance_id.into(),
                versioning: dataset.versioning.clone(),
                version_selection: dataset.version_selection.clone(),
                law_ref: dataset.law_ref.clone(),
                view_of: None,
                // B5：数据集级 push 事件声明随包携带（导入侧 resolver 门禁强校验）
                event_schemas: dataset.event_schemas.clone(),
            },
            entries,
            data_dependencies: enriched_deps,
            tests: tests.clone(),
            audit: BundleAudit {
                exported_at: at.into(),
                exported_by: by.into(),
                source_version,
                content_hash: String::new(),
                hash_algo: "blake3".into(),
            },
        };
        let hash = bundle.compute_content_hash();
        bundle.audit.content_hash = hash;
        bundle
    }
}
