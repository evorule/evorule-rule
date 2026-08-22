//! SQLite 存储层（31 号 §8）
//!
//! 存储策略：SQLite（索引/元数据）+ JSON（rule_body 原样保存，零转译）。
//! - 表：`datasets`（元数据列 + JSON 块）、`entries`（rule_body 以 JSON 文本列保存）；
//! - 索引：domain/tags（检索）、entry_id+version（版本链查询）、tenant_id+visibility（多租户，⑧）；
//! - 约束：唯一性、不可变性（frozen 拒绝原地修改）、符号三方一致（导入/提交时校验，显式报错）。

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::bundle::{BundleError, BundleExporter, BundleImporter, BundleTests, DatasetBundle, ImportResult};
use crate::model::auth::{ApiKey, AuthAudit, Role, Tenant, User};
use crate::model::dataset::{Meta, RuleDataset, Visibility};
use crate::model::dependency::{DataDependencies, ServiceTemplateRecord};
use crate::model::entry::RuleEntry;
use crate::model::governance::Governance;
use crate::model::lifecycle::{Lifecycle, LifecycleStatus, StateChange};
use crate::model::llm_audit::{LlmAuditFilter, LlmAuditStats, LlmOpAudit, OperationStat};
use crate::model::version::{BumpKind, VersionError, Versioning};
use crate::validate::{ValidationError, Validator, scan_credentials};

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

    #[error("条目 `{dataset}/{entry}` 不存在")]
    EntryNotFound { dataset: String, entry: String },

    #[error("服务模板 `{0}` 不存在")]
    TemplateNotFound(String),

    #[error("数据集 `{dataset}` 当前状态 `{status:?}` 不可删除（仅 Draft/Rejected）")]
    DatasetNotDeletable { dataset: String, status: LifecycleStatus },

    #[error("条目 `{dataset}/{entry}` 当前状态 `{status:?}` 不可删除（仅 Draft）")]
    EntryNotDeletable { dataset: String, entry: String, status: Option<LifecycleStatus> },

    #[error("版本 diff 区间非法: from=`{from}` to=`{to}`（需均存在于版本链且 from 先于 to）")]
    InvalidDiffRange { from: String, to: String },

    #[error("非法状态迁移: {from:?} → {to:?}")]
    IllegalTransition { from: Option<LifecycleStatus>, to: LifecycleStatus },

    #[error("LLM 操作审计记录 `{0}` 已存在（request_id 唯一）")]
    AuditExists(String),

    #[error("租户 `{0}` 不存在")]
    TenantNotFound(String),

    #[error("用户 `{0}` 已存在（tenant 内用户名唯一）")]
    UsernameTaken(String),

    #[error("角色 `{0}` 非法")]
    InvalidRole(String),
}

/// 规则存储
pub struct RuleStore {
    /// rusqlite `Connection` 为 Send 但非 Sync，axum 跨线程共享需 `Mutex` 包裹
    conn: std::sync::Mutex<Connection>,
}

/// service_templates 行原始列（rusqlite 闭包只读原始列，JSON 反序列化移到闭包外）
type ServiceTemplateRow = (
    String,        // template_id
    String,        // tenant_id
    String,        // service_name
    String,        // kind
    String,        // io_contract (JSON)
    String,        // endpoint_template
    Option<String>, // method
    String,        // headers_template (JSON)
    String,        // placeholder_notes (JSON)
    String,        // created_at
    String,        // created_by
);

impl RuleStore {
    /// 打开（或创建）数据库文件
    pub fn open(path: &str) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: std::sync::Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// 内存库（测试用）
    pub fn in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: std::sync::Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
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

            -- 37 号 §8：LLM 命名操作审计（"LLM 每步可审计"）
            CREATE TABLE IF NOT EXISTS llm_op_audit (
                request_id   TEXT PRIMARY KEY,
                operation    TEXT NOT NULL,                 -- draft_rule / gen_tests / explain_rule
                model        TEXT,
                status       TEXT NOT NULL,                 -- completed | failed
                duration_ms  INTEGER NOT NULL,
                result_ref   TEXT,                          -- 产出条目引用（可选）
                error        TEXT,
                created_at   TEXT NOT NULL                  -- ISO-8601 UTC
            );
            CREATE INDEX IF NOT EXISTS idx_audit_op_time
                ON llm_op_audit(operation, created_at);

            -- 43 号：认证与用户身份（正交 A，MVP 单租户实例）
            CREATE TABLE IF NOT EXISTS tenants (
                tenant_id    TEXT PRIMARY KEY,
                name         TEXT NOT NULL,
                instance_id  TEXT NOT NULL,                 -- 39 号：真实实例身份，进溯源
                created_at   TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS users (
                user_id       TEXT PRIMARY KEY,
                tenant_id     TEXT NOT NULL,
                username      TEXT NOT NULL,
                password_hash TEXT NOT NULL,                -- PBKDF2-HMAC-SHA256（MVP）
                salt          TEXT NOT NULL,
                role          TEXT NOT NULL,                -- viewer/rule_engineer/approver/admin
                disabled      INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                UNIQUE (tenant_id, username),
                FOREIGN KEY (tenant_id) REFERENCES tenants(tenant_id)
            );
            CREATE TABLE IF NOT EXISTS auth_audits (
                audit_id   TEXT PRIMARY KEY,
                action     TEXT NOT NULL,                   -- register/login/refresh/logout/disable_user
                user_id    TEXT,
                tenant_id  TEXT NOT NULL,
                outcome    TEXT NOT NULL,                   -- success | failure
                detail     TEXT,
                created_at TEXT NOT NULL                    -- ISO-8601 UTC
            );
            CREATE INDEX IF NOT EXISTS idx_auth_audit_time
                ON auth_audits(tenant_id, created_at);

            -- 43 号 §3.3：JWT 撤销黑名单（登出后按 jti 拉黑至 exp，防刷新旋转续用）
            CREATE TABLE IF NOT EXISTS revoked_tokens (
                jti        TEXT PRIMARY KEY,
                tenant_id  TEXT NOT NULL,
                user_id    TEXT,
                token_type TEXT NOT NULL,                  -- access | refresh
                expires_at INTEGER NOT NULL,               -- 过 exp 即可清理，鉴权时忽略
                revoked_at TEXT NOT NULL                   -- ISO-8601 UTC
            );
            CREATE INDEX IF NOT EXISTS idx_revoked_token_exp
                ON revoked_tokens(expires_at);

            -- 44 号 §14：API Key（MVP 最小 scope 版，仅存哈希）
            CREATE TABLE IF NOT EXISTS api_keys (
                key_id     TEXT PRIMARY KEY,
                tenant_id  TEXT NOT NULL,
                name       TEXT NOT NULL,
                scope      TEXT NOT NULL,                  -- pull（MVP）
                key_hash   TEXT NOT NULL,                  -- SHA-256(token)
                created_at TEXT NOT NULL,
                revoked_at TEXT,
                FOREIGN KEY (tenant_id) REFERENCES tenants(tenant_id)
            );
            CREATE INDEX IF NOT EXISTS idx_api_keys_tenant ON api_keys(tenant_id, revoked_at);

            -- 44 号 §5：条目级状态迁移审计（only-append，`GET /entries/{id}/history`）
            CREATE TABLE IF NOT EXISTS entry_state_history (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                dataset_id TEXT NOT NULL,
                entry_id   TEXT NOT NULL,
                version    INTEGER NOT NULL,
                from_state TEXT,
                to_state   TEXT NOT NULL,
                at         TEXT NOT NULL,
                by         TEXT NOT NULL,
                cause      TEXT NOT NULL,
                FOREIGN KEY (dataset_id, entry_id, version)
                    REFERENCES entries(dataset_id, entry_id, version)
            );
            CREATE INDEX IF NOT EXISTS idx_esh_entry
                ON entry_state_history(dataset_id, entry_id, version);

            -- 44 号 §7：无凭据服务模板注册（35 号 §5）
            CREATE TABLE IF NOT EXISTS service_templates (
                template_id       TEXT PRIMARY KEY,
                tenant_id         TEXT NOT NULL,
                service_name      TEXT NOT NULL,
                kind              TEXT NOT NULL,           -- pull | push
                io_contract       TEXT NOT NULL,           -- JSON
                endpoint_template TEXT NOT NULL,
                method            TEXT,
                headers_template  TEXT NOT NULL DEFAULT '{}',
                placeholder_notes TEXT NOT NULL DEFAULT '{}',
                created_at        TEXT NOT NULL,
                created_by        TEXT NOT NULL,
                FOREIGN KEY (tenant_id) REFERENCES tenants(tenant_id)
            );
            CREATE INDEX IF NOT EXISTS idx_templates_tenant
                ON service_templates(tenant_id, service_name);
            "#,
        )?;
        // 轻量迁移：若旧库 entries 表缺 35 号新增的 consumed_inputs 列，则补齐
        // （CREATE TABLE IF NOT EXISTS 不会为已存在表加列，需显式 ALTER）
        let _ = conn.execute("ALTER TABLE entries ADD COLUMN consumed_inputs TEXT NOT NULL DEFAULT '[]'", []);
        Ok(())
    }

    // ------------------------------------------------------------------
    // 数据集 CRUD
    // ------------------------------------------------------------------

    /// 创建数据集
    pub fn create_dataset(&self, ds: &RuleDataset) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
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
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
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

    /// 整行更新数据集（PATCH：取→改→落库；版本链/生命周期由专用方法管理，不在此改）
    pub fn update_dataset(&self, ds: &RuleDataset) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE datasets SET name=?1, description=?2, domain=?3, tags=?4, visibility=?5,
                    lifecycle=?6, versioning=?7, law_ref=?8, version_selection=?9,
                    data_dependencies=?10, meta=?11
             WHERE dataset_id=?12",
            params![
                ds.name,
                ds.description,
                serde_json::to_string(&ds.domain)?,
                serde_json::to_string(&ds.tags)?,
                serde_json::to_string(&ds.visibility)?,
                serde_json::to_string(&ds.lifecycle)?,
                serde_json::to_string(&ds.versioning)?,
                // Option 列：None 写 NULL，避免把 "null" 字符串存进 TEXT 列（读回时无法反序列化）
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
                ds.dataset_id,
            ],
        )?;
        if n == 0 {
            return Err(StoreError::DatasetNotFound(ds.dataset_id.clone()));
        }
        Ok(())
    }

    /// 删除数据集（44 号 §4：仅 Draft/Rejected 态，admin 权限由 handler 把关）
    pub fn delete_dataset(&self, dataset_id: &str) -> Result<(), StoreError> {
        let ds = self
            .get_dataset(dataset_id)?
            .ok_or_else(|| StoreError::DatasetNotFound(dataset_id.into()))?;
        let status = ds.lifecycle.status;
        if !matches!(status, LifecycleStatus::Draft | LifecycleStatus::Rejected) {
            return Err(StoreError::DatasetNotDeletable {
                dataset: dataset_id.into(),
                status,
            });
        }
        let conn = self.conn.lock().unwrap();
        // 外键依赖顺序：entry_state_history → entries → datasets
        conn.execute(
            "DELETE FROM entry_state_history WHERE dataset_id=?1",
            params![dataset_id],
        )?;
        conn.execute("DELETE FROM entries WHERE dataset_id=?1", params![dataset_id])?;
        conn.execute("DELETE FROM datasets WHERE dataset_id=?1", params![dataset_id])?;
        Ok(())
    }

    /// 数据集版本链（44 号 §4 `GET /datasets/{id}/versions`）
    pub fn list_dataset_versions(&self, dataset_id: &str) -> Result<Versioning, StoreError> {
        Ok(self
            .get_dataset(dataset_id)?
            .ok_or_else(|| StoreError::DatasetNotFound(dataset_id.into()))?
            .versioning)
    }

    /// 全部数据集（搜索/审计用；不含租户过滤，由调用方按双条件筛）
    fn list_all_datasets(&self) -> Result<Vec<RuleDataset>, StoreError> {
        let ids: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let ids = conn
                .prepare("SELECT dataset_id FROM datasets ORDER BY dataset_id")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            ids
        };
        ids.into_iter()
            .map(|id| {
                self.get_dataset(&id)?
                    .ok_or_else(|| StoreError::DatasetNotFound(id.clone()))
            })
            .collect()
    }

    /// 按租户列出数据集
    pub fn list_datasets(&self, tenant_id: &str) -> Result<Vec<RuleDataset>, StoreError> {
        let ids: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let ids = conn.prepare("SELECT dataset_id FROM datasets WHERE tenant_id = ?1")?
                .query_map(params![tenant_id], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            ids
        };
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
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
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
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
        conn.execute(
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
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
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
            .lock()
            .unwrap()
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
                let entry_ids: Vec<String> = {
                    let conn = self.conn.lock().unwrap();
                    let ids = conn.prepare(
                        "SELECT DISTINCT entry_id FROM entries WHERE dataset_id = ?1",
                    )?
                    .query_map(params![dataset_id], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                    ids
                };
                let mut out = Vec::new();
                for id in entry_ids {
                    if let Some(e) = self.get_latest_entry(dataset_id, &id)? {
                        out.push(e);
                    }
                }
                return Ok(out);
            }
        };
        let entry_ids: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let ids = conn.prepare("SELECT entry_id FROM entries WHERE dataset_id=?1 AND version=?2")?
                .query_map(params![dataset_id, version], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            ids
        };
        entry_ids
            .iter()
            .map(|id| {
                self.get_entry(dataset_id, id, version)?
                    .ok_or_else(|| StoreError::DatasetNotFound(id.clone()))
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // LLM 操作审计（37 号 §8："LLM 每步可审计"）
    // ------------------------------------------------------------------

    /// 记录一条 LLM 命名操作审计（request_id 唯一；同 id 重试 → 覆盖，幂等）。
    pub fn record_llm_audit(&self, audit: &LlmOpAudit) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO llm_op_audit
               (request_id, operation, model, status, duration_ms, result_ref, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(request_id) DO UPDATE SET
                operation=excluded.operation,
                model=excluded.model,
                status=excluded.status,
                duration_ms=excluded.duration_ms,
                result_ref=excluded.result_ref,
                error=excluded.error,
                created_at=excluded.created_at",
            params![
                audit.request_id,
                audit.operation,
                audit.model,
                audit.status,
                audit.duration_ms as i64,
                audit.result_ref,
                audit.error,
                audit.created_at,
            ],
        )?;
        Ok(())
    }

    /// 按 request_id 取一条审计记录
    pub fn get_llm_audit(&self, request_id: &str) -> Result<Option<LlmOpAudit>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT request_id, operation, model, status, duration_ms, result_ref, error, created_at
             FROM llm_op_audit WHERE request_id=?1",
        )?;
        let mut rows = stmt.query_map(params![request_id], row_to_audit)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// 列出审计记录（按时间倒序，limit 上限）——`list_llm_audits_filtered` 的便捷封装
    pub fn list_llm_audits(&self, limit: usize) -> Result<Vec<LlmOpAudit>, StoreError> {
        self.list_llm_audits_filtered(&LlmAuditFilter {
            operation: None,
            status: None,
            limit,
        })
    }

    /// 按过滤条件列出审计记录（对外展示接口，37 号 §8）
    ///
    /// 可按操作/状态过滤，按时间倒序，`limit` 上限；空过滤条件 = 全量倒序。
    pub fn list_llm_audits_filtered(
        &self,
        filter: &LlmAuditFilter,
    ) -> Result<Vec<LlmOpAudit>, StoreError> {
        let mut sql = String::from(
            "SELECT request_id, operation, model, status, duration_ms, result_ref, error, created_at
             FROM llm_op_audit WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(op) = &filter.operation {
            sql.push_str(" AND operation = ?");
            params.push(Box::new(op.clone()));
        }
        if let Some(status) = &filter.status {
            sql.push_str(" AND status = ?");
            params.push(Box::new(status.clone()));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");
        params.push(Box::new(filter.limit as i64));

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), row_to_audit)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// 审计统计摘要（对外展示接口，37 号 §8）
    ///
    /// 总条数 / 成功 / 失败 / 平均耗时，并按操作维度聚合（供报表与"LLM 每步可审计"展示）。
    pub fn llm_audit_stats(&self) -> Result<LlmAuditStats, StoreError> {
        let conn = self.conn.lock().unwrap();
        let (total, completed, failed, avg) = conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END), 0),
                    COALESCE(AVG(duration_ms), 0)
             FROM llm_op_audit",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            },
        )?;

        let mut stmt = conn.prepare(
            "SELECT operation, COUNT(*),
                    COALESCE(SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END), 0),
                    COALESCE(AVG(duration_ms), 0)
             FROM llm_op_audit GROUP BY operation",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        let mut by_operation = std::collections::BTreeMap::new();
        for row in rows {
            let (op, count, completed_op, avg_op) = row?;
            by_operation.insert(
                op,
                OperationStat {
                    count: count as u64,
                    completed: completed_op as u64,
                    failed: count as u64 - completed_op as u64,
                    avg_duration_ms: avg_op as u64,
                },
            );
        }

        Ok(LlmAuditStats {
            total: total as u64,
            completed: completed as u64,
            failed: failed as u64,
            avg_duration_ms: avg as u64,
            by_operation,
        })
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
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
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
        self.publish_dataset_with_cause(
            dataset_id,
            publisher,
            at,
            &format!("独立发布审批通过，instance_id={}", instance_id),
        )
    }

    /// 独立发布（34 号 §3/§9-1）：仅 Active 可发布，`cause` 由调用方（含二次确认回执与真实发布者）提供。
    pub fn publish_dataset_with_cause(
        &self,
        dataset_id: &str,
        publisher: &str,
        at: &str,
        cause: &str,
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
        // 发布前凭据静态扫描（35 号 §6/§9-3 强约束 MVP 手段）：数据集元数据 + 全部条目规则体。
        // 命中疑似凭据 → 拒绝发布，交由发布审批人复核（不静默放行，硬失败）。
        self.scan_dataset_credentials(dataset_id, &ds)?;
        let published_as = format!("{}@{}", ds.dataset_id, ds.versioning.current);
        ds.lifecycle.status = LifecycleStatus::Published;
        ds.lifecycle.state_history.push(StateChange {
            from: format!("{:?}", LifecycleStatus::Active),
            to: format!("{:?}", LifecycleStatus::Published),
            at: at.into(),
            by: publisher.into(),
            cause: cause.into(),
            published_as: Some(published_as),
        });
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE datasets SET lifecycle=?1 WHERE dataset_id=?2",
            params![serde_json::to_string(&ds.lifecycle)?, dataset_id],
        )?;
        Ok(())
    }

    /// 发布前凭据静态扫描（35 号 §6/§9-3）：序列化数据集元数据 + 全部条目规则体，命中疑似凭据则拒绝。
    fn scan_dataset_credentials(
        &self,
        dataset_id: &str,
        ds: &RuleDataset,
    ) -> Result<(), StoreError> {
        let mut texts = Vec::new();
        texts.push(serde_json::to_string(ds)?);
        if let Some(deps) = &ds.data_dependencies {
            texts.push(serde_json::to_string(deps)?);
        }
        // 条目规则体（执行内容核心，重点扫描）
        for entry in self.list_entries(dataset_id, None)? {
            texts.push(serde_json::to_string(&entry.rule_body)?);
            if let Some(g) = &entry.governance {
                texts.push(serde_json::to_string(g)?);
            }
        }
        let mut hits = Vec::new();
        for t in &texts {
            hits.extend(scan_credentials(t));
        }
        if !hits.is_empty() {
            return Err(StoreError::Validation(
                ValidationError::CredentialScanFailed { hits },
            ));
        }
        Ok(())
    }

    /// 撤销发布（44 号 §4 `POST /datasets/{id}/unpublish`，admin 权限由 handler 把关）：
    /// `Published → Rejected`，state_history 留痕（34 号 §2/§4：撤销发布移除对外可见，历史快照保留）。
    pub fn unpublish_dataset(&self, dataset_id: &str, by: &str, at: &str) -> Result<(), StoreError> {
        let Some(mut ds) = self.get_dataset(dataset_id)? else {
            return Err(StoreError::DatasetNotFound(dataset_id.into()));
        };
        let prev = ds.lifecycle.status;
        if prev != LifecycleStatus::Published {
            return Err(StoreError::IllegalTransition {
                from: Some(prev),
                to: LifecycleStatus::Rejected,
            });
        }
        ds.lifecycle.status = LifecycleStatus::Rejected;
        ds.lifecycle.state_history.push(StateChange {
            from: format!("{:?}", prev),
            to: format!("{:?}", LifecycleStatus::Rejected),
            at: at.into(),
            by: by.into(),
            cause: "撤销发布（Published→Rejected，独立审批记录在案）".into(),
            published_as: None,
        });
        ds.meta.updated_at = Some(at.into());
        ds.meta.updated_by = Some(by.into());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE datasets SET lifecycle=?1, meta=?2 WHERE dataset_id=?3",
            params![
                serde_json::to_string(&ds.lifecycle)?,
                serde_json::to_string(&ds.meta)?,
                dataset_id,
            ],
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

    // ------------------------------------------------------------------
    // 条目级状态机与审计（44 号 §5）
    // ------------------------------------------------------------------

    /// 删除条目（44 号 §5：仅 Draft 态，engineer 权限由 handler 把关）
    pub fn delete_entry(&self, dataset_id: &str, entry_id: &str) -> Result<(), StoreError> {
        let Some(entry) = self.get_latest_entry(dataset_id, entry_id)? else {
            return Err(StoreError::EntryNotFound {
                dataset: dataset_id.into(),
                entry: entry_id.into(),
            });
        };
        if entry.status.unwrap_or(LifecycleStatus::Active) != LifecycleStatus::Draft {
            return Err(StoreError::EntryNotDeletable {
                dataset: dataset_id.into(),
                entry: entry_id.into(),
                status: entry.status,
            });
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM entry_state_history WHERE dataset_id=?1 AND entry_id=?2",
            params![dataset_id, entry_id],
        )?;
        conn.execute(
            "DELETE FROM entries WHERE dataset_id=?1 AND entry_id=?2",
            params![dataset_id, entry_id],
        )?;
        Ok(())
    }

    /// 条目级状态迁移（44 号 §5）：Draft→Candidate（闸门一）/ Candidate→Active（闸门二）/
    /// Candidate|Draft→Rejected。只增不改 `entry_state_history`（审计即记忆）。
    pub fn transition_entry_status(
        &self,
        dataset_id: &str,
        entry_id: &str,
        to: LifecycleStatus,
        by: &str,
        at: &str,
        cause: &str,
    ) -> Result<(), StoreError> {
        let Some(mut entry) = self.get_latest_entry(dataset_id, entry_id)? else {
            return Err(StoreError::EntryNotFound {
                dataset: dataset_id.into(),
                entry: entry_id.into(),
            });
        };
        let from = entry.status.unwrap_or(LifecycleStatus::Active);
        // 专项拦截（37 号 §5 强约束）：LLM 产出（llm_generated=true）只能停留 Draft。
        // 状态机层禁止其离开 Draft（含 Draft→Candidate），不只靠人工闸门；validate_llm_boundary
        // 仅拦截"当前状态非 Draft"，此处补"迁移目标非 Draft"的离开拦截。
        if entry
            .governance
            .as_ref()
            .map(|g| g.is_llm_generated())
            .unwrap_or(false)
            && to != LifecycleStatus::Draft
        {
            return Err(StoreError::Validation(ValidationError::LlmGeneratedNotDraft {
                entry: entry_id.into(),
                status: to,
            }));
        }
        let valid = matches!(
            (from, to),
            (LifecycleStatus::Draft, LifecycleStatus::Candidate)
                | (LifecycleStatus::Candidate, LifecycleStatus::Active)
                | (LifecycleStatus::Candidate, LifecycleStatus::Rejected)
                | (LifecycleStatus::Draft, LifecycleStatus::Rejected)
        );
        if !valid {
            return Err(StoreError::IllegalTransition {
                from: Some(from),
                to,
            });
        }
        entry.status = Some(to);
        // 治理时间戳（31 号 §4 lifecycle_timestamps）
        let mut gov = entry.governance.clone().unwrap_or_default();
        let mut ts = gov.lifecycle_timestamps.clone().unwrap_or_default();
        match to {
            LifecycleStatus::Candidate => ts.candidate_at = Some(at.into()),
            LifecycleStatus::Active => ts.active_at = Some(at.into()),
            _ => {}
        }
        gov.lifecycle_timestamps = Some(ts);
        entry.governance = Some(gov);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE entries SET status=?1, governance=?2
             WHERE dataset_id=?3 AND entry_id=?4 AND version=?5",
            params![
                serde_json::to_string(&entry.status)?,
                serde_json::to_string(&entry.governance)?,
                dataset_id,
                entry_id,
                entry.version,
            ],
        )?;
        conn.execute(
            "INSERT INTO entry_state_history
                (dataset_id, entry_id, version, from_state, to_state, at, by, cause)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                dataset_id,
                entry_id,
                entry.version,
                format!("{:?}", from),
                format!("{:?}", to),
                at,
                by,
                cause,
            ],
        )?;
        Ok(())
    }

    /// 条目状态迁移历史（44 号 §5 `GET /entries/{id}/history`，only-append 只读）
    pub fn get_entry_state_history(
        &self,
        dataset_id: &str,
        entry_id: &str,
    ) -> Result<Vec<StateChange>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT from_state, to_state, at, by, cause FROM entry_state_history
             WHERE dataset_id=?1 AND entry_id=?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![dataset_id, entry_id], |r| {
            Ok(StateChange {
                from: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                to: r.get(1)?,
                at: r.get(2)?,
                by: r.get(3)?,
                cause: r.get(4)?,
                published_as: None,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// 租户内定位条目（44 号 §5 顶层 `/entries/{id}` 路由用；entry_id 仅数据集内唯一，
    /// 故在租户各数据集最新版中查找首个匹配）
    pub fn find_entry_in_tenant(
        &self,
        tenant_id: &str,
        entry_id: &str,
    ) -> Result<Option<(String, RuleEntry)>, StoreError> {
        for ds in self.list_datasets(tenant_id)? {
            if let Some(e) = self.get_latest_entry(&ds.dataset_id, entry_id)? {
                return Ok(Some((ds.dataset_id, e)));
            }
        }
        Ok(None)
    }

    // ------------------------------------------------------------------
    // 数据依赖（44 号 §7 deps/；35 号 决策点⑤）
    // ------------------------------------------------------------------

    /// 更新数据集 data_dependencies（44 号 §7 `PUT /deps/datasets/{id}`）
    pub fn update_dataset_deps(
        &self,
        dataset_id: &str,
        deps: &DataDependencies,
        by: &str,
        at: &str,
    ) -> Result<(), StoreError> {
        let Some(mut ds) = self.get_dataset(dataset_id)? else {
            return Err(StoreError::DatasetNotFound(dataset_id.into()));
        };
        ds.data_dependencies = Some(deps.clone());
        ds.meta.updated_at = Some(at.into());
        ds.meta.updated_by = Some(by.into());
        self.update_dataset(&ds)
    }

    /// 注册无凭据服务模板（44 号 §7 `POST /deps/templates`，admin 权限由 handler 把关）
    pub fn create_service_template(&self, t: &ServiceTemplateRecord) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO service_templates
                (template_id, tenant_id, service_name, kind, io_contract, endpoint_template,
                 method, headers_template, placeholder_notes, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                t.template_id,
                t.tenant_id,
                t.service_name,
                t.kind,
                serde_json::to_string(&t.io_contract)?,
                t.endpoint_template,
                t.method,
                serde_json::to_string(&t.headers_template)?,
                serde_json::to_string(&t.placeholder_notes)?,
                t.created_at,
                t.created_by,
            ],
        )?;
        Ok(())
    }

    /// 模板详情（44 号 §7 `GET /deps/templates/{id}`）
    pub fn get_service_template(
        &self,
        template_id: &str,
    ) -> Result<Option<ServiceTemplateRecord>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT template_id, tenant_id, service_name, kind, io_contract, endpoint_template,
                    method, headers_template, placeholder_notes, created_at, created_by
             FROM service_templates WHERE template_id = ?1",
        )?;
        // 闭包只读原始列（rusqlite 错误域），JSON 反序列化移到闭包外（StoreError::Json）
        let row = stmt
            .query_row(params![template_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, String>(10)?,
                ))
            })
            .optional()?;
        let Some((
            template_id,
            tenant_id,
            service_name,
            kind,
            io_contract,
            endpoint_template,
            method,
            headers_template,
            placeholder_notes,
            created_at,
            created_by,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(ServiceTemplateRecord {
            template_id,
            tenant_id,
            service_name,
            kind,
            io_contract: serde_json::from_str(&io_contract)?,
            endpoint_template,
            method,
            headers_template: serde_json::from_str(&headers_template)?,
            placeholder_notes: serde_json::from_str(&placeholder_notes)?,
            created_at,
            created_by,
        }))
    }

    /// 模板列表（44 号 §7 `GET /deps/templates`，租户作用域）
    pub fn list_service_templates(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<ServiceTemplateRecord>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT template_id, tenant_id, service_name, kind, io_contract, endpoint_template,
                    method, headers_template, placeholder_notes, created_at, created_by
             FROM service_templates WHERE tenant_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![tenant_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, String>(10)?,
            ))
        })?;
        let raw: Vec<ServiceTemplateRow> = rows.collect::<Result<_, _>>()?;
        raw.into_iter()
            .map(
                |(template_id, tenant_id, service_name, kind, io_contract, endpoint_template, method, headers_template, placeholder_notes, created_at, created_by)| {
                    Ok(ServiceTemplateRecord {
                        template_id,
                        tenant_id,
                        service_name,
                        kind,
                        io_contract: serde_json::from_str(&io_contract)?,
                        endpoint_template,
                        method,
                        headers_template: serde_json::from_str(&headers_template)?,
                        placeholder_notes: serde_json::from_str(&placeholder_notes)?,
                        created_at,
                        created_by,
                    })
                },
            )
            .collect()
    }

    // ------------------------------------------------------------------
    // 检索（44 号 §9 search/）
    // ------------------------------------------------------------------

    /// 数据集检索（44 号 §9）：private 仅当前租户；public+Published 对所有人可见（双条件，38 号 §3）。
    pub fn search_datasets(
        &self,
        tenant_id: &str,
        domain: Option<&str>,
        q: Option<&str>,
        tags: &[String],
        effective_from_after: Option<&str>,
        visibility: Option<Visibility>,
    ) -> Result<Vec<RuleDataset>, StoreError> {
        let mut out: Vec<RuleDataset> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // 1) 本租户全部可见
        for ds in self.list_datasets(tenant_id)? {
            if seen.insert(ds.dataset_id.clone()) {
                out.push(ds);
            }
        }
        // 2) public+Published 跨租户可检索（34 号 §3 双条件）
        for ds in self.list_all_datasets()? {
            if ds.visibility == Visibility::Public
                && ds.lifecycle.status == LifecycleStatus::Published
                && seen.insert(ds.dataset_id.clone())
            {
                out.push(ds);
            }
        }
        let lower_q = q.map(|s| s.to_lowercase());
        out.retain(|ds| {
            if let Some(d) = domain {
                if !ds.domain.iter().any(|x| x.eq_ignore_ascii_case(d)) {
                    return false;
                }
            }
            if let Some(lq) = &lower_q {
                let hay = format!(
                    "{} {} {}",
                    ds.name,
                    ds.description.as_deref().unwrap_or(""),
                    ds.dataset_id
                )
                .to_lowercase();
                if !hay.contains(lq) {
                    return false;
                }
            }
            if !tags.is_empty() && !tags.iter().any(|t| ds.tags.contains(t)) {
                return false;
            }
            if let Some(after) = effective_from_after {
                let eff = ds
                    .law_ref
                    .as_ref()
                    .and_then(|l| l.effective_from.as_deref())
                    .unwrap_or("");
                if eff < after {
                    return false;
                }
            }
            if let Some(v) = visibility {
                if ds.visibility != v {
                    return false;
                }
            }
            true
        });
        Ok(out)
    }

    /// 条目检索（44 号 §9）：租户作用域（数据端点一律以 tenant 为界，38 号）
    pub fn search_entries(
        &self,
        tenant_id: &str,
        dataset_id: Option<&str>,
        domain: Option<&str>,
        q: Option<&str>,
        tags: &[String],
        status: Option<LifecycleStatus>,
    ) -> Result<Vec<RuleEntry>, StoreError> {
        let mut out = Vec::new();
        let lower_q = q.map(|s| s.to_lowercase());
        for ds in self.list_datasets(tenant_id)? {
            if let Some(did) = dataset_id {
                if ds.dataset_id != did {
                    continue;
                }
            }
            for e in self.list_entries(&ds.dataset_id, None)? {
                if let Some(d) = domain {
                    if !e.domain.eq_ignore_ascii_case(d) {
                        continue;
                    }
                }
                if let Some(st) = status {
                    if e.status != Some(st) {
                        continue;
                    }
                }
                if let Some(lq) = &lower_q {
                    let hay = format!(
                        "{} {} {} {}",
                        e.entry_id,
                        e.provenance.source,
                        e.provenance.clause.as_deref().unwrap_or(""),
                        e.rule_body
                    )
                    .to_lowercase();
                    if !hay.contains(lq) {
                        continue;
                    }
                }
                if !tags.is_empty() && !tags.iter().any(|t| e.tags.contains(t)) {
                    continue;
                }
                out.push(e);
            }
        }
        Ok(out)
    }

    /// 版本 diff（44 号 §9 `GET /search/datasets/{id}/diff`，33 号内容哈希语义）。
    ///
    /// MVP 只存当前版本条目，无法重建历史版本内容 → 返回**结构级 diff**（版本链增量 + 当前条目清单），
    /// 内容级 diff 待历史快照落库后补（批次 1）。诚实标注，不伪造。
    pub fn version_diff(
        &self,
        dataset_id: &str,
        from: &str,
        to: &str,
    ) -> Result<serde_json::Value, StoreError> {
        let ds = self
            .get_dataset(dataset_id)?
            .ok_or_else(|| StoreError::DatasetNotFound(dataset_id.into()))?;
        let chain = &ds.versioning.chain;
        let fi = chain.iter().position(|v| v == from).ok_or_else(|| {
            StoreError::InvalidDiffRange {
                from: from.into(),
                to: to.into(),
            }
        })?;
        let ti = chain.iter().position(|v| v == to).ok_or_else(|| {
            StoreError::InvalidDiffRange {
                from: from.into(),
                to: to.into(),
            }
        })?;
        if ti <= fi {
            return Err(StoreError::InvalidDiffRange {
                from: from.into(),
                to: to.into(),
            });
        }
        let entries = self.list_entries(dataset_id, None)?;
        let entry_ids: Vec<String> = entries.iter().map(|e| e.entry_id.clone()).collect();
        Ok(serde_json::json!({
            "dataset_id": dataset_id,
            "from": from,
            "to": to,
            "added_versions": &chain[fi + 1..=ti],
            "current_entry_count": entries.len(),
            "current_entry_ids": entry_ids,
            "note": "MVP 结构级 diff：仅版本链增量 + 当前条目清单；内容级 diff 待历史快照落库（批次 1）",
        }))
    }

    /// 生命周期审计（44 号 §11 `GET /audits/lifecycle`）：租户内数据集 state_history 扁平输出
    pub fn list_lifecycle_audits(&self, tenant_id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        let mut out = Vec::new();
        for ds in self.list_datasets(tenant_id)? {
            for sc in &ds.lifecycle.state_history {
                out.push(serde_json::json!({
                    "dataset_id": ds.dataset_id,
                    "from": sc.from,
                    "to": sc.to,
                    "at": sc.at,
                    "by": sc.by,
                    "cause": sc.cause,
                    "published_as": sc.published_as,
                }));
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // 快照包导入（44 号 §6 bundles/import；36 号 5 步校验链）
    // ------------------------------------------------------------------

    /// 导入快照包：`BundleImporter::validate`（schema→防篡改→符号三方一致→版本解析→闸门一）通过后落库。
    ///
    /// - 数据集：不存在则新建（tenant=导入方），已存在则覆盖其版本链/依赖/锚；
    /// - 条目：BundleEntry → RuleEntry（治理版本=1，状态 Active），先清空旧条目再写入（可重试幂等）；
    /// - 校验链任一失败 → 显式错误（35 号 §9 硬失败，不静默降级）。
    pub fn import_bundle(
        &self,
        bundle: &DatasetBundle,
        tenant_id: &str,
        by: &str,
        at: &str,
        instance_id: &str,
    ) -> Result<ImportResult, StoreError> {
        let result = BundleImporter::validate(bundle)?;
        let did = &bundle.dataset.dataset_id;
        let existing = self.get_dataset(did)?;
        let mut ds = match existing {
            Some(mut e) => {
                // 覆盖导入：更新版本链/锚/依赖/可见性，记录导入 cause
                e.name = bundle.dataset.name.clone();
                e.versioning = bundle.dataset.versioning.clone();
                e.law_ref = bundle.dataset.law_ref.clone();
                e.version_selection = bundle.dataset.version_selection.clone();
                e.data_dependencies = bundle.data_dependencies.clone();
                e.lifecycle.status = LifecycleStatus::Active;
                e.lifecycle.state_history.push(StateChange {
                    from: format!("{:?}", e.lifecycle.status),
                    to: format!("{:?}", LifecycleStatus::Active),
                    at: at.into(),
                    by: by.into(),
                    cause: format!("导入快照包 {}（instance_id={}）", bundle.bundle_id, instance_id),
                    published_as: None,
                });
                e.meta.updated_at = Some(at.into());
                e.meta.updated_by = Some(by.into());
                // 清空旧条目（导入可重试幂等）
                self.delete_dataset_entries(did)?;
                e
            }
            None => RuleDataset {
                dataset_id: did.clone(),
                name: bundle.dataset.name.clone(),
                description: Some(format!("由快照包 {} 导入", bundle.bundle_id)),
                domain: bundle
                    .entries
                    .iter()
                    .map(|e| e.domain.clone())
                    .collect(),
                tags: vec![],
                tenant_id: tenant_id.into(),
                visibility: Visibility::Private,
                lifecycle: Lifecycle {
                    status: LifecycleStatus::Active,
                    state_history: vec![StateChange {
                        from: format!("{:?}", LifecycleStatus::Draft),
                        to: format!("{:?}", LifecycleStatus::Active),
                        at: at.into(),
                        by: by.into(),
                        cause: format!("导入快照包 {}（instance_id={}）", bundle.bundle_id, instance_id),
                        published_as: None,
                    }],
                },
                versioning: bundle.dataset.versioning.clone(),
                law_ref: bundle.dataset.law_ref.clone(),
                version_selection: bundle.dataset.version_selection.clone(),
                data_dependencies: bundle.data_dependencies.clone(),
                meta: Meta {
                    created_at: at.into(),
                    created_by: by.into(),
                    updated_at: None,
                    updated_by: None,
                },
            },
        };
        self.update_dataset_or_create(&mut ds)?;
        // 条目落库
        for be in &bundle.entries {
            let entry = RuleEntry {
                entry_id: be.entry_id.clone(),
                dataset_id: did.clone(),
                version: 1,
                status: Some(LifecycleStatus::Active),
                provenance: be.provenance.clone(),
                domain: be.domain.clone(),
                tags: be.tags.clone(),
                data_source_binding: be.dependencies.clone(),
                consumed_inputs: vec![],
                rule_body: be.rule_body.clone(),
                governance: Some(Governance {
                    author: Some(by.into()),
                    updater: None,
                    llm_generated: None,
                    lifecycle_timestamps: None,
                }),
            };
            self.add_entry(&entry)?;
        }
        Ok(result)
    }

    /// 导入辅助：数据集存在则整行更新，否则新建
    fn update_dataset_or_create(&self, ds: &mut RuleDataset) -> Result<(), StoreError> {
        match self.get_dataset(&ds.dataset_id)? {
            Some(_) => self.update_dataset(ds),
            None => self.create_dataset(ds),
        }
    }

    /// 删除数据集全部条目（导入覆盖用；不动数据集本身）
    fn delete_dataset_entries(&self, dataset_id: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM entry_state_history WHERE dataset_id=?1",
            params![dataset_id],
        )?;
        conn.execute("DELETE FROM entries WHERE dataset_id=?1", params![dataset_id])?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 认证与用户（43 号 正交 A，MVP 单租户实例）
    // ------------------------------------------------------------------

    /// 确保实例默认租户存在（MVP 单租户：不存在则创建，存在则返回）
    pub fn ensure_default_tenant(
        &self,
        tenant_id: &str,
        name: &str,
        instance_id: &str,
        created_at: &str,
    ) -> Result<Tenant, StoreError> {
        self.conn.lock().unwrap().execute(
            "INSERT OR IGNORE INTO tenants (tenant_id, name, instance_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![tenant_id, name, instance_id, created_at],
        )?;
        let tenant = self
            .get_tenant(tenant_id)?
            .ok_or_else(|| StoreError::TenantNotFound(tenant_id.into()))?;
        Ok(tenant)
    }

    pub fn get_tenant(&self, tenant_id: &str) -> Result<Option<Tenant>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT tenant_id, name, instance_id, created_at FROM tenants WHERE tenant_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![tenant_id], row_to_tenant)?;
        match rows.next() {
            Some(Ok(t)) => Ok(Some(t)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn create_user(&self, user: &User) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO users
               (user_id, tenant_id, username, password_hash, salt, role, disabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                user.user_id,
                user.tenant_id,
                user.username,
                user.password_hash,
                user.salt,
                user.role.as_str(),
                user.disabled as i64,
                user.created_at,
                user.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_user_by_username(
        &self,
        tenant_id: &str,
        username: &str,
    ) -> Result<Option<User>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_id, tenant_id, username, password_hash, salt, role, disabled, created_at, updated_at
             FROM users WHERE tenant_id = ?1 AND username = ?2",
        )?;
        let mut rows = stmt.query_map(params![tenant_id, username], row_to_user)?;
        match rows.next() {
            Some(Ok(u)) => Ok(Some(u)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_user(&self, user_id: &str) -> Result<Option<User>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_id, tenant_id, username, password_hash, salt, role, disabled, created_at, updated_at
             FROM users WHERE user_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![user_id], row_to_user)?;
        match rows.next() {
            Some(Ok(u)) => Ok(Some(u)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 禁用/启用用户（管理员操作；禁用的用户登录/刷新被拒）
    pub fn set_user_disabled(&self, user_id: &str, disabled: bool, at: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET disabled = ?1, updated_at = ?2 WHERE user_id = ?3",
            params![disabled as i64, at, user_id],
        )?;
        Ok(())
    }

    /// 认证审计（only-append，43 号 §6）
    pub fn record_auth_audit(&self, audit: &AuthAudit) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO auth_audits (audit_id, action, user_id, tenant_id, outcome, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                audit.audit_id,
                audit.action,
                audit.user_id,
                audit.tenant_id,
                audit.outcome,
                audit.detail,
                audit.created_at,
            ],
        )?;
        Ok(())
    }

    /// 列出认证审计（倒序 + limit）
    pub fn list_auth_audits(&self, tenant_id: &str, limit: usize) -> Result<Vec<AuthAudit>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT audit_id, action, user_id, tenant_id, outcome, detail, created_at
             FROM auth_audits WHERE tenant_id = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![tenant_id, limit as i64], row_to_auth_audit)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// 按 jti 拉黑 token（43 号 §3.3；登出后至 exp 拒用，`expires_at` 用于到期清理）
    pub fn revoke_token(
        &self,
        jti: &str,
        tenant_id: &str,
        user_id: Option<&str>,
        token_type: &str,
        expires_at: i64,
        revoked_at: &str,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO revoked_tokens (jti, tenant_id, user_id, token_type, expires_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(jti) DO NOTHING",
            params![jti, tenant_id, user_id, token_type, expires_at, revoked_at],
        )?;
        Ok(())
    }

    /// 是否已被拉黑（已过 exp 的记录视为不再有效，返回 false）
    pub fn is_token_revoked(&self, jti: &str, now: i64) -> Result<bool, StoreError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM revoked_tokens WHERE jti = ?1 AND expires_at > ?2",
            params![jti, now],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    // ------------------------------------------------------------------
    // API Key（44 号 §14，MVP 最小 scope 版）
    // ------------------------------------------------------------------

    pub fn create_api_key(&self, key: &ApiKey) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO api_keys (key_id, tenant_id, name, scope, key_hash, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                key.key_id,
                key.tenant_id,
                key.name,
                key.scope,
                key.key_hash,
                key.created_at,
                key.revoked_at,
            ],
        )?;
        Ok(())
    }

    /// 按 key 哈希查（登录/鉴权用；revoked 也返回，由调用方判定）
    pub fn get_api_key_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT key_id, tenant_id, name, scope, key_hash, created_at, revoked_at
             FROM api_keys WHERE key_hash = ?1",
        )?;
        let mut rows = stmt.query_map(params![key_hash], row_to_api_key)?;
        match rows.next() {
            Some(Ok(k)) => Ok(Some(k)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_api_keys(&self, tenant_id: &str) -> Result<Vec<ApiKey>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT key_id, tenant_id, name, scope, key_hash, created_at, revoked_at
             FROM api_keys WHERE tenant_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![tenant_id], row_to_api_key)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn revoke_api_key(&self, tenant_id: &str, key_id: &str, at: &str) -> Result<bool, StoreError> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE api_keys SET revoked_at = ?3
             WHERE tenant_id = ?1 AND key_id = ?2 AND revoked_at IS NULL",
            params![tenant_id, key_id, at],
        )?;
        Ok(n > 0)
    }
}

/// 变更线中文标签（审计 cause 用）
fn bump_kind_label(kind: BumpKind) -> &'static str {
    match kind {
        BumpKind::Major => "升版",
        BumpKind::Patch => "Patch",
    }
}

/// `llm_op_audit` 行 → `LlmOpAudit`
fn row_to_audit(r: &rusqlite::Row) -> rusqlite::Result<LlmOpAudit> {
    Ok(LlmOpAudit {
        request_id: r.get(0)?,
        operation: r.get(1)?,
        model: r.get(2)?,
        status: r.get(3)?,
        duration_ms: r.get::<_, i64>(4)? as u64,
        result_ref: r.get(5)?,
        error: r.get(6)?,
        created_at: r.get(7)?,
    })
}

/// `tenants` 行 → `Tenant`
fn row_to_tenant(r: &rusqlite::Row) -> rusqlite::Result<Tenant> {
    Ok(Tenant {
        tenant_id: r.get(0)?,
        name: r.get(1)?,
        instance_id: r.get(2)?,
        created_at: r.get(3)?,
    })
}

/// `users` 行 → `User`
fn row_to_user(r: &rusqlite::Row) -> rusqlite::Result<User> {
    let role_str: String = r.get(5)?;
    let role = Role::parse(&role_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(StoreError::InvalidRole(role_str.clone())),
        )
    })?;
    Ok(User {
        user_id: r.get(0)?,
        tenant_id: r.get(1)?,
        username: r.get(2)?,
        password_hash: r.get(3)?,
        salt: r.get(4)?,
        role,
        disabled: r.get::<_, i64>(6)? != 0,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// `auth_audits` 行 → `AuthAudit`
fn row_to_auth_audit(r: &rusqlite::Row) -> rusqlite::Result<AuthAudit> {
    Ok(AuthAudit {
        audit_id: r.get(0)?,
        action: r.get(1)?,
        user_id: r.get(2)?,
        tenant_id: r.get(3)?,
        outcome: r.get(4)?,
        detail: r.get(5)?,
        created_at: r.get(6)?,
    })
}

/// `api_keys` 行 → `ApiKey`
fn row_to_api_key(r: &rusqlite::Row) -> rusqlite::Result<ApiKey> {
    Ok(ApiKey {
        key_id: r.get(0)?,
        tenant_id: r.get(1)?,
        name: r.get(2)?,
        scope: r.get(3)?,
        key_hash: r.get(4)?,
        created_at: r.get(5)?,
        revoked_at: r.get(6)?,
    })
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
            .lock()
            .unwrap()
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
    fn test_publish_rejects_credential_scan() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        // 往规则体里塞疑似凭据（35 号 §6/§9-3：发布前扫描拦截）
        let mut entry = draft_entry();
        entry.rule_body = serde_json::json!({
            "transform": [{ "type": "io_request", "params": { "service_name": "payroll_svc" } }],
            "note": "内嵌了一个不该存在的密钥",
            "env": { "api_key": "SK-LIVE-abc12345" }
        });
        store.add_entry(&entry).unwrap();
        for to in [LifecycleStatus::Candidate, LifecycleStatus::Active] {
            store
                .transition_dataset_status("ds-tax-2024", to, "eng", "提交", "t")
                .unwrap();
        }
        let err = store
            .publish_dataset("ds-tax-2024", "publisher-01", "t", "org")
            .unwrap_err();
        assert!(matches!(err, StoreError::Validation(ValidationError::CredentialScanFailed { .. })), "{err}");
        // 未发布（仍 Active）
        let ds = store.get_dataset("ds-tax-2024").unwrap().unwrap();
        assert_eq!(ds.lifecycle.status, LifecycleStatus::Active);
    }

    #[test]
    fn test_llm_generated_entry_cannot_leave_draft() {
        use crate::model::governance::{Governance, LlmGenerated};
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        let mut entry = draft_entry();
        // 标记为 LLM 产出（37 号 §5：只到 Draft）
        entry.governance = Some(Governance {
            llm_generated: Some(LlmGenerated {
                flag: true,
                model: Some("deepseek-v4".into()),
                op: Some("draft_rule".into()),
                timestamp: Some("2026-07-01T08:10:00Z".into()),
            }),
            ..Governance::default()
        });
        store.add_entry(&entry).unwrap();
        // 状态机层拦截：LLM 产出 Draft → Candidate 非法
        let err = store
            .transition_entry_status("ds-tax-2024", &entry.entry_id, LifecycleStatus::Candidate, "eng", "t", "提交")
            .unwrap_err();
        assert!(matches!(err, StoreError::Validation(ValidationError::LlmGeneratedNotDraft { .. })), "{err}");
        // 仍为 Draft
        let e = store.get_latest_entry("ds-tax-2024", &entry.entry_id).unwrap().unwrap();
        assert_eq!(e.status, Some(LifecycleStatus::Draft));
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
            .lock()
            .unwrap()
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

    #[test]
    fn test_llm_audit_roundtrip() {
        let store = RuleStore::in_memory().unwrap();
        let a = LlmOpAudit {
            request_id: "req-0001".into(),
            operation: "draft_rule".into(),
            model: Some("deepseek-v4".into()),
            status: "completed".into(),
            duration_ms: 1234,
            result_ref: Some("ds-tax-2024/tax-001-rule-01".into()),
            error: None,
            created_at: "2026-08-22T10:00:00Z".into(),
        };
        store.record_llm_audit(&a).unwrap();
        let got = store.get_llm_audit("req-0001").unwrap().unwrap();
        assert_eq!(got, a);
        assert_eq!(store.get_llm_audit("nope").unwrap(), None);
    }

    #[test]
    fn test_llm_audit_upsert_idempotent() {
        let store = RuleStore::in_memory().unwrap();
        let a = LlmOpAudit {
            request_id: "req-1".into(),
            operation: "gen_tests".into(),
            model: None,
            status: "failed".into(),
            duration_ms: 100,
            result_ref: None,
            error: Some("LLM 调用失败".into()),
            created_at: "2026-08-22T10:00:00Z".into(),
        };
        store.record_llm_audit(&a).unwrap();
        // 同 request_id 重试 → 覆盖更新（幂等），不产生第二行
        let mut retry = a.clone();
        retry.status = "completed".into();
        retry.error = None;
        retry.duration_ms = 200;
        store.record_llm_audit(&retry).unwrap();
        let got = store.get_llm_audit("req-1").unwrap().unwrap();
        assert_eq!(got.status, "completed");
        assert_eq!(got.duration_ms, 200);
        assert_eq!(got.error, None);
        assert_eq!(store.list_llm_audits(100).unwrap().len(), 1);
    }

    #[test]
    fn test_llm_audit_list_order_and_limit() {
        let store = RuleStore::in_memory().unwrap();
        for i in 0..5 {
            store
                .record_llm_audit(&LlmOpAudit {
                    request_id: format!("req-{}", i),
                    operation: "explain_rule".into(),
                    model: None,
                    status: "completed".into(),
                    duration_ms: i,
                    result_ref: None,
                    error: None,
                    // 时间戳递增，确保倒序可断言
                    created_at: format!("2026-08-22T10:00:0{}Z", i),
                })
                .unwrap();
        }
        let all = store.list_llm_audits(100).unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].request_id, "req-4"); // 倒序：最新在前
        assert_eq!(all[4].request_id, "req-0");
        let limited = store.list_llm_audits(2).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].request_id, "req-4");
    }

    #[test]
    fn test_llm_audit_filter_by_operation_and_status() {
        let store = RuleStore::in_memory().unwrap();
        for i in 0..3 {
            store
                .record_llm_audit(&LlmOpAudit {
                    request_id: format!("d-{}", i),
                    operation: "draft_rule".into(),
                    model: None,
                    status: "completed".into(),
                    duration_ms: i * 10,
                    result_ref: None,
                    error: None,
                    created_at: format!("2026-08-22T10:00:0{}Z", i),
                })
                .unwrap();
        }
        store
            .record_llm_audit(&LlmOpAudit {
                request_id: "g-1".into(),
                operation: "gen_tests".into(),
                model: None,
                status: "failed".into(),
                duration_ms: 5,
                result_ref: None,
                error: Some("boom".into()),
                created_at: "2026-08-22T10:00:09Z".into(),
            })
            .unwrap();

        // 按 operation 过滤
        let drafts = store
            .list_llm_audits_filtered(&LlmAuditFilter {
                operation: Some("draft_rule".into()),
                status: None,
                limit: 100,
            })
            .unwrap();
        assert_eq!(drafts.len(), 3);
        assert!(drafts.iter().all(|a| a.operation == "draft_rule"));

        // 按 status 过滤
        let failed = store
            .list_llm_audits_filtered(&LlmAuditFilter {
                operation: None,
                status: Some("failed".into()),
                limit: 100,
            })
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].request_id, "g-1");

        // 组合过滤（无匹配）
        let none = store
            .list_llm_audits_filtered(&LlmAuditFilter {
                operation: Some("gen_tests".into()),
                status: Some("completed".into()),
                limit: 100,
            })
            .unwrap();
        assert!(none.is_empty());

        // limit 生效
        let one = store
            .list_llm_audits_filtered(&LlmAuditFilter {
                operation: Some("draft_rule".into()),
                status: None,
                limit: 1,
            })
            .unwrap();
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn test_llm_audit_stats_aggregation() {
        let store = RuleStore::in_memory().unwrap();
        // draft_rule: 3 成功（耗时 10/20/30）+ 1 失败（50）
        for (id, ms) in [("d1", 10), ("d2", 20), ("d3", 30)] {
            store
                .record_llm_audit(&LlmOpAudit {
                    request_id: id.into(),
                    operation: "draft_rule".into(),
                    model: None,
                    status: "completed".into(),
                    duration_ms: ms,
                    result_ref: None,
                    error: None,
                    created_at: "2026-08-22T10:00:00Z".into(),
                })
                .unwrap();
        }
        store
            .record_llm_audit(&LlmOpAudit {
                request_id: "d4".into(),
                operation: "draft_rule".into(),
                model: None,
                status: "failed".into(),
                duration_ms: 50,
                result_ref: None,
                error: Some("boom".into()),
                created_at: "2026-08-22T10:00:00Z".into(),
            })
            .unwrap();
        // gen_tests: 1 成功（耗时 100）
        store
            .record_llm_audit(&LlmOpAudit {
                request_id: "g1".into(),
                operation: "gen_tests".into(),
                model: None,
                status: "completed".into(),
                duration_ms: 100,
                result_ref: None,
                error: None,
                created_at: "2026-08-22T10:00:00Z".into(),
            })
            .unwrap();

        let stats = store.llm_audit_stats().unwrap();
        assert_eq!(stats.total, 5);
        assert_eq!(stats.completed, 4);
        assert_eq!(stats.failed, 1);
        // 平均耗时 = (10+20+30+50+100)/5 = 210/5 = 42
        assert_eq!(stats.avg_duration_ms, 42);

        let draft = stats.by_operation.get("draft_rule").unwrap();
        assert_eq!(draft.count, 4);
        assert_eq!(draft.completed, 3);
        assert_eq!(draft.failed, 1);
        // draft 平均 = (10+20+30+50)/4 = 110/4 = 27.5 → 27
        assert_eq!(draft.avg_duration_ms, 27);
        let gen = stats.by_operation.get("gen_tests").unwrap();
        assert_eq!(gen.count, 1);
        assert_eq!(gen.completed, 1);
        assert_eq!(gen.failed, 0);
        assert_eq!(gen.avg_duration_ms, 100);
    }

    #[test]
    fn test_llm_audit_stats_empty() {
        let store = RuleStore::in_memory().unwrap();
        let stats = store.llm_audit_stats().unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.completed, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.avg_duration_ms, 0);
        assert!(stats.by_operation.is_empty());
    }
}
