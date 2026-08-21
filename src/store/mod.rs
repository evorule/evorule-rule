//! SQLite 存储层（31 号 §8）
//!
//! 存储策略：SQLite（索引/元数据）+ JSON（rule_body 原样保存，零转译）。
//! - 表：`datasets`（元数据列 + JSON 块）、`entries`（rule_body 以 JSON 文本列保存）；
//! - 索引：domain/tags（检索）、entry_id+version（版本链查询）、tenant_id+visibility（多租户，⑧）；
//! - 约束：唯一性、不可变性（frozen 拒绝原地修改）、符号三方一致（导入/提交时校验，显式报错）。

use rusqlite::{Connection, params};
use thiserror::Error;

use crate::bundle::{BundleError, BundleExporter, BundleTests, DatasetBundle};
use crate::model::dataset::{RuleDataset, Visibility};
use crate::model::entry::RuleEntry;
use crate::model::lifecycle::{Lifecycle, LifecycleStatus, StateChange};
use crate::model::version::{BumpKind, VersionError};
use crate::validate::{ValidationError, Validator};

/// 存储错误
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("校验失败: {0}")]
    Validation(#[from] ValidationError),

    #[error("版本错误: {0}")]
    Version(#[from] VersionError),

    #[error("快照包错误: {0}")]
    Bundle(#[from] BundleError),

    #[error("数据集 `{0}` 不存在")]
    DatasetNotFound(String),

    #[error("数据集 `{dataset}` 进入 Published 必须走独立发布审批（publish_dataset），不能经通用状态迁移")]
    PublishRequiresApproval { dataset: String },

    #[error("条目 `{dataset}/{entry}` 已存在（version={version}）")]
    EntryExists { dataset: String, entry: String, version: u32 },

    #[error("条目 `{dataset}/{entry}` 已冻结（Active/Published），不可原地修改")]
    EntryFrozen { dataset: String, entry: String },

    #[error("非法状态迁移: {from:?} → {to:?}")]
    IllegalTransition { from: Option<LifecycleStatus>, to: LifecycleStatus },
}

/// 规则存储
pub struct RuleStore {
    conn: Connection,
}

impl RuleStore {
    /// 打开（或创建）数据库文件
    pub fn open(path: &str) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// 内存库（测试用）
    pub fn in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS datasets (
                dataset_id   TEXT PRIMARY KEY,
                tenant_id    TEXT NOT NULL,
                name         TEXT NOT NULL,
                description  TEXT,
                domain       TEXT NOT NULL DEFAULT '[]',   -- JSON array
                tags         TEXT NOT NULL DEFAULT '[]',   -- JSON array
                visibility   TEXT NOT NULL DEFAULT 'private',
                lifecycle    TEXT NOT NULL,                -- JSON (status + state_history)
                versioning   TEXT NOT NULL,                -- JSON
                law_ref      TEXT,                          -- JSON nullable
                version_selection TEXT,                     -- JSON nullable
                data_dependencies TEXT,                     -- JSON nullable
                meta         TEXT NOT NULL                  -- JSON
            );
            CREATE INDEX IF NOT EXISTS idx_datasets_tenant
                ON datasets(tenant_id, visibility);

            CREATE TABLE IF NOT EXISTS entries (
                dataset_id  TEXT NOT NULL,
                entry_id    TEXT NOT NULL,
                version     INTEGER NOT NULL,
                status      TEXT,                            -- nullable: 默认继承数据集
                provenance  TEXT NOT NULL,                   -- JSON
                domain      TEXT NOT NULL,
                tags        TEXT NOT NULL DEFAULT '[]',
                data_source_binding TEXT NOT NULL DEFAULT '[]',
                consumed_inputs TEXT NOT NULL DEFAULT '[]',  -- 35 号 §4：推入式输入符号
                rule_body   TEXT NOT NULL,                   -- evorule 原生 JSON（零转译）
                governance  TEXT,                            -- JSON nullable
                content_hash TEXT NOT NULL,
                PRIMARY KEY (dataset_id, entry_id, version),
                FOREIGN KEY (dataset_id) REFERENCES datasets(dataset_id)
            );
            CREATE INDEX IF NOT EXISTS idx_entries_domain ON entries(dataset_id, domain);
            CREATE INDEX IF NOT EXISTS idx_entries_hash ON entries(dataset_id, content_hash);
            "#,
        )?;
        // 轻量迁移：若旧库 entries 表缺 35 号新增的 consumed_inputs 列，则补齐
        // （CREATE TABLE IF NOT EXISTS 不会为已存在表加列，需显式 ALTER）
        let _ = self.conn.execute("ALTER TABLE entries ADD COLUMN consumed_inputs TEXT NOT NULL DEFAULT '[]'", []);
        Ok(())
    }

    // ------------------------------------------------------------------
    // 数据集 CRUD
    // ------------------------------------------------------------------

    /// 创建数据集
    pub fn create_dataset(&self, ds: &RuleDataset) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO datasets
               (dataset_id, tenant_id, name, description, domain, tags, visibility,
                lifecycle, versioning, law_ref, version_selection, data_dependencies, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                ds.dataset_id,
                ds.tenant_id,
                ds.name,
                ds.description,
                serde_json::to_string(&ds.domain)?,
                serde_json::to_string(&ds.tags)?,
                serde_json::to_string(&ds.visibility)?,
                serde_json::to_string(&ds.lifecycle)?,
                serde_json::to_string(&ds.versioning)?,
                ds.law_ref.as_ref().map(serde_json::to_string).transpose()?,
                ds.version_selection
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                ds.data_dependencies
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                serde_json::to_string(&ds.meta)?,
            ],
        )?;
        Ok(())
    }

    /// 取数据集
    pub fn get_dataset(&self, dataset_id: &str) -> Result<Option<RuleDataset>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT dataset_id, tenant_id, name, description, domain, tags, visibility,
                    lifecycle, versioning, law_ref, version_selection, data_dependencies, meta
             FROM datasets WHERE dataset_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![dataset_id], |r| {
            let lifecycle: String = r.get(7)?;
            let versioning: String = r.get(8)?;
            let meta: String = r.get(12)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                lifecycle,
                versioning,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                meta,
            ))
        })?;

        let Some(row) = rows.next() else {
            return Ok(None);
        };
        let (
            dataset_id,
            tenant_id,
            name,
            description,
            domain,
            tags,
            visibility,
            lifecycle,
            versioning,
            law_ref,
            version_selection,
            data_dependencies,
            meta,
        ) = row?;

        Ok(Some(RuleDataset {
            dataset_id,
            tenant_id,
            name,
            description,
            domain: serde_json::from_str(&domain)?,
            tags: serde_json::from_str(&tags)?,
            visibility: serde_json::from_str(&visibility)?,
            lifecycle: serde_json::from_str(&lifecycle)?,
            versioning: serde_json::from_str(&versioning)?,
            law_ref: law_ref.map(|s| serde_json::from_str(&s)).transpose()?,
            version_selection: version_selection
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
            data_dependencies: data_dependencies
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
            meta: serde_json::from_str(&meta)?,
        }))
    }

    /// 按租户列出数据集
    pub fn list_datasets(&self, tenant_id: &str) -> Result<Vec<RuleDataset>, StoreError> {
        let ids: Vec<String> = self
            .conn
            .prepare("SELECT dataset_id FROM datasets WHERE tenant_id = ?1")?
            .query_map(params![tenant_id], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        ids.iter()
            .map(|id| {
                self.get_dataset(id)?
                    .ok_or_else(|| StoreError::DatasetNotFound(id.clone()))
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // 版本与发布（决策点②：数据集版本 = 版本与发布最小单位；决策点③：两级变更线）
    // ------------------------------------------------------------------

    /// 创建数据集新版本（升版 `Major` / `Patch`）。
    ///
    /// 语义：
    /// - 新版本 = 新编辑循环：生命周期重置为 `Draft`，`state_history` 记录审计 cause
    ///   （34 号 §6：v1 可能 Published、v2 还在 Draft；MVP 单 lifecycle 字段 = 当前版本生命周期）；
    /// - 版本链只追加、版本号不可复用（`Versioning::bump` / 33 号 §8）；
    /// - 返回新版本号。
    pub fn create_dataset_version(
        &self,
        dataset_id: &str,
        kind: BumpKind,
        by: &str,
        at: &str,
    ) -> Result<String, StoreError> {
        let Some(mut ds) = self.get_dataset(dataset_id)? else {
            return Err(StoreError::DatasetNotFound(dataset_id.into()));
        };
        // 版本链完整性（防损坏数据被继续追加）
        ds.versioning.validate()?;
        // 按变更线生成新版本
        let new_versioning = ds.versioning.bump(kind)?;
        let new_version = new_versioning.current.clone();
        let prev_status = ds.lifecycle.status;
        ds.versioning = new_versioning;
        ds.lifecycle.status = LifecycleStatus::Draft;
        ds.lifecycle.state_history.push(StateChange {
            from: format!("{:?}", prev_status),
            to: format!("{:?}", LifecycleStatus::Draft),
            at: at.into(),
            by: by.into(),
            cause: format!("创建新版本 {}（{}）", new_version, bump_kind_label(kind)),
            published_as: None,
        });
        // 元数据更新时间
        ds.meta.updated_at = Some(at.into());
        ds.meta.updated_by = Some(by.into());
        self.conn.execute(
            "UPDATE datasets SET versioning=?1, lifecycle=?2, meta=?3 WHERE dataset_id=?4",
            params![
                serde_json::to_string(&ds.versioning)?,
                serde_json::to_string(&ds.lifecycle)?,
                serde_json::to_string(&ds.meta)?,
                dataset_id,
            ],
        )?;
        Ok(new_version)
    }

    // ------------------------------------------------------------------
    // 条目 CRUD
    // ------------------------------------------------------------------

    /// 新增条目：校验（数据集存在 + 符号三方一致 + LLM 边界 + 唯一性）
    pub fn add_entry(&self, entry: &RuleEntry) -> Result<(), StoreError> {
        // 1) 数据集存在
        let ds = self
            .get_dataset(&entry.dataset_id)?
            .ok_or_else(|| StoreError::DatasetNotFound(entry.dataset_id.clone()))?;
        // 2) 符号三方一致
        Validator::validate_symbol_consistency(&ds, entry)?;
        // 3) LLM 边界
        Validator::validate_llm_boundary(entry)?;
        // 4) 唯一性（entry_id + version 已由主键保证，此处显式检查以便友好报错）
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM entries WHERE dataset_id=?1 AND entry_id=?2 AND version=?3)",
            params![entry.dataset_id, entry.entry_id, entry.version],
            |r| r.get(0),
        )?;
        if exists {
            return Err(StoreError::EntryExists {
                dataset: entry.dataset_id.clone(),
                entry: entry.entry_id.clone(),
                version: entry.version,
            });
        }
        // 5) 写入
        self.conn.execute(
            "INSERT INTO entries
               (dataset_id, entry_id, version, status, provenance, domain, tags,
                data_source_binding, consumed_inputs, rule_body, governance, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                entry.dataset_id,
                entry.entry_id,
                entry.version,
                entry.status.map(|s| serde_json::to_string(&s)).transpose()?,
                serde_json::to_string(&entry.provenance)?,
                entry.domain,
                serde_json::to_string(&entry.tags)?,
                serde_json::to_string(&entry.data_source_binding)?,
                serde_json::to_string(&entry.consumed_inputs)?,
                serde_json::to_string(&entry.rule_body)?,
                entry.governance.as_ref().map(serde_json::to_string).transpose()?,
                entry.content_hash(),
            ],
        )?;
        Ok(())
    }

    /// 取条目（指定版本）
    pub fn get_entry(
        &self,
        dataset_id: &str,
        entry_id: &str,
        version: u32,
    ) -> Result<Option<RuleEntry>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_id, dataset_id, version, status, provenance, domain, tags,
                    data_source_binding, consumed_inputs, rule_body, governance
             FROM entries WHERE dataset_id=?1 AND entry_id=?2 AND version=?3",
        )?;
        let mut rows = stmt.query_map(params![dataset_id, entry_id, version], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })?;
        let Some(row) = rows.next() else {
            return Ok(None);
        };
        let (
            entry_id,
            dataset_id,
            version,
            status,
            provenance,
            domain,
            tags,
            binding,
            consumed_inputs,
            rule_body,
            governance,
        ) = row?;
        Ok(Some(RuleEntry {
            entry_id,
            dataset_id,
            version,
            status: status
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
            provenance: serde_json::from_str(&provenance)?,
            domain,
            tags: serde_json::from_str(&tags)?,
            data_source_binding: serde_json::from_str(&binding)?,
            consumed_inputs: serde_json::from_str(&consumed_inputs)?,
            rule_body: serde_json::from_str(&rule_body)?,
            governance: governance
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
        }))
    }

    /// 取条目最新版本
    pub fn get_latest_entry(
        &self,
        dataset_id: &str,
        entry_id: &str,
    ) -> Result<Option<RuleEntry>, StoreError> {
        let version: Option<u32> = self
            .conn
            .query_row(
                "SELECT MAX(version) FROM entries WHERE dataset_id=?1 AND entry_id=?2",
                params![dataset_id, entry_id],
                |r| r.get(0),
            )
            .ok();
        let Some(v) = version else {
            return Ok(None);
        };
        self.get_entry(dataset_id, entry_id, v)
    }

    /// 列出数据集下某版本的全部条目
    pub fn list_entries(
        &self,
        dataset_id: &str,
        version: Option<u32>,
    ) -> Result<Vec<RuleEntry>, StoreError> {
        let version = match version {
            Some(v) => v,
            None => {
                // 未指定 → 每个 entry_id 取最新版本
                let entry_ids: Vec<String> = self
                    .conn
                    .prepare(
                        "SELECT DISTINCT entry_id FROM entries WHERE dataset_id = ?1",
                    )?
                    .query_map(params![dataset_id], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut out = Vec::new();
                for id in entry_ids {
                    if let Some(e) = self.get_latest_entry(dataset_id, &id)? {
                        out.push(e);
                    }
                }
                return Ok(out);
            }
        };
        let entry_ids: Vec<String> = self
            .conn
            .prepare("SELECT entry_id FROM entries WHERE dataset_id=?1 AND version=?2")?
            .query_map(params![dataset_id, version], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        entry_ids
            .iter()
            .map(|id| {
                self.get_entry(dataset_id, id, version)?
                    .ok_or_else(|| StoreError::DatasetNotFound(id.clone()))
            })
            .collect()
    }

    /// 更新草稿条目内容（frozen 拒绝原地修改，§9-2 快照模式）
    pub fn update_draft_entry(&self, entry: &RuleEntry) -> Result<(), StoreError> {
        let ds = self
            .get_dataset(&entry.dataset_id)?
            .ok_or_else(|| StoreError::DatasetNotFound(entry.dataset_id.clone()))?;
        if entry.is_frozen() {
            return Err(StoreError::EntryFrozen {
                dataset: entry.dataset_id.clone(),
                entry: entry.entry_id.clone(),
            });
        }
        // 校验通过后原地更新
        Validator::validate_symbol_consistency(&ds, entry)?;
        Validator::validate_llm_boundary(entry)?;
        let n = self.conn.execute(
            "UPDATE entries SET status=?3, provenance=?4, domain=?5, tags=?6,
                    data_source_binding=?7, rule_body=?8, governance=?9, content_hash=?10
             WHERE dataset_id=?1 AND entry_id=?2 AND version=?11",
            params![
                entry.dataset_id,
                entry.entry_id,
                entry.status.map(|s| serde_json::to_string(&s)).transpose()?,
                serde_json::to_string(&entry.provenance)?,
                entry.domain,
                serde_json::to_string(&entry.tags)?,
                serde_json::to_string(&entry.data_source_binding)?,
                serde_json::to_string(&entry.rule_body)?,
                entry.governance.as_ref().map(serde_json::to_string).transpose()?,
                entry.content_hash(),
                entry.version,
            ],
        )?;
        if n == 0 {
            return Err(StoreError::DatasetNotFound(entry.entry_id.clone()));
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // 状态机
    // ------------------------------------------------------------------

    /// 状态迁移：校验合法 + 追加 state_history（数据集级）
    ///
    /// **`to=Published` 被禁止**：Published 只能经独立发布审批 `publish_dataset` 进入
    /// （34 号 §3 强约束，不静默顺带发布）。
    pub fn transition_dataset_status(
        &self,
        dataset_id: &str,
        to: LifecycleStatus,
        by: &str,
        cause: &str,
        at: &str,
    ) -> Result<(), StoreError> {
        if to == LifecycleStatus::Published {
            return Err(StoreError::PublishRequiresApproval {
                dataset: dataset_id.into(),
            });
        }
        let Some(mut ds) = self.get_dataset(dataset_id)? else {
            return Err(StoreError::DatasetNotFound(dataset_id.into()));
        };
        let from = ds.lifecycle.status;
        Validator::validate_transition(Some(from), to)
            .map_err(|(f, t)| StoreError::IllegalTransition { from: f, to: t })?;
        ds.lifecycle.status = to;
        ds.lifecycle.state_history.push(StateChange {
            from: format!("{:?}", from),
            to: format!("{:?}", to),
            at: at.into(),
            by: by.into(),
            cause: cause.into(),
            published_as: None,
        });
        self.conn.execute(
            "UPDATE datasets SET lifecycle=?1 WHERE dataset_id=?2",
            params![serde_json::to_string(&ds.lifecycle)?, dataset_id],
        )?;
        Ok(())
    }

    /// 取数据集生命周期（供状态机使用）
    pub fn dataset_lifecycle(&self, dataset_id: &str) -> Result<Option<Lifecycle>, StoreError> {
        Ok(self.get_dataset(dataset_id)?.map(|ds| ds.lifecycle))
    }

    /// **独立发布审批**（34 号 §3 强约束）：`Active → Published`。
    ///
    /// - 仅 Active 可发布；Published 只能经此显式操作进入，不由激活顺带触发；
    /// - 审计（34 号 §4）：`cause` 携带真实发布者 `instance_id`（决策点⑨ 白标不掩盖来源），
    ///   `published_as = {dataset_id}@{current_version}`（发布单位 = 数据集版本，决策点②）。
    pub fn publish_dataset(
        &self,
        dataset_id: &str,
        publisher: &str,
        at: &str,
        instance_id: &str,
    ) -> Result<(), StoreError> {
        let Some(mut ds) = self.get_dataset(dataset_id)? else {
            return Err(StoreError::DatasetNotFound(dataset_id.into()));
        };
        // 版本链完整性（published_as 依赖 current，防损坏数据被发布）
        ds.versioning.validate()?;
        // 独立发布审批前置：仅 Active
        Validator::validate_publish(Some(ds.lifecycle.status)).map_err(|(f, t)| {
            StoreError::IllegalTransition {
                from: f,
                to: t,
            }
        })?;
        let published_as = format!("{}@{}", ds.dataset_id, ds.versioning.current);
        ds.lifecycle.status = LifecycleStatus::Published;
        ds.lifecycle.state_history.push(StateChange {
            from: format!("{:?}", LifecycleStatus::Active),
            to: format!("{:?}", LifecycleStatus::Published),
            at: at.into(),
            by: publisher.into(),
            cause: format!("独立发布审批通过，instance_id={}", instance_id),
            published_as: Some(published_as),
        });
        self.conn.execute(
            "UPDATE datasets SET lifecycle=?1 WHERE dataset_id=?2",
            params![serde_json::to_string(&ds.lifecycle)?, dataset_id],
        )?;
        Ok(())
    }

    /// 对外可拉取判定（34 号 §3 双条件）：`visibility=public` AND `status=Published`。
    /// 防"内部激活即被外部拉到"；不存在返回 false。
    pub fn is_publicly_pullable(&self, dataset_id: &str) -> Result<bool, StoreError> {
        Ok(self
            .get_dataset(dataset_id)?
            .map(|ds| {
                ds.visibility == Visibility::Public
                    && ds.lifecycle.status == LifecycleStatus::Published
            })
            .unwrap_or(false))
    }

    /// 导出快照包（36 号 §2）：数据集当前版本 + 条目（最新版）→ 只读 DatasetBundle。
    ///
    /// `tests` 为沙箱验证证据（闸门一产出），由调用方如实提供；`instance_id` 为真实发布者身份
    /// （决策点⑨ 白标不掩盖）。不校验数据集状态——导出任意状态均可（消费交付由 `is_publicly_pullable` 把关）。
    pub fn export_bundle(
        &self,
        dataset_id: &str,
        tests: &BundleTests,
        by: &str,
        at: &str,
        instance_id: &str,
    ) -> Result<DatasetBundle, StoreError> {
        let ds = self.get_dataset(dataset_id)?
            .ok_or_else(|| StoreError::DatasetNotFound(dataset_id.into()))?;
        let entries = self.list_entries(dataset_id, None)?;
        Ok(BundleExporter::export(&ds, &entries, tests, by, at, instance_id))
    }
}

/// 变更线中文标签（审计 cause 用）
fn bump_kind_label(kind: BumpKind) -> &'static str {
    match kind {
        BumpKind::Major => "升版",
        BumpKind::Patch => "Patch",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DataDependencies, Provenance, ServiceDecl, SourceBinding, Visibility};

    fn tax_dataset() -> RuleDataset {
        RuleDataset {
            dataset_id: "ds-tax-2024".into(),
            name: "税务合规".into(),
            description: None,
            domain: vec!["tax".into()],
            tags: vec![],
            tenant_id: "org-evorule".into(),
            visibility: Visibility::Private,
            lifecycle: Lifecycle::default(),
            versioning: Default::default(),
            law_ref: None,
            version_selection: None,
            data_dependencies: Some(DataDependencies {
                inputs: vec![],
                services: vec![ServiceDecl {
                    service_name: "payroll_svc".into(),
                    io_contract: None,
                    sensitive: false,
                    description: None,
                    template: None,
                }],
            }),
            meta: crate::model::Meta {
                created_at: "t".into(),
                created_by: "u".into(),
                updated_at: None,
                updated_by: None,
            },
        }
    }

    fn draft_entry() -> RuleEntry {
        RuleEntry {
            entry_id: "tax-001".into(),
            dataset_id: "ds-tax-2024".into(),
            version: 1,
            status: Some(LifecycleStatus::Draft),
            provenance: Provenance {
                source: "《企业所得税法》".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "tax".into(),
            tags: vec![],
            data_source_binding: vec![SourceBinding {
                rule_ref: "rule_body.transform[0].params.service_name".into(),
                service_name: "payroll_svc".into(),
            }],
            consumed_inputs: vec![],
            rule_body: serde_json::json!({
                "transform": [{"type": "io_request", "params": {"service_name": "payroll_svc"}}]
            }),
            governance: None,
        }
    }

    #[test]
    fn test_dataset_crud() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        let got = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        assert_eq!(got.dataset_id, "ds-tax-2024");
        assert!(got.data_dependencies.unwrap().has_service("payroll_svc"));
        let list = store.list_datasets("org-evorule").unwrap();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn test_add_and_get_entry() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        store.add_entry(&draft_entry()).unwrap();
        let got = store.get_entry("ds-tax-2024", "tax-001", 1).unwrap().unwrap();
        assert_eq!(got.rule_body["transform"][0]["params"]["service_name"], "payroll_svc");
        // 最新版本
        let latest = store.get_latest_entry("ds-tax-2024", "tax-001").unwrap().unwrap();
        assert_eq!(latest.version, 1);
    }

    #[test]
    fn test_add_entry_missing_dataset() {
        let store = RuleStore::in_memory().unwrap();
        let err = store.add_entry(&draft_entry()).unwrap_err();
        assert!(matches!(err, StoreError::DatasetNotFound(_)));
    }

    #[test]
    fn test_add_entry_dup_version_rejected() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        store.add_entry(&draft_entry()).unwrap();
        let err = store.add_entry(&draft_entry()).unwrap_err();
        assert!(matches!(err, StoreError::EntryExists { .. }));
    }

    #[test]
    fn test_update_frozen_rejected() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        // 先 Draft 入库，迁移到 Active（冻结）
        store.add_entry(&draft_entry()).unwrap();
        store
            .transition_dataset_status("ds-tax-2024", LifecycleStatus::Candidate, "eng", "提交", "t")
            .unwrap();
        store
            .transition_dataset_status("ds-tax-2024", LifecycleStatus::Active, "approver", "审批", "t")
            .unwrap();
        // 条目 status 单独未改，但通过数据集级判断：这里直接构造 frozen 条目测 update
        let mut frozen = draft_entry();
        frozen.status = Some(LifecycleStatus::Active);
        let err = store.update_draft_entry(&frozen).unwrap_err();
        assert!(matches!(err, StoreError::EntryFrozen { .. }));
    }

    #[test]
    fn test_transition_illegal() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        // Draft → Published：必须走独立发布审批，通用迁移显式拒绝（34 号 §3）
        let err = store
            .transition_dataset_status("ds-tax-2024", LifecycleStatus::Published, "x", "x", "t")
            .unwrap_err();
        assert!(matches!(err, StoreError::PublishRequiresApproval { .. }));
        // 其它非法迁移仍报 IllegalTransition（如 Draft → Active）
        let err = store
            .transition_dataset_status("ds-tax-2024", LifecycleStatus::Active, "x", "x", "t")
            .unwrap_err();
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
    }

    #[test]
    fn test_publish_dataset() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        // 走完整流程到 Active（Draft → Candidate → Active）
        for to in [LifecycleStatus::Candidate, LifecycleStatus::Active] {
            store
                .transition_dataset_status("ds-tax-2024", to, "eng", "提交", "t")
                .unwrap();
        }
        // 独立发布审批
        store
            .publish_dataset("ds-tax-2024", "publisher-01", "t", "org-evorule")
            .unwrap();
        let ds = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        assert_eq!(ds.lifecycle.status, LifecycleStatus::Published);
        // 审计：from/to/真实发布者 instance_id/published_as（发布单位 = 数据集版本）
        let last = ds.lifecycle.state_history.last().unwrap();
        assert_eq!(last.from, "Active");
        assert_eq!(last.to, "Published");
        assert_eq!(last.by, "publisher-01");
        assert!(last.cause.contains("instance_id=org-evorule"));
        assert_eq!(last.published_as.as_deref(), Some("ds-tax-2024@v1"));
    }

    #[test]
    fn test_publish_requires_active() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        // Draft 直接发布 → 拒绝（独立发布审批仅 Active 可发）
        let err = store
            .publish_dataset("ds-tax-2024", "publisher-01", "t", "org")
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::IllegalTransition {
                to: LifecycleStatus::Published,
                ..
            }
        ));
        // Candidate 发布 → 拒绝
        store
            .transition_dataset_status("ds-tax-2024", LifecycleStatus::Candidate, "eng", "提交", "t")
            .unwrap();
        let err = store
            .publish_dataset("ds-tax-2024", "publisher-01", "t", "org")
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::IllegalTransition {
                to: LifecycleStatus::Published,
                ..
            }
        ));
        // 缺失数据集
        let err = store
            .publish_dataset("nope", "publisher-01", "t", "org")
            .unwrap_err();
        assert!(matches!(err, StoreError::DatasetNotFound(_)));
    }

    #[test]
    fn test_publicly_pullable_double_condition() {
        let store = RuleStore::in_memory().unwrap();
        let mut ds = tax_dataset();
        store.create_dataset(&ds).unwrap();
        // private + Draft → 不可拉
        assert!(!store.is_publicly_pullable("ds-tax-2024").unwrap());
        // 走流程到 Published，但 visibility=private → 仍不可拉（双条件缺一）
        for to in [LifecycleStatus::Candidate, LifecycleStatus::Active] {
            store
                .transition_dataset_status("ds-tax-2024", to, "eng", "提交", "t")
                .unwrap();
        }
        store
            .publish_dataset("ds-tax-2024", "publisher-01", "t", "org")
            .unwrap();
        assert!(!store.is_publicly_pullable("ds-tax-2024").unwrap());
        // visibility=public + Published → 可拉
        ds.visibility = Visibility::Public;
        store
            .conn
            .execute(
                "UPDATE datasets SET visibility=?1 WHERE dataset_id=?2",
                rusqlite::params![serde_json::to_string(&ds.visibility).unwrap(), "ds-tax-2024"],
            )
            .unwrap();
        assert!(store.is_publicly_pullable("ds-tax-2024").unwrap());
        // 不存在 → false
        assert!(!store.is_publicly_pullable("nope").unwrap());
    }

    #[test]
    fn test_retract_publish_and_revise() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        for to in [LifecycleStatus::Candidate, LifecycleStatus::Active] {
            store
                .transition_dataset_status("ds-tax-2024", to, "eng", "提交", "t")
                .unwrap();
        }
        store
            .publish_dataset("ds-tax-2024", "publisher-01", "t", "org")
            .unwrap();
        // 撤销发布（Published → Rejected）
        store
            .transition_dataset_status("ds-tax-2024", LifecycleStatus::Rejected, "admin", "撤销发布", "t")
            .unwrap();
        let ds = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        assert_eq!(ds.lifecycle.status, LifecycleStatus::Rejected);
        // 修订重来（Rejected → Draft）
        store
            .transition_dataset_status("ds-tax-2024", LifecycleStatus::Draft, "eng", "修订重来", "t")
            .unwrap();
        let ds = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        assert_eq!(ds.lifecycle.status, LifecycleStatus::Draft);
    }

    #[test]
    fn test_export_bundle_from_store() {
        use crate::bundle::{BundleImporter, BundleTests, TestVerdict};
        let store = RuleStore::in_memory().unwrap();
        let mut ds = tax_dataset();
        ds.law_ref = Some(crate::model::version::LawRef {
            document_id: "gov-tax-2023-001".into(),
            law_version: None,
            effective_from: Some("2024-01-01".into()),
            effective_to: None,
        });
        store.create_dataset(&ds).unwrap();
        store.add_entry(&draft_entry()).unwrap();
        // 导出：数据集当前版本 + 条目
        let tests = BundleTests {
            subset: vec![],
            fixtures: vec![],
            verdict: TestVerdict::Pass,
        };
        let bundle = store
            .export_bundle("ds-tax-2024", &tests, "publisher-01", "2026-08-21T12:00:00Z", "org-evorule")
            .unwrap();
        assert_eq!(bundle.audit.source_version, "v1");
        assert_eq!(bundle.entries.len(), 1);
        assert_eq!(bundle.dataset.instance_id, "org-evorule");
        // 导出包可导入校验通过（快照包消费闭环）
        let r = BundleImporter::validate(&bundle).unwrap();
        assert_eq!(r.entry_count, 1);
        assert_eq!(r.verdict, TestVerdict::Pass);
        // 缺失数据集
        let err = store
            .export_bundle("nope", &tests, "x", "t", "org")
            .unwrap_err();
        assert!(matches!(err, StoreError::DatasetNotFound(_)));
    }

    #[test]
    fn test_symbol_consistency_enforced_at_store() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        let mut bad = draft_entry();
        bad.data_source_binding[0].service_name = "undeclared_svc".into();
        let err = store.add_entry(&bad).unwrap_err();
        assert!(matches!(err, StoreError::Validation(ValidationError::ServiceNotDeclared { .. })));
    }

    #[test]
    fn test_create_dataset_version() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        // 初始 v1
        let ds = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        assert_eq!(ds.versioning.current, "v1");
        // 升版 v1 → v2
        let v = store
            .create_dataset_version("ds-tax-2024", BumpKind::Major, "eng", "t")
            .unwrap();
        assert_eq!(v, "v2");
        let ds = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        assert_eq!(ds.versioning.chain, vec!["v1", "v2"]);
        // 新版本 = 新编辑循环：生命周期重置为 Draft
        assert_eq!(ds.lifecycle.status, LifecycleStatus::Draft);
        // Patch v2 → v2.p1
        let v = store
            .create_dataset_version("ds-tax-2024", BumpKind::Patch, "eng", "t")
            .unwrap();
        assert_eq!(v, "v2.p1");
        let ds = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        assert_eq!(ds.versioning.chain, vec!["v1", "v2", "v2.p1"]);
        // 审计 cause 留痕
        let last = ds.lifecycle.state_history.last().unwrap();
        assert!(last.cause.contains("v2.p1") && last.cause.contains("Patch"));
        assert_eq!(last.to, "Draft");
        assert_eq!(last.by, "eng");
    }

    #[test]
    fn test_create_version_missing_dataset() {
        let store = RuleStore::in_memory().unwrap();
        let err = store
            .create_dataset_version("nope", BumpKind::Major, "x", "t")
            .unwrap_err();
        assert!(matches!(err, StoreError::DatasetNotFound(_)));
    }

    #[test]
    fn test_create_version_detects_corrupt_chain() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        // 人为破坏版本链（current 与链尾不一致）→ 拒绝继续追加（显式报错，不静默修复）
        let mut ds = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        ds.versioning.chain.push("v9".into());
        store
            .conn
            .execute(
                "UPDATE datasets SET versioning=?1 WHERE dataset_id=?2",
                rusqlite::params![
                    serde_json::to_string(&ds.versioning).unwrap(),
                    "ds-tax-2024"
                ],
            )
            .unwrap();
        let err = store
            .create_dataset_version("ds-tax-2024", BumpKind::Major, "x", "t")
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::Version(VersionError::ChainTailMismatch { .. })
        ));
    }
}
