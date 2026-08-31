//! SQLite 存储层（31 号 §8）
//!
//! 存储策略：SQLite（索引/元数据）+ JSON（rule_body 原样保存，零转译）。
//! - 表：`datasets`（元数据列 + JSON 块）、`entries`（rule_body 以 JSON 文本列保存）；
//! - 索引：domain/tags（检索）、entry_id+version（版本链查询）、tenant_id+visibility（多租户，⑧）；
//! - 约束：唯一性、不可变性（frozen 拒绝原地修改）、符号三方一致（导入/提交时校验，显式报错）。

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

/// PostgreSQL 生产后端（45 号批次1 §2）：仅 `--features postgres` 编译。
/// 默认 SQLite（`RuleStore`）仍是 MVP 活跃引擎，本模块为生产级后续接线位（骨架）。
#[cfg(feature = "postgres")]
pub mod pg;

use crate::bundle::{
    BundleEntry, BundleError, BundleExporter, BundleImporter, BundleTests, DatasetBundle,
    EntryKind, ImportResult,
};
use crate::model::auth::{ApiKey, AuthAudit, Org, Role, Tenant, User, UserOrg};
use crate::model::dataset::{DatasetKind, Meta, RuleDataset, Visibility};
use crate::model::dependency::{DataDependencies, ServiceTemplateRecord};
use crate::model::entry::RuleEntry;
use crate::model::governance::Governance;
use crate::model::knowledge::KnowledgeEntry;
use crate::model::lifecycle::{Lifecycle, LifecycleStatus, StateChange};
use crate::model::llm_audit::{LlmAuditFilter, LlmAuditStats, LlmOpAudit, OperationStat};
use crate::model::service_catalog::{
    OFFICIAL_NATIVE_SERVICES, ServiceCatalogEntry, official_entry,
};
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

    #[error("条目版本不存在: dataset=`{dataset}` entry=`{entry}` version=`{version}`")]
    EntryVersionNotFound { dataset: String, entry: String, version: u32 },

    #[error("非法状态迁移: {from:?} → {to:?}")]
    IllegalTransition { from: Option<LifecycleStatus>, to: LifecycleStatus },

    #[error("LLM 操作审计记录 `{0}` 已存在（request_id 唯一）")]
    AuditExists(String),

    #[error("租户 `{0}` 不存在")]
    TenantNotFound(String),

    #[error("组织 `{0}` 不存在")]
    OrgNotFound(String),

    #[error("组织 `{0}` 已存在（org_id 唯一）")]
    OrgAlreadyExists(String),

    #[error("用户 `{0}` 不存在")]
    UserNotFound(String),

    #[error("用户名 `{0}` 在多个组织各自注册，跨组织登录歧义（B1 起用户名全局唯一）")]
    UsernameAmbiguous(String),

    #[error("用户 `{0}` 已存在（tenant 内用户名唯一）")]
    UsernameTaken(String),

    #[error("角色 `{0}` 非法")]
    InvalidRole(String),

    #[error("数据集 `{dataset}` 类型为 {actual}，不接受{expected}条目（数据集类型创建后不可变更）")]
    DatasetKindMismatch {
        dataset: String,
        expected: &'static str,
        actual: &'static str,
    },

    #[error("快照包 `{0}` 混含规则条目与数据条目：MVP 数据集类型单一（rule_set 或 knowledge 二选一），混合包请拆分")]
    MixedBundleKinds(String),

    #[error("数据条目 `{entry}` 的 schema_ref 缺失或为空")]
    KnowledgeMissingSchemaRef { entry: String },
}

/// 条目二态视图（Q12 R4）：顶层 `/entries/{id}` 路由跨表（规则/知识平行表）定位后的统一返回
#[derive(Debug, Clone)]
pub enum AnyEntry {
    Rule(RuleEntry),
    Knowledge(KnowledgeEntry),
}

impl AnyEntry {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            AnyEntry::Rule(e) => serde_json::to_value(e).unwrap_or(serde_json::Value::Null),
            AnyEntry::Knowledge(e) => serde_json::to_value(e).unwrap_or(serde_json::Value::Null),
        }
    }

    pub fn is_frozen(&self) -> bool {
        match self {
            AnyEntry::Rule(e) => e.is_frozen(),
            AnyEntry::Knowledge(e) => e.is_frozen(),
        }
    }

    pub fn is_llm_generated(&self) -> bool {
        match self {
            AnyEntry::Rule(e) => e
                .governance
                .as_ref()
                .map(|g| g.is_llm_generated())
                .unwrap_or(false),
            AnyEntry::Knowledge(e) => e.is_llm_generated(),
        }
    }

    pub fn entry_id(&self) -> &str {
        match self {
            AnyEntry::Rule(e) => &e.entry_id,
            AnyEntry::Knowledge(e) => &e.entry_id,
        }
    }

    pub fn dataset_id(&self) -> &str {
        match self {
            AnyEntry::Rule(e) => &e.dataset_id,
            AnyEntry::Knowledge(e) => &e.dataset_id,
        }
    }
}

/// 规则存储
pub struct RuleStore {
    /// rusqlite `Connection` 为 Send 但非 Sync，axum 跨线程共享需 `Mutex` 包裹
    conn: std::sync::Mutex<Connection>,
    /// 领域 schema 目录（Q12 D3）：`{db 同级}/domain_schemas/` 下 *.json，
    /// 以 schema `$id`（缺省取文件名）为 `schema_ref` URI 索引。bundle 仓不内置领域，宿主注入。
    domain_schema_dir: Option<std::path::PathBuf>,
    /// 领域 schema 缓存（懒加载一次；新增 schema 需重启生效——MVP 如实标注）
    domain_schema_cache:
        std::sync::Mutex<Option<std::collections::BTreeMap<String, serde_json::Value>>>,
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

/// service_catalog 行原始列（rusqlite 闭包只读原始列，JSON 反序列化移到闭包外）
type ServiceCatalogRow = (
    String,         // service_name
    String,         // version
    Option<String>, // description
    Option<String>, // io_contract (JSON)
    i64,            // sensitive (0/1)
    String,         // binding_hint (JSON)
    String,         // managed_by
    String,         // scope
    String,         // created_at
    Option<String>, // updated_at
);

impl RuleStore {
    /// 打开（或创建）数据库文件
    ///
    /// 领域 schema 目录（Q12 D3）：与 db 文件同级的 `domain_schemas/`（内存库无目录，resolver 未命中 = 拒绝）。
    pub fn open(path: &str) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        let domain_schema_dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.join("domain_schemas"));
        let store = Self {
            conn: std::sync::Mutex::new(conn),
            domain_schema_dir,
            domain_schema_cache: std::sync::Mutex::new(None),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// 内存库（测试用）
    pub fn in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: std::sync::Mutex::new(conn),
            domain_schema_dir: None,
            domain_schema_cache: std::sync::Mutex::new(None),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// 领域 schema 解析（Q12 D3，bundle SSOT 门禁注入点）：
    /// 按目录索引查 `schema_ref` URI；未命中返回 None（门禁显式拒绝，不静默放行）。
    ///
    /// 索引键：schema 的 `$id` 字段（**必须是合法 URI**，jsonschema 校验器强制——裸词如
    /// `rpsm-body` 会导致 payload 校验报"领域 schema 本身非法"）；无 `$id` 时回退文件名
    /// （此时 schema 本身可过校验，但建议统一写 `$id`）。
    ///
    /// 用法（`DomainSchemaResolver = &dyn Fn`，闭包须由调用方局部绑定后取引用）：
    /// `let resolver = |uri: &str| store.lookup_domain_schema(uri);`
    pub fn lookup_domain_schema(&self, uri: &str) -> Option<serde_json::Value> {
        {
            let cache = self.domain_schema_cache.lock().unwrap();
            if let Some(map) = cache.as_ref() {
                return map.get(uri).cloned();
            }
        }
        let dir = self.domain_schema_dir.as_ref()?;
        let mut map = std::collections::BTreeMap::new();
        let Ok(dirs) = std::fs::read_dir(dir) else {
            return None;
        };
        for e in dirs.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let key = v
                .get("$id")
                .and_then(|i| i.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    p.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            if !key.is_empty() {
                map.insert(key, v);
            }
        }
        let mut cache = self.domain_schema_cache.lock().unwrap();
        *cache = Some(map);
        cache.as_ref().unwrap().get(uri).cloned()
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

            -- 33 号 §6 / C1（内容哈希落库去重）：内容寻址快照，key=(dataset, content_hash)。
            -- 未变条目跨版本复用同一快照行（零拷贝，rule_body 不重复存储）；entries 仍内联
            -- rule_body 以兼容现有读取路径与既有库动态迁移（物理去重/移除内联列为存储层后续项）。
            -- created_at 用于去重统计与可见性。
            CREATE TABLE IF NOT EXISTS entry_snapshots (
                dataset_id    TEXT NOT NULL,
                content_hash  TEXT NOT NULL,
                rule_body     TEXT NOT NULL,               -- evorule 原生 JSON（content_hash 的内容源）
                created_at    TEXT NOT NULL,
                PRIMARY KEY (dataset_id, content_hash),
                FOREIGN KEY (dataset_id) REFERENCES datasets(dataset_id)
            );
            CREATE INDEX IF NOT EXISTS idx_snapshots_ds ON entry_snapshots(dataset_id);

            -- 45 号批次1 / 33 号 §6：数据集版本内容归因快照（闭合 C 类残留）。
            -- 记录每个数据集版本的条目 content_hash 归因（不可变）："哪一数据集版本含哪几条快照"。
            -- 由 create_dataset_version 落库；version_diff 据此做内容归因级 diff（先于 PostgreSQL 迁移，
            -- 治理数据 MVP 仍 SQLite，45 号 §5 如实标注）。
            CREATE TABLE IF NOT EXISTS dataset_versions (
                dataset_id   TEXT NOT NULL,
                version      TEXT NOT NULL,               -- 数据集版本号（v1 / v1.p1）
                entry_hash   TEXT NOT NULL,               -- 条目 content_hash
                entry_id     TEXT NOT NULL,               -- 冗余便于归因可读（非唯一：跨版本复用）
                created_at   TEXT NOT NULL,
                PRIMARY KEY (dataset_id, version, entry_hash),
                FOREIGN KEY (dataset_id) REFERENCES datasets(dataset_id)
            );
            CREATE INDEX IF NOT EXISTS idx_dsver_ds ON dataset_versions(dataset_id, version);

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

            -- B1 双层租户（2026-08-31 用户裁定）：org 层与成员关系。
            -- tenants 表保留为 platform 注册表（每实例一行）；org 承接原租户的数据隔离语义
            -- （datasets.tenant_id 等字段语义平移为 org id，字段名不变零迁移）。
            CREATE TABLE IF NOT EXISTS orgs (
                org_id     TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                disabled   INTEGER NOT NULL DEFAULT 0,   -- 停用的 org 拒绝新登录/刷新
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS user_org_memberships (
                user_id    TEXT NOT NULL,
                org_id     TEXT NOT NULL,
                role       TEXT NOT NULL,                -- viewer/rule_engineer/approver/admin/platform_admin
                created_at TEXT NOT NULL,
                PRIMARY KEY (user_id, org_id),
                FOREIGN KEY (user_id) REFERENCES users(user_id)
            );
            CREATE INDEX IF NOT EXISTS idx_membership_org
                ON user_org_memberships(org_id);

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

            -- 02 方案 C2：服务目录（服务名/契约治理侧 SSOT；scope=platform 官方 + tenant 租户自定义）
            CREATE TABLE IF NOT EXISTS service_catalog (
                service_name TEXT PRIMARY KEY,
                version      TEXT NOT NULL DEFAULT '1.0.0',
                description  TEXT,
                io_contract  TEXT,                          -- JSON nullable
                sensitive    INTEGER NOT NULL DEFAULT 0,
                binding_hint TEXT NOT NULL DEFAULT 'native',
                managed_by   TEXT NOT NULL,                 -- official | org:<tenant_id>
                scope        TEXT NOT NULL,                 -- platform | tenant:<tenant_id>
                created_at   TEXT NOT NULL,
                updated_at   TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_catalog_scope ON service_catalog(scope);
            "#,
        )?;
        // 轻量迁移：若旧库 entries 表缺 35 号新增的 consumed_inputs 列，则补齐
        // （CREATE TABLE IF NOT EXISTS 不会为已存在表加列，需显式 ALTER）
        let _ = conn.execute("ALTER TABLE entries ADD COLUMN consumed_inputs TEXT NOT NULL DEFAULT '[]'", []);
        // Q12 数据资产化 R1：datasets 表补 dataset_kind 列（存量默认 rule_set，零迁移成本）
        let _ = conn.execute("ALTER TABLE datasets ADD COLUMN dataset_kind TEXT NOT NULL DEFAULT 'rule_set'", []);
        // Q12 数据资产化 R3：knowledge 条目平行表（方案 D 定案：rule 查询热路径零扰动）
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS knowledge_entries (
                dataset_id   TEXT NOT NULL,
                entry_id     TEXT NOT NULL,
                version      INTEGER NOT NULL,
                status       TEXT,
                provenance   TEXT NOT NULL,          -- JSON
                domain       TEXT NOT NULL,
                tags         TEXT NOT NULL DEFAULT '[]',  -- JSON array
                payload      TEXT NOT NULL,          -- 领域结构化 JSON（content_hash 的内容源，零转译）
                schema_ref   TEXT NOT NULL,          -- 领域 JSON Schema 引用 URI（D3）
                governance   TEXT,                   -- JSON nullable
                content_hash TEXT NOT NULL,
                PRIMARY KEY (dataset_id, entry_id, version),
                FOREIGN KEY (dataset_id) REFERENCES datasets(dataset_id)
            );
            CREATE INDEX IF NOT EXISTS idx_kentry_ds ON knowledge_entries(dataset_id);

            -- knowledge 条目状态迁移审计（only-append；独立于 entry_state_history，FK 指向平行表）
            CREATE TABLE IF NOT EXISTS knowledge_state_history (
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
                    REFERENCES knowledge_entries(dataset_id, entry_id, version)
            );

            -- knowledge 内容寻址快照去重（33 号 §6 同语义）
            CREATE TABLE IF NOT EXISTS knowledge_snapshots (
                dataset_id   TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                payload      TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                PRIMARY KEY (dataset_id, content_hash),
                FOREIGN KEY (dataset_id) REFERENCES datasets(dataset_id)
            );

            -- Q12 交付边界收口 B：knowledge_entries 全文索引（FTS5 trigram，中文 3-gram 可命中）
            -- 索引文本与 search_knowledge_entries 的 q 匹配域逐字段一致：
            -- entry_id + provenance.source + provenance.clause + payload（非整段 provenance JSON）
            CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_fts USING fts5(text, tokenize='trigram');
            CREATE TRIGGER IF NOT EXISTS knowledge_fts_ins AFTER INSERT ON knowledge_entries BEGIN
                INSERT INTO knowledge_fts(rowid, text) VALUES (
                    new.rowid,
                    new.entry_id || ' ' || COALESCE(json_extract(new.provenance, '$.source'), '') || ' ' ||
                    COALESCE(json_extract(new.provenance, '$.clause'), '') || ' ' || new.payload
                );
            END;
            CREATE TRIGGER IF NOT EXISTS knowledge_fts_del AFTER DELETE ON knowledge_entries BEGIN
                DELETE FROM knowledge_fts WHERE rowid = old.rowid;
            END;
            CREATE TRIGGER IF NOT EXISTS knowledge_fts_upd AFTER UPDATE ON knowledge_entries BEGIN
                DELETE FROM knowledge_fts WHERE rowid = old.rowid;
                INSERT INTO knowledge_fts(rowid, text) VALUES (
                    new.rowid,
                    new.entry_id || ' ' || COALESCE(json_extract(new.provenance, '$.source'), '') || ' ' ||
                    COALESCE(json_extract(new.provenance, '$.clause'), '') || ' ' || new.payload
                );
            END;
            -- 存量库回填（幂等：FTS 已有 rowid 不重插；每次 init 执行，防旧库升级漏数据）
            INSERT INTO knowledge_fts(rowid, text)
                SELECT rowid,
                       entry_id || ' ' || COALESCE(json_extract(provenance, '$.source'), '') || ' ' ||
                       COALESCE(json_extract(provenance, '$.clause'), '') || ' ' || payload
                FROM knowledge_entries
                WHERE rowid NOT IN (SELECT rowid FROM knowledge_fts);
            "#,
        )?;
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
               (dataset_id, tenant_id, name, description, dataset_kind, domain, tags, visibility,
                lifecycle, versioning, law_ref, version_selection, data_dependencies, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                ds.dataset_id,
                ds.tenant_id,
                ds.name,
                ds.description,
                ds.dataset_kind.as_str(),
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
            "SELECT dataset_id, tenant_id, name, description, dataset_kind, domain, tags, visibility,
                    lifecycle, versioning, law_ref, version_selection, data_dependencies, meta
             FROM datasets WHERE dataset_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![dataset_id], |r| {
            let lifecycle: String = r.get(8)?;
            let versioning: String = r.get(9)?;
            let meta: String = r.get(13)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                lifecycle,
                versioning,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
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
            dataset_kind,
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
            dataset_kind: DatasetKind::parse(&dataset_kind).unwrap_or_default(),
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
        // 外键依赖顺序：state_history → entries（含 knowledge 平行表）→ datasets
        conn.execute(
            "DELETE FROM entry_state_history WHERE dataset_id=?1",
            params![dataset_id],
        )?;
        conn.execute("DELETE FROM entries WHERE dataset_id=?1", params![dataset_id])?;
        conn.execute(
            "DELETE FROM knowledge_state_history WHERE dataset_id=?1",
            params![dataset_id],
        )?;
        conn.execute(
            "DELETE FROM knowledge_snapshots WHERE dataset_id=?1",
            params![dataset_id],
        )?;
        conn.execute(
            "DELETE FROM knowledge_entries WHERE dataset_id=?1",
            params![dataset_id],
        )?;
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

    /// 浏览口径列表（Q12 交付边界收口 A，V3 反转）：本租户全部 + 他租户 Public+Published。
    ///
    /// 与 [`Self::list_datasets`]（租户内严格口径，供检索/内部迭代）不同，本方法面向
    /// `GET /datasets` 浏览端点：他租户数据集仅在 `visibility=public` **且**
    /// `lifecycle.status=published` 时混入 —— 与 V1 详情端点双条件口径一致，列表→详情不出现断链。
    /// 条目级只读边界与写拒绝由各端点既有租户校验承接（不变）。
    pub fn list_datasets_browsable(&self, tenant_id: &str) -> Result<Vec<RuleDataset>, StoreError> {
        // visibility 列以 serde_json::to_string 存储（带引号，如 "public"），匹配须用同一序列化口径
        let public_flag = serde_json::to_string(&crate::model::dataset::Visibility::Public)?;
        let ids: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let ids = conn
                .prepare(
                    "SELECT dataset_id FROM datasets WHERE tenant_id = ?1 OR visibility = ?2",
                )?
                .query_map(params![tenant_id, public_flag], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            ids
        };
        let mut out = Vec::new();
        for id in &ids {
            let Some(ds) = self.get_dataset(id)? else {
                return Err(StoreError::DatasetNotFound(id.clone()));
            };
            if ds.tenant_id == tenant_id
                || (ds.visibility == crate::model::dataset::Visibility::Public
                    && ds.lifecycle.status == crate::model::lifecycle::LifecycleStatus::Published)
            {
                out.push(ds);
            }
        }
        Ok(out)
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
        // 旧版本号（触发升版的版本）：为它落库条目 content_hash 归因（45 号批次1 / C 类闭合）
        let old_version = ds.versioning.current.clone();
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
        // 内容归因落库（45 号批次1）：记录"旧版本含哪些条目快照"（POST-bump 前当前 entries = 旧版本内容）。
        // 跨版本未变内容复用同一 snapshot，归因行按 (version, content_hash) 唯一，可在多版本出现。
        // Q12 R3：union 规则条目与数据条目两表（平行表归因口径一致）。
        let rows: Vec<(String, String)> = conn
            .prepare(
                "SELECT entry_id, content_hash FROM entries WHERE dataset_id=?1
                 UNION ALL
                 SELECT entry_id, content_hash FROM knowledge_entries WHERE dataset_id=?1
                 ORDER BY entry_id",
            )?
            .query_map(params![dataset_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<_, _>>()?;
        {
            let mut stmt = conn.prepare(
                "INSERT INTO dataset_versions (dataset_id, version, entry_hash, entry_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (entry_id, content_hash) in rows {
                stmt.execute(params![dataset_id, old_version, content_hash, entry_id, at])?;
            }
        }
        Ok(new_version)
    }

    // ------------------------------------------------------------------
    // 条目 CRUD
    // ------------------------------------------------------------------

    /// 新增条目：校验（数据集存在 + 类型匹配 + 符号三方一致 + LLM 边界 + 唯一性）
    pub fn add_entry(&self, entry: &RuleEntry) -> Result<(), StoreError> {
        // 1) 数据集存在 + 类型匹配（Q12 R4：规则条目只进 rule_set 数据集）
        let ds = self
            .get_dataset(&entry.dataset_id)?
            .ok_or_else(|| StoreError::DatasetNotFound(entry.dataset_id.clone()))?;
        if ds.dataset_kind != DatasetKind::RuleSet {
            return Err(StoreError::DatasetKindMismatch {
                dataset: ds.dataset_id.clone(),
                expected: "规则（rule）",
                actual: ds.dataset_kind.as_str(),
            });
        }
        // 2) 符号三方一致
        Validator::validate_symbol_consistency(&ds, entry)?;
        // 3) LLM 边界
        Validator::validate_llm_boundary(entry)?;
        // 3b) 执行侧条目门禁（C8）：与 server 执行侧导入同口径（BundleImporter::validate_entry
        //     SSOT：rule_body 结构 + 符号三方一致），消除"治理放行、执行拒收"窗口
        let declared_services: Vec<String> = ds
            .data_dependencies
            .as_ref()
            .map(|d| d.services.iter().map(|s| s.service_name.clone()).collect())
            .unwrap_or_default();
        let bundle_entry = BundleEntry {
            entry_id: entry.entry_id.clone(),
            entry_kind: EntryKind::Rule,
            rule_body: entry.rule_body.clone(),
            schema_ref: None,
            provenance: entry.provenance.clone(),
            domain: entry.domain.clone(),
            tags: entry.tags.clone(),
            dependencies: entry.data_source_binding.clone(),
        };
        let resolver = |uri: &str| self.lookup_domain_schema(uri);
        BundleImporter::validate_entry(&bundle_entry, &declared_services, &resolver)?;
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
        // 5a) 内容寻址快照去重落库（33 号 §6/C1）：未变内容跨版本复用同一快照行
        conn.execute(
            "INSERT INTO entry_snapshots(dataset_id, content_hash, rule_body, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(dataset_id, content_hash) DO NOTHING",
            params![
                entry.dataset_id,
                entry.content_hash(),
                serde_json::to_string(&entry.rule_body)?,
                epoch_ms_now(),
            ],
        )?;
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

    // ------------------------------------------------------------------
    // Knowledge 条目 CRUD 与生命周期（Q12 数据资产化 R3，方案 D：平行表 + 治理链复用）
    // ------------------------------------------------------------------

    /// 新增 knowledge 条目：校验（数据集存在且为 knowledge 类型 + D3 领域 schema 强校验
    /// 经 BundleImporter::validate_entry 同一 SSOT 门禁 + 唯一性），写入平行表。
    pub fn add_knowledge_entry(&self, entry: &KnowledgeEntry) -> Result<(), StoreError> {
        // 1) 数据集存在 + 类型匹配（数据条目只进 knowledge 数据集）
        let ds = self
            .get_dataset(&entry.dataset_id)?
            .ok_or_else(|| StoreError::DatasetNotFound(entry.dataset_id.clone()))?;
        if ds.dataset_kind != DatasetKind::Knowledge {
            return Err(StoreError::DatasetKindMismatch {
                dataset: ds.dataset_id.clone(),
                expected: "数据（knowledge）",
                actual: ds.dataset_kind.as_str(),
            });
        }
        // 2) schema_ref 必填（store 层兜底；bundle 门禁内同样拒绝，双层防御）
        if entry.schema_ref.trim().is_empty() {
            return Err(StoreError::KnowledgeMissingSchemaRef {
                entry: entry.entry_id.clone(),
            });
        }
        // 3) LLM 边界（37 号强约束同口径：LLM 产出只能停留 Draft）
        if entry.is_llm_generated() && entry.status != Some(LifecycleStatus::Draft) {
            return Err(StoreError::Validation(
                ValidationError::LlmGeneratedNotDraft {
                    entry: entry.entry_id.clone(),
                    status: entry.status.unwrap_or(LifecycleStatus::Active),
                },
            ));
        }
        // 4) D3 门禁（SSOT）：payload 过领域 jsonschema 校验；resolver 未命中 = 拒绝
        let bundle_entry = BundleEntry {
            entry_id: entry.entry_id.clone(),
            entry_kind: EntryKind::Knowledge,
            rule_body: entry.payload.clone(),
            schema_ref: Some(entry.schema_ref.clone()),
            provenance: entry.provenance.clone(),
            domain: entry.domain.clone(),
            tags: entry.tags.clone(),
            dependencies: vec![],
        };
        let resolver = |uri: &str| self.lookup_domain_schema(uri);
        BundleImporter::validate_entry(&bundle_entry, &[], &resolver)?;
        // 5) 唯一性（主键保证，此处显式检查以便友好报错）
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM knowledge_entries WHERE dataset_id=?1 AND entry_id=?2 AND version=?3)",
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
        // 6) 写入（内容寻址快照去重落库，33 号 §6/C1 同语义）
        conn.execute(
            "INSERT INTO knowledge_snapshots(dataset_id, content_hash, payload, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(dataset_id, content_hash) DO NOTHING",
            params![
                entry.dataset_id,
                entry.content_hash(),
                serde_json::to_string(&entry.payload)?,
                epoch_ms_now(),
            ],
        )?;
        conn.execute(
            "INSERT INTO knowledge_entries
               (dataset_id, entry_id, version, status, provenance, domain, tags,
                payload, schema_ref, governance, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.dataset_id,
                entry.entry_id,
                entry.version,
                entry.status.map(|s| serde_json::to_string(&s)).transpose()?,
                serde_json::to_string(&entry.provenance)?,
                entry.domain,
                serde_json::to_string(&entry.tags)?,
                serde_json::to_string(&entry.payload)?,
                entry.schema_ref,
                entry.governance.as_ref().map(serde_json::to_string).transpose()?,
                entry.content_hash(),
            ],
        )?;
        Ok(())
    }

    /// 取 knowledge 条目（指定版本）
    pub fn get_knowledge_entry(
        &self,
        dataset_id: &str,
        entry_id: &str,
        version: u32,
    ) -> Result<Option<KnowledgeEntry>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT entry_id, dataset_id, version, status, provenance, domain, tags,
                    payload, schema_ref, governance
             FROM knowledge_entries WHERE dataset_id=?1 AND entry_id=?2 AND version=?3",
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
                r.get::<_, Option<String>>(9)?,
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
            payload,
            schema_ref,
            governance,
        ) = row?;
        Ok(Some(KnowledgeEntry {
            entry_id,
            dataset_id,
            version,
            status: status
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
            provenance: serde_json::from_str(&provenance)?,
            domain,
            tags: serde_json::from_str(&tags)?,
            payload: serde_json::from_str(&payload)?,
            schema_ref,
            governance: governance
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
        }))
    }

    /// 取 knowledge 条目最新版本
    pub fn get_latest_knowledge_entry(
        &self,
        dataset_id: &str,
        entry_id: &str,
    ) -> Result<Option<KnowledgeEntry>, StoreError> {
        let version: Option<u32> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT MAX(version) FROM knowledge_entries WHERE dataset_id=?1 AND entry_id=?2",
                params![dataset_id, entry_id],
                |r| r.get(0),
            )
            .ok();
        let Some(v) = version else {
            return Ok(None);
        };
        self.get_knowledge_entry(dataset_id, entry_id, v)
    }

    /// 列出 knowledge 数据集下某版本的全部数据条目（None = 各 entry_id 取最新版本）
    pub fn list_knowledge_entries(
        &self,
        dataset_id: &str,
        version: Option<u32>,
    ) -> Result<Vec<KnowledgeEntry>, StoreError> {
        let version = match version {
            Some(v) => v,
            None => {
                let entry_ids: Vec<String> = {
                    let conn = self.conn.lock().unwrap();
                    let ids: Vec<String> = conn
                        .prepare(
                            "SELECT DISTINCT entry_id FROM knowledge_entries WHERE dataset_id = ?1",
                        )?
                        .query_map(params![dataset_id], |r| r.get(0))?
                        .collect::<Result<_, _>>()?;
                    ids
                };
                let mut out = Vec::new();
                for id in entry_ids {
                    if let Some(e) = self.get_latest_knowledge_entry(dataset_id, &id)? {
                        out.push(e);
                    }
                }
                return Ok(out);
            }
        };
        let entry_ids: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let ids: Vec<String> = conn
                .prepare(
                    "SELECT entry_id FROM knowledge_entries WHERE dataset_id=?1 AND version=?2",
                )?
                .query_map(params![dataset_id, version], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            ids
        };
        entry_ids
            .iter()
            .map(|id| {
                self.get_knowledge_entry(dataset_id, id, version)?
                    .ok_or_else(|| StoreError::EntryNotFound {
                        dataset: dataset_id.into(),
                        entry: id.clone(),
                    })
            })
            .collect()
    }

    /// knowledge 条目版本历史（升序返回全部版本，同 list_entry_versions 语义）
    pub fn list_knowledge_entry_versions(
        &self,
        dataset_id: &str,
        entry_id: &str,
    ) -> Result<Vec<KnowledgeEntry>, StoreError> {
        let mut out = Vec::new();
        let versions: Vec<u32> = {
            let conn = self.conn.lock().unwrap();
            let vs: Vec<u32> = conn
                .prepare(
                    "SELECT version FROM knowledge_entries WHERE dataset_id=?1 AND entry_id=?2 ORDER BY version",
                )?
                .query_map(params![dataset_id, entry_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            vs
        };
        for v in versions {
            if let Some(e) = self.get_knowledge_entry(dataset_id, entry_id, v)? {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// 更新 knowledge 草稿条目（frozen 拒绝原地修改，§9-2 快照模式；无符号三方一致——数据条目不进 TCB）
    pub fn update_draft_knowledge_entry(&self, entry: &KnowledgeEntry) -> Result<(), StoreError> {
        if entry.is_frozen() {
            return Err(StoreError::EntryFrozen {
                dataset: entry.dataset_id.clone(),
                entry: entry.entry_id.clone(),
            });
        }
        if entry.schema_ref.trim().is_empty() {
            return Err(StoreError::KnowledgeMissingSchemaRef {
                entry: entry.entry_id.clone(),
            });
        }
        // D3 门禁（SSOT）复检：payload 过领域 schema
        let bundle_entry = BundleEntry {
            entry_id: entry.entry_id.clone(),
            entry_kind: EntryKind::Knowledge,
            rule_body: entry.payload.clone(),
            schema_ref: Some(entry.schema_ref.clone()),
            provenance: entry.provenance.clone(),
            domain: entry.domain.clone(),
            tags: entry.tags.clone(),
            dependencies: vec![],
        };
        let resolver = |uri: &str| self.lookup_domain_schema(uri);
        BundleImporter::validate_entry(&bundle_entry, &[], &resolver)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO knowledge_snapshots(dataset_id, content_hash, payload, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(dataset_id, content_hash) DO NOTHING",
            params![
                entry.dataset_id,
                entry.content_hash(),
                serde_json::to_string(&entry.payload)?,
                epoch_ms_now(),
            ],
        )?;
        let n = conn.execute(
            "UPDATE knowledge_entries SET status=?3, provenance=?4, domain=?5, tags=?6,
                    payload=?7, schema_ref=?8, governance=?9, content_hash=?10
             WHERE dataset_id=?1 AND entry_id=?2 AND version=?11",
            params![
                entry.dataset_id,
                entry.entry_id,
                entry.status.map(|s| serde_json::to_string(&s)).transpose()?,
                serde_json::to_string(&entry.provenance)?,
                entry.domain,
                serde_json::to_string(&entry.tags)?,
                serde_json::to_string(&entry.payload)?,
                entry.schema_ref,
                entry.governance.as_ref().map(serde_json::to_string).transpose()?,
                entry.content_hash(),
                entry.version,
            ],
        )?;
        if n == 0 {
            return Err(StoreError::EntryNotFound {
                dataset: entry.dataset_id.clone(),
                entry: entry.entry_id.clone(),
            });
        }
        Ok(())
    }

    /// 删除 knowledge 条目（仅 Draft 态）
    pub fn delete_knowledge_entry(
        &self,
        dataset_id: &str,
        entry_id: &str,
    ) -> Result<(), StoreError> {
        let Some(entry) = self.get_latest_knowledge_entry(dataset_id, entry_id)? else {
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
            "DELETE FROM knowledge_state_history WHERE dataset_id=?1 AND entry_id=?2",
            params![dataset_id, entry_id],
        )?;
        conn.execute(
            "DELETE FROM knowledge_entries WHERE dataset_id=?1 AND entry_id=?2",
            params![dataset_id, entry_id],
        )?;
        Ok(())
    }

    /// knowledge 条目级状态迁移（44 号 §5 同口径：闸门一/闸门二/拒绝；only-append 审计）
    pub fn transition_knowledge_entry_status(
        &self,
        dataset_id: &str,
        entry_id: &str,
        to: LifecycleStatus,
        by: &str,
        at: &str,
        cause: &str,
    ) -> Result<(), StoreError> {
        let Some(mut entry) = self.get_latest_knowledge_entry(dataset_id, entry_id)? else {
            return Err(StoreError::EntryNotFound {
                dataset: dataset_id.into(),
                entry: entry_id.into(),
            });
        };
        let from = entry.status.unwrap_or(LifecycleStatus::Active);
        // LLM 产出只能停留 Draft（37 号强约束，同 RuleEntry 口径）
        if entry.is_llm_generated() && to != LifecycleStatus::Draft {
            return Err(StoreError::Validation(
                ValidationError::LlmGeneratedNotDraft {
                    entry: entry_id.into(),
                    status: to,
                },
            ));
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
            "UPDATE knowledge_entries SET status=?1, governance=?2
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
            "INSERT INTO knowledge_state_history
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

    /// knowledge 条目状态迁移历史（only-append 只读）
    pub fn get_knowledge_entry_state_history(
        &self,
        dataset_id: &str,
        entry_id: &str,
    ) -> Result<Vec<StateChange>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT from_state, to_state, at, by, cause FROM knowledge_state_history
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

    /// 条目版本历史（C1：内容寻址落库后可回查；33 号 §6 历史可回查）。升序返回全部版本。
    pub fn list_entry_versions(
        &self,
        dataset_id: &str,
        entry_id: &str,
    ) -> Result<Vec<RuleEntry>, StoreError> {
        let mut out = Vec::new();
        let versions: Vec<u32> = {
            let conn = self.conn.lock().unwrap();
            let vs = conn.prepare(
                "SELECT version FROM entries WHERE dataset_id=?1 AND entry_id=?2 ORDER BY version",
            )?
            .query_map(params![dataset_id, entry_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
            vs
        };
        for v in versions {
            if let Some(e) = self.get_entry(dataset_id, entry_id, v)? {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// 内容级 diff（C2，44 号 §9 / 33 号）：对比条目两个版本的**内容快照**。
    ///
    /// - `content_hash` 相同 → 未变；不同 → 已变（给出规则体 JSON 键级差异摘要 added/removed/changed）。
    /// - 同一 `content_hash` 跨版本共享同一快照行（33 号 §6 去重语义）。
    pub fn entry_content_diff(
        &self,
        dataset_id: &str,
        entry_id: &str,
        from_version: u32,
        to_version: u32,
    ) -> Result<serde_json::Value, StoreError> {
        if to_version <= from_version {
            return Err(StoreError::InvalidDiffRange {
                from: from_version.to_string(),
                to: to_version.to_string(),
            });
        }
        let from = self
            .get_entry(dataset_id, entry_id, from_version)?
            .ok_or_else(|| StoreError::EntryVersionNotFound {
                dataset: dataset_id.into(),
                entry: entry_id.into(),
                version: from_version,
            })?;
        let to = self
            .get_entry(dataset_id, entry_id, to_version)?
            .ok_or_else(|| StoreError::EntryVersionNotFound {
                dataset: dataset_id.into(),
                entry: entry_id.into(),
                version: to_version,
            })?;

        let from_hash = from.content_hash();
        let to_hash = to.content_hash();
        let mut result = serde_json::json!({
            "dataset_id": dataset_id,
            "entry_id": entry_id,
            "from": from_version,
            "to": to_version,
            "from_content_hash": from_hash,
            "to_content_hash": to_hash,
            "changed": from_hash != to_hash,
            "note": "内容级 diff：content_hash 刻定规则体内容；跨版本共享同一快照行（33 号 §6）",
        });
        if from_hash != to_hash {
            let (added, removed, changed) = json_keywise_diff(&from.rule_body, &to.rule_body);
            result["keys"] = serde_json::json!({ "added": added, "removed": removed, "changed": changed });
        }
        Ok(result)
    }

    /// knowledge 条目内容级 diff（Q12 R4，与 entry_content_diff 同语义：payload 刻定内容）
    pub fn knowledge_entry_content_diff(
        &self,
        dataset_id: &str,
        entry_id: &str,
        from_version: u32,
        to_version: u32,
    ) -> Result<serde_json::Value, StoreError> {
        if to_version <= from_version {
            return Err(StoreError::InvalidDiffRange {
                from: from_version.to_string(),
                to: to_version.to_string(),
            });
        }
        let from = self
            .get_knowledge_entry(dataset_id, entry_id, from_version)?
            .ok_or_else(|| StoreError::EntryVersionNotFound {
                dataset: dataset_id.into(),
                entry: entry_id.into(),
                version: from_version,
            })?;
        let to = self
            .get_knowledge_entry(dataset_id, entry_id, to_version)?
            .ok_or_else(|| StoreError::EntryVersionNotFound {
                dataset: dataset_id.into(),
                entry: entry_id.into(),
                version: to_version,
            })?;
        let from_hash = from.content_hash();
        let to_hash = to.content_hash();
        let mut result = serde_json::json!({
            "dataset_id": dataset_id,
            "entry_id": entry_id,
            "from": from_version,
            "to": to_version,
            "from_content_hash": from_hash,
            "to_content_hash": to_hash,
            "changed": from_hash != to_hash,
            "note": "内容级 diff：content_hash 刻定 payload 内容；跨版本共享同一快照行（33 号 §6）",
        });
        if from_hash != to_hash {
            let (added, removed, changed) = json_keywise_diff(&from.payload, &to.payload);
            result["keys"] = serde_json::json!({ "added": added, "removed": removed, "changed": changed });
        }
        Ok(result)
    }

    /// 内容去重统计（C1）：数据集内 `entries` 行数 vs 去重后 `entry_snapshots` 快照数。
    pub fn snapshot_dedup_stats(&self, dataset_id: &str) -> Result<serde_json::Value, StoreError> {
        let conn = self.conn.lock().unwrap();
        let entry_rows: u64 = conn.query_row(
            "SELECT COUNT(*) FROM entries WHERE dataset_id=?1",
            params![dataset_id],
            |r| r.get(0),
        )?;
        let snapshots: u64 = conn.query_row(
            "SELECT COUNT(*) FROM entry_snapshots WHERE dataset_id=?1",
            params![dataset_id],
            |r| r.get(0),
        )?;
        Ok(serde_json::json!({
            "dataset_id": dataset_id,
            "entry_version_rows": entry_rows,
            "distinct_snapshots": snapshots,
            "dedup_ratio": if entry_rows == 0 { serde_json::Value::Null } else {
                serde_json::json!((snapshots as f64) / (entry_rows as f64))
            },
        }))
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
        // 内容寻址快照去重落库（33 号 §6/C1）
        conn.execute(
            "INSERT INTO entry_snapshots(dataset_id, content_hash, rule_body, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(dataset_id, content_hash) DO NOTHING",
            params![
                entry.dataset_id,
                entry.content_hash(),
                serde_json::to_string(&entry.rule_body)?,
                epoch_ms_now(),
            ],
        )?;
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
        // Q12 R3：knowledge 条目 payload 同样纳入扫描（数据资产同样不得携带凭据）
        for entry in self.list_knowledge_entries(dataset_id, None)? {
            texts.push(serde_json::to_string(&entry.payload)?);
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
        // C3/C4/C6：服务契约 SSOT 下沉 —— 从服务目录（平台 + 本租户）构建补齐映射
        let catalog: std::collections::BTreeMap<String, ServiceCatalogEntry> = self
            .list_services(&ds.tenant_id)?
            .into_iter()
            .map(|e| (e.service_name.clone(), e))
            .collect();
        // Q12 R5：按数据集类型分流导出（knowledge 数据集 → Knowledge 条目 bundle，同一 SSOT）
        match ds.dataset_kind {
            DatasetKind::RuleSet => {
                let entries = self.list_entries(dataset_id, None)?;
                Ok(BundleExporter::export(
                    &ds,
                    &entries,
                    tests,
                    by,
                    at,
                    instance_id,
                    &catalog,
                ))
            }
            DatasetKind::Knowledge => {
                let entries = self.list_knowledge_entries(dataset_id, None)?;
                Ok(BundleExporter::export_knowledge(
                    &ds,
                    &entries,
                    tests,
                    by,
                    at,
                    instance_id,
                    &catalog,
                ))
            }
        }
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
    /// 故在租户各数据集最新版中查找首个匹配。Q12 R4：规则表与 knowledge 平行表均参与定位）
    pub fn find_entry_in_tenant(
        &self,
        tenant_id: &str,
        entry_id: &str,
    ) -> Result<Option<(String, AnyEntry)>, StoreError> {
        for ds in self.list_datasets(tenant_id)? {
            if let Some(e) = self.get_latest_entry(&ds.dataset_id, entry_id)? {
                return Ok(Some((ds.dataset_id, AnyEntry::Rule(e))));
            }
            if let Some(e) = self.get_latest_knowledge_entry(&ds.dataset_id, entry_id)? {
                return Ok(Some((ds.dataset_id, AnyEntry::Knowledge(e))));
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
    // 服务目录（02 方案 C2：服务名/契约治理侧 SSOT）
    // ------------------------------------------------------------------

    /// 注册/更新服务目录条目（upsert，主键 service_name）
    pub fn upsert_service(&self, e: &ServiceCatalogEntry) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO service_catalog
               (service_name, version, description, io_contract, sensitive, binding_hint,
                managed_by, scope, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(service_name) DO UPDATE SET
               version=excluded.version, description=excluded.description,
               io_contract=excluded.io_contract, sensitive=excluded.sensitive,
               binding_hint=excluded.binding_hint, managed_by=excluded.managed_by,
               scope=excluded.scope, updated_at=excluded.updated_at",
            params![
                e.service_name,
                e.version,
                e.description,
                e.io_contract.as_ref().map(serde_json::to_string).transpose()?,
                e.sensitive as i64,
                serde_json::to_string(&e.binding_hint)?,
                e.managed_by,
                e.scope,
                e.created_at,
                e.updated_at,
            ],
        )?;
        Ok(())
    }

    /// 取服务目录条目
    pub fn get_service(&self, service_name: &str) -> Result<Option<ServiceCatalogEntry>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT service_name, version, description, io_contract, sensitive, binding_hint,
                    managed_by, scope, created_at, updated_at
             FROM service_catalog WHERE service_name = ?1",
        )?;
        let row = stmt
            .query_row(params![service_name], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, Option<String>>(9)?,
                ))
            })
            .optional()?;
        let Some((
            service_name,
            version,
            description,
            io_contract,
            sensitive,
            binding_hint,
            managed_by,
            scope,
            created_at,
            updated_at,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(ServiceCatalogEntry {
            service_name,
            version,
            description,
            io_contract: io_contract
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            sensitive: sensitive != 0,
            binding_hint: serde_json::from_str(&binding_hint)?,
            managed_by,
            scope,
            created_at,
            updated_at,
        }))
    }

    /// 服务目录列表：平台官方（scope=platform）+ 本租户自定义
    pub fn list_services(&self, tenant_id: &str) -> Result<Vec<ServiceCatalogEntry>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT service_name, version, description, io_contract, sensitive, binding_hint,
                    managed_by, scope, created_at, updated_at
             FROM service_catalog
             WHERE scope = 'platform' OR scope = ?1
             ORDER BY managed_by, service_name",
        )?;
        let rows = stmt.query_map(params![format!("tenant:{tenant_id}")], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })?;
        let raw: Vec<ServiceCatalogRow> = rows.collect::<Result<_, _>>()?;
        raw.into_iter()
            .map(
                |(service_name, version, description, io_contract, sensitive, binding_hint, managed_by, scope, created_at, updated_at)| {
                    Ok(ServiceCatalogEntry {
                        service_name,
                        version,
                        description,
                        io_contract: io_contract
                            .as_deref()
                            .map(serde_json::from_str)
                            .transpose()?,
                        sensitive: sensitive != 0,
                        binding_hint: serde_json::from_str(&binding_hint)?,
                        managed_by,
                        scope,
                        created_at,
                        updated_at,
                    })
                },
            )
            .collect()
    }

    /// C2：预置官方 7 个原生服务（幂等 seed，服务启动时调用）。
    ///
    /// 仅当平台官方目录（scope=platform）为空时补齐，避免覆盖运维/租户后续对
    /// 官方服务版本的自定义。返回本次新增条数。
    pub fn seed_official_services_if_empty(&self, now: &str) -> Result<usize, StoreError> {
        let platform_count: i64 = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM service_catalog WHERE scope = 'platform'",
                [],
                |r| r.get(0),
            )?
        };
        if platform_count > 0 {
            return Ok(0);
        }
        let mut inserted = 0usize;
        for (name, sensitive, desc) in OFFICIAL_NATIVE_SERVICES {
            self.upsert_service(&official_entry(name, *sensitive, desc, now))?;
            inserted += 1;
        }
        Ok(inserted)
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

    /// knowledge 数据条目检索（Q12 段2 P3）：与 [`Self::search_entries`] 同语义同过滤口径，
    /// 消除"rule 有过滤、knowledge 无过滤"的不对称。q 匹配域：entry_id/provenance/payload。
    ///
    /// q 匹配两段式（Q12 交付边界收口 B，FTS5 trigram 全文索引，中文 3-gram 可命中）：
    /// - q ≥3 字符：走 `knowledge_fts` 索引（trigram 对 ASCII 大小写不敏感、中文按 3-gram 子串命中）；
    /// - q <3 字符：trigram 无法索引，退回进程内 contains 扫描（匹配域一致，口径不变）。
    /// FTS 命中按 (dataset_id, entry_id) 归并且仅取最新版本行 —— 与条目迭代（每条目最新版）口径一致，
    /// 避免"旧版本命中、最新版已不含 q"的幽灵结果。FTS 不可用即报错（fail-fast），不做静默降级。
    pub fn search_knowledge_entries(
        &self,
        tenant_id: &str,
        dataset_id: Option<&str>,
        domain: Option<&str>,
        q: Option<&str>,
        tags: &[String],
        status: Option<LifecycleStatus>,
    ) -> Result<Vec<KnowledgeEntry>, StoreError> {
        // 全文命中集（q ≥3 字符；整串引号包裹成 phrase，内部双引号翻倍防注入/语法干扰）
        let fts_hits: Option<std::collections::HashSet<(String, String)>> = match q {
            Some(qs) if qs.chars().count() >= 3 => {
                let pattern = format!("\"{}\"", qs.replace('"', "\"\""));
                let conn = self.conn.lock().unwrap();
                let mut stmt = conn.prepare(
                    "SELECT k.dataset_id, k.entry_id FROM knowledge_fts f \
                     JOIN knowledge_entries k ON k.rowid = f.rowid \
                     WHERE knowledge_fts MATCH ?1 \
                     GROUP BY k.dataset_id, k.entry_id \
                     HAVING k.version = (SELECT MAX(version) FROM knowledge_entries k2 \
                                          WHERE k2.dataset_id = k.dataset_id AND k2.entry_id = k.entry_id)",
                )?;
                let hits = stmt
                    .query_map(params![pattern], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Some(hits.into_iter().collect())
            }
            _ => None,
        };
        let mut out = Vec::new();
        let lower_q = q.map(|s| s.to_lowercase());
        for ds in self.list_datasets(tenant_id)? {
            if let Some(did) = dataset_id {
                if ds.dataset_id != did {
                    continue;
                }
            }
            for e in self.list_knowledge_entries(&ds.dataset_id, None)? {
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
                    if let Some(hits) = &fts_hits {
                        if !hits.contains(&(ds.dataset_id.clone(), e.entry_id.clone())) {
                            continue;
                        }
                    } else {
                        let hay = format!(
                            "{} {} {} {}",
                            e.entry_id,
                            e.provenance.source,
                            e.provenance.clause.as_deref().unwrap_or(""),
                            e.payload
                        )
                        .to_lowercase();
                        if !hay.contains(lq) {
                            continue;
                        }
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
    /// - **结构级**（版本链增量 + 当前条目清单）始终返回；
    /// - **内容归因级**（45 号批次1 / C 类闭合）：对比 `from`/`to` 两版本在 `dataset_versions`
    ///   快照表中的条目 content_hash 归因，给出 added/removed/unchanged（跨版本未变内容复用同哈希）。
    ///   若 from 无归因记录（该版本未被升版留档，如从未升版过的初始版本），如实标注归因缺失，不伪造。
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
        // 内容归因（45 号批次1）：取两版本的归因哈希集。
        // - from 必为历史版本 → 读 dataset_versions 留档；
        // - to 若为当前版本（未升版，无留档）→ 用当前 entries 实时哈希；
        //   否则为历史版本 → 读留档。
        let from_set = self.dataset_version_hashes(dataset_id, from)?;
        let to_set = if ds.versioning.current == *to {
            entries.iter().map(|e| e.content_hash()).collect()
        } else {
            self.dataset_version_hashes(dataset_id, to)?
        };
        // from 无可归因记录（该版本从未被升版留档）→ 结构级回退，如实标注
        if from_set.is_empty() && to_set.is_empty() {
            return Ok(serde_json::json!({
                "dataset_id": dataset_id,
                "from": from,
                "to": to,
                "added_versions": &chain[fi + 1..=ti],
                "current_entry_count": entries.len(),
                "current_entry_ids": entry_ids,
                "content_attribution": null,
                "note": "结构级 diff（版本链增量 + 当前条目清单）；内容归因不可用：from/to 版本均无 dataset_versions 快照留档（初始版本未升版过）。45 号批次1 落库后，后续升版将产生归因。",
            }));
        }
        // 内容归因级 diff：按 content_hash 比较（跨版本复用同哈希 = 未变）
        let added: Vec<String> = to_set.difference(&from_set).cloned().collect();
        let removed: Vec<String> = from_set.difference(&to_set).cloned().collect();
        let unchanged: Vec<String> = from_set.intersection(&to_set).cloned().collect();
        Ok(serde_json::json!({
            "dataset_id": dataset_id,
            "from": from,
            "to": to,
            "added_versions": &chain[fi + 1..=ti],
            "current_entry_count": entries.len(),
            "current_entry_ids": entry_ids,
            "content_attribution": {
                "added": added,
                "removed": removed,
                "unchanged": unchanged,
                "note": "按条目 content_hash 归因（45 号批次1 dataset_versions 快照表）：跨版本未变内容复用同哈希，归为 unchanged。",
            },
        }))
    }

    /// 读取某数据集某版本在 `dataset_versions` 快照表中的条目 content_hash 归因集
    /// （45 号批次1；跨版本未变内容多版本共享，去重后作集合返回）。
    fn dataset_version_hashes(
        &self,
        dataset_id: &str,
        version: &str,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .prepare(
                "SELECT entry_hash FROM dataset_versions
                 WHERE dataset_id=?1 AND version=?2 ORDER BY entry_hash",
            )?
            .query_map(params![dataset_id, version], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
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

    /// 导入快照包：`BundleImporter::validate`（schema→防篡改→符号三方一致/数据条目 D3→版本解析→闸门一）通过后落库。
    ///
    /// - 数据集：不存在则新建（tenant=导入方），已存在则覆盖其版本链/依赖/锚；
    /// - 条目：按 `entry_kind` 分流——Rule → RuleEntry（entries 表）、Knowledge → KnowledgeEntry
    ///   （knowledge 平行表），治理版本=1，状态 Active；先清空旧条目再写入（可重试幂等）；
    /// - 混合 kind 的包显式拒绝（MVP 数据集类型单一，不静默混装）；
    /// - 校验链任一失败 → 显式错误（35 号 §9 硬失败，不静默降级）。
    pub fn import_bundle(
        &self,
        bundle: &DatasetBundle,
        tenant_id: &str,
        by: &str,
        at: &str,
        instance_id: &str,
    ) -> Result<ImportResult, StoreError> {
        let resolver = |uri: &str| self.lookup_domain_schema(uri);
        let result = BundleImporter::validate(bundle, &resolver)?;
        // Q12 R5：混合 kind 拒绝（数据集类型单一）
        let has_rule = bundle.entries.iter().any(|e| e.entry_kind == EntryKind::Rule);
        let has_knowledge = bundle
            .entries
            .iter()
            .any(|e| e.entry_kind == EntryKind::Knowledge);
        if has_rule && has_knowledge {
            return Err(StoreError::MixedBundleKinds(bundle.bundle_id.clone()));
        }
        let dataset_kind = if has_knowledge {
            DatasetKind::Knowledge
        } else {
            DatasetKind::RuleSet
        };
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
                dataset_kind,
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
        // 条目落库（Q12 R5：按 entry_kind 分流入对应平行表）
        for be in &bundle.entries {
            match be.entry_kind {
                EntryKind::Rule => {
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
                EntryKind::Knowledge => {
                    // validate 链已保证 schema_ref 存在且 payload 过领域 schema（D3）
                    let schema_ref = be.schema_ref.clone().ok_or_else(|| {
                        StoreError::KnowledgeMissingSchemaRef {
                            entry: be.entry_id.clone(),
                        }
                    })?;
                    let entry = KnowledgeEntry {
                        entry_id: be.entry_id.clone(),
                        dataset_id: did.clone(),
                        version: 1,
                        status: Some(LifecycleStatus::Active),
                        provenance: be.provenance.clone(),
                        domain: be.domain.clone(),
                        tags: be.tags.clone(),
                        payload: be.rule_body.clone(),
                        schema_ref,
                        governance: Some(Governance {
                            author: Some(by.into()),
                            updater: None,
                            llm_generated: None,
                            lifecycle_timestamps: None,
                        }),
                    };
                    self.add_knowledge_entry(&entry)?;
                }
            }
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
        // Q12 R5：knowledge 平行表同步清空（导入可重试幂等，两表口径一致）
        conn.execute(
            "DELETE FROM knowledge_state_history WHERE dataset_id=?1",
            params![dataset_id],
        )?;
        conn.execute(
            "DELETE FROM knowledge_entries WHERE dataset_id=?1",
            params![dataset_id],
        )?;
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

    // ------------------------------------------------------------------
    // B1 双层租户：org 与成员关系（2026-08-31 用户裁定）
    // ------------------------------------------------------------------

    /// 确保 platform 默认 org 存在（幂等；镜像默认租户，存量数据 tenant_id 即该 org id）
    pub fn ensure_default_org(&self, org_id: &str, name: &str, created_at: &str) -> Result<Org, StoreError> {
        self.conn.lock().unwrap().execute(
            "INSERT OR IGNORE INTO orgs (org_id, name, disabled, created_at)
             VALUES (?1, ?2, 0, ?3)",
            params![org_id, name, created_at],
        )?;
        self.get_org(org_id)?
            .ok_or_else(|| StoreError::OrgNotFound(org_id.into()))
    }

    pub fn create_org(&self, org: &Org) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        let inserted = conn.execute(
            "INSERT INTO orgs (org_id, name, disabled, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(org_id) DO NOTHING",
            params![org.org_id, org.name, org.disabled as i64, org.created_at],
        )?;
        if inserted == 0 {
            return Err(StoreError::OrgAlreadyExists(org.org_id.clone()));
        }
        Ok(())
    }

    pub fn get_org(&self, org_id: &str) -> Result<Option<Org>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT org_id, name, disabled, created_at FROM orgs WHERE org_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![org_id], |row| {
            Ok(Org {
                org_id: row.get(0)?,
                name: row.get(1)?,
                disabled: row.get::<_, i64>(2)? != 0,
                created_at: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(Ok(o)) => Ok(Some(o)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_orgs(&self) -> Result<Vec<Org>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT org_id, name, disabled, created_at FROM orgs ORDER BY created_at, org_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Org {
                org_id: row.get(0)?,
                name: row.get(1)?,
                disabled: row.get::<_, i64>(2)? != 0,
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 设置用户在某 org 的角色（upsert 成员关系；用户必须存在）
    pub fn upsert_user_org_role(
        &self,
        org_id: &str,
        user_id: &str,
        role: Role,
        created_at: &str,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        let exists = conn
            .prepare("SELECT 1 FROM users WHERE user_id = ?1")?
            .exists(params![user_id])?;
        if !exists {
            return Err(StoreError::UserNotFound(user_id.into()));
        }
        conn.execute(
            "INSERT INTO user_org_memberships (user_id, org_id, role, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, org_id) DO UPDATE SET role = excluded.role",
            params![user_id, org_id, role.as_str(), created_at],
        )?;
        Ok(())
    }

    /// 用户在某 org 的成员角色（无成员行返回 None）
    pub fn get_user_org_role(&self, org_id: &str, user_id: &str) -> Result<Option<Role>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT role FROM user_org_memberships WHERE org_id = ?1 AND user_id = ?2",
        )?;
        let mut rows = stmt.query_map(params![org_id, user_id], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(Ok(r)) => Ok(Role::parse(&r)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 用户的全 org 成员关系列表
    pub fn list_user_orgs(&self, user_id: &str) -> Result<Vec<UserOrg>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT org_id, user_id, role, created_at FROM user_org_memberships
             WHERE user_id = ?1 ORDER BY created_at, org_id",
        )?;
        let rows = stmt.query_map(params![user_id], |row| {
            let role_s: String = row.get(2)?;
            Ok(UserOrg {
                org_id: row.get(0)?,
                user_id: row.get(1)?,
                role: Role::parse(&role_s).unwrap_or(Role::Viewer),
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 某 org 的全部成员（B1 成员管理列表）
    pub fn list_user_orgs_in_org(&self, org_id: &str) -> Result<Vec<UserOrg>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT org_id, user_id, role, created_at FROM user_org_memberships
             WHERE org_id = ?1 ORDER BY created_at, user_id",
        )?;
        let rows = stmt.query_map(params![org_id], |row| {
            let role_s: String = row.get(2)?;
            Ok(UserOrg {
                org_id: row.get(0)?,
                user_id: row.get(1)?,
                role: Role::parse(&role_s).unwrap_or(Role::Viewer),
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 按用户名全局查用户（B1 跨 org 登录用；同用户名多行视为歧义，返回冲突错误）
    pub fn get_user_by_username_any(&self, username: &str) -> Result<Option<User>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_id, tenant_id, username, password_hash, salt, role, disabled, created_at, updated_at
             FROM users WHERE username = ?1",
        )?;
        let mut rows = stmt.query_map(params![username], row_to_user)?;
        match rows.next() {
            Some(Ok(u)) => {
                // 歧义防护：同用户名在多个 org 各自注册（历史行为允许）时拒绝全局解析
                if rows.next().is_some() {
                    return Err(StoreError::UsernameAmbiguous(username.into()));
                }
                Ok(Some(u))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
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

/// 单调 epoch 毫秒字符串（C1 快照 `created_at`，仅用于去重统计/顺序，非业务时间的权威来源）
fn epoch_ms_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

/// JSON 键级差异摘要（C2）：扁平化后比较两对象，返回 added/removed/changed 键路径。
fn json_keywise_diff(
    a: &serde_json::Value,
    b: &serde_json::Value,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    // 以"键路径（扁平化）+ 规范化叶子值"判定增删改
    let map_a = flatten(a);
    let map_b = flatten(b);
    let oa = map_a.as_object().cloned().unwrap_or_default();
    let ob = map_b.as_object().cloned().unwrap_or_default();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (k, v) in &oa {
        match ob.get(k) {
            None => removed.push(k.clone()),
            Some(vb) => {
                if vb != v {
                    changed.push(k.clone());
                }
            }
        }
    }
    for (k, _) in &ob {
        if !oa.contains_key(k) {
            added.push(k.clone());
        }
    }
    (added, removed, changed)
}

/// 将任意 JSON 扁平化为 `键路径 → 值`（数组元素以 `\N[i]` 索引）。
fn flatten(v: &serde_json::Value) -> serde_json::Value {
    fn walk(prefix: String, node: &serde_json::Value, acc: &mut serde_json::Map<String, serde_json::Value>) {
        match node {
            serde_json::Value::Object(m) => {
                for (k, c) in m {
                    let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    walk(p, c, acc);
                }
            }
            serde_json::Value::Array(arr) => {
                for (i, item) in arr.iter().enumerate() {
                    walk(format!("{prefix}\\{i}"), item, acc);
                }
            }
            other => {
                acc.insert(prefix, other.clone());
            }
        }
    }
    let mut acc = serde_json::Map::new();
    walk(String::new(), v, &mut acc);
    serde_json::Value::Object(acc)
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
            dataset_kind: Default::default(),
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
                    version: None,
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
    fn test_service_catalog_crud_and_seed() {
        // 02 方案 C2：服务目录 upsert/get/list/seed（幂等，平台 + 租户作用域）
        use crate::model::service_catalog::BindingHint;

        let store = RuleStore::in_memory().unwrap();
        // 官方 seed：预置 7 个原生服务；重复 seed 幂等
        assert_eq!(store.seed_official_services_if_empty("t").unwrap(), 7);
        assert_eq!(store.seed_official_services_if_empty("t").unwrap(), 0, "重复 seed 应幂等");

        // 平台官方目录对任意租户可见
        let platform = store.list_services("tenant_a").unwrap();
        assert_eq!(platform.len(), 7);
        let llm = store.get_service("llm_advisor").unwrap().unwrap();
        assert!(llm.sensitive, "llm_advisor 应标记 sensitive（C6）");
        assert_eq!(llm.scope, "platform");
        assert_eq!(llm.binding_hint, BindingHint::Native);

        // 租户注册/更新自定义服务
        store
            .upsert_service(&ServiceCatalogEntry {
                service_name: "payroll_svc".into(),
                version: "1.2.0".into(),
                description: Some("payroll".into()),
                io_contract: None,
                sensitive: true,
                binding_hint: BindingHint::Registry,
                managed_by: "org:tenant_a".into(),
                scope: "tenant:tenant_a".into(),
                created_at: "t".into(),
                updated_at: None,
            })
            .unwrap();
        let got = store.get_service("payroll_svc").unwrap().unwrap();
        assert_eq!(got.version, "1.2.0");
        assert!(got.sensitive);

        // 租户可见范围 = 官方 7 + 本租户 1；其他租户看不到租户自定义
        assert_eq!(store.list_services("tenant_a").unwrap().len(), 8);
        assert_eq!(store.list_services("tenant_b").unwrap().len(), 7);

        // 平台官方条目可被运维更新（seed 只在空目录时补齐，不覆盖既有）
        let mut e = store.get_service("llm_advisor").unwrap().unwrap();
        e.version = "2.0.0".into();
        store.upsert_service(&e).unwrap();
        assert_eq!(
            store.get_service("llm_advisor").unwrap().unwrap().version,
            "2.0.0"
        );
        assert_eq!(store.seed_official_services_if_empty("t").unwrap(), 0, "目录非空不再 seed");
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

    /// C8：入库门禁接执行侧 SSOT（BundleImporter::validate_entry）——
    /// 结构非法 rule_body（如空 transform）入库被拒，杜绝"治理放行、执行拒收"窗口
    #[test]
    fn test_add_entry_rejects_invalid_rule_body_structure() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        let mut e = draft_entry();
        // 清空绑定（否则符号一致校验先报 ServiceNotInRuleBody），聚焦结构门禁
        e.data_source_binding = vec![];
        e.rule_body = serde_json::json!({"transform": []});
        let err = store.add_entry(&e).unwrap_err();
        assert!(matches!(
            err,
            StoreError::Bundle(BundleError::InvalidEntryStructure { .. })
        ));
    }

    /// C1：内容哈希去重落库——跨版本未变内容复用同一快照行，变更产生新快照
    #[test]
    fn test_entry_snapshot_dedup() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();

        let mut v1 = draft_entry();
        v1.version = 1;
        store.add_entry(&v1).unwrap();

        // v2 与 v1 内容完全一致 → 去重，不应新增快照
        let mut v2 = draft_entry();
        v2.version = 2;
        store.add_entry(&v2).unwrap();

        // v3 内容变化（仅改 description，transform 仍引用 payroll_svc 以过符号一致性）→ 新快照
        let mut v3 = draft_entry();
        v3.version = 3;
        v3.rule_body["description"] = serde_json::json!("税率调整后的新规则");
        store.add_entry(&v3).unwrap();

        let stats = store.snapshot_dedup_stats("ds-tax-2024").unwrap();
        assert_eq!(stats["entry_version_rows"], 3);
        assert_eq!(stats["distinct_snapshots"], 2, "v1/v2 内容一致应共享同一快照");

        // 版本历史可回查（33 号 §6）
        let versions = store.list_entry_versions("ds-tax-2024", "tax-001").unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].version, 1);
        assert_eq!(versions[2].version, 3);
    }

    /// C2：条目内容级 diff（content_hash 刻定内容；变更给出键级差异）
    #[test]
    fn test_entry_content_diff() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();

        let mut v1 = draft_entry();
        v1.version = 1;
        v1.rule_body["description"] = serde_json::json!("初版规则说明"); // 让 description 成为两版共有键
        store.add_entry(&v1).unwrap();

        let mut v2 = draft_entry();
        v2.version = 2; // 内容与 v1 一致 → 应判未变
        v2.rule_body["description"] = serde_json::json!("初版规则说明");
        store.add_entry(&v2).unwrap();

        let mut v3 = draft_entry();
        v3.version = 3;
        v3.rule_body["description"] = serde_json::json!("改版规则说明");
        store.add_entry(&v3).unwrap();

        // 内容未变（v1→v2 相同 rule_body）→ changed=false
        let unchanged = store
            .entry_content_diff("ds-tax-2024", "tax-001", 1, 2)
            .unwrap();
        assert_eq!(unchanged["changed"], false);

        // 内容变化（v1→v3）→ changed=true，键级差异里含 description
        let changed_diff = store
            .entry_content_diff("ds-tax-2024", "tax-001", 1, 3)
            .unwrap();
        assert_eq!(changed_diff["changed"], true);
        assert!(changed_diff["keys"]["changed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k.as_str().unwrap().contains("description")));

        // 非法区间（from >= to）
        let err = store
            .entry_content_diff("ds-tax-2024", "tax-001", 3, 1)
            .unwrap_err();
        assert!(matches!(err, StoreError::InvalidDiffRange { .. }));

        // 版本不存在
        let err = store
            .entry_content_diff("ds-tax-2024", "tax-001", 1, 99)
            .unwrap_err();
        assert!(matches!(err, StoreError::EntryVersionNotFound { .. }));
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
        // 导出包可导入校验通过（快照包消费闭环；纯规则包 resolver 不命中任何领域 schema）
        let no_schema = |_uri: &str| None;
        let r = BundleImporter::validate(&bundle, &no_schema).unwrap();
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

    // ---------- Q12 数据资产化（R6）：knowledge 数据集 store 层专项 ----------

    /// 文件库 + 领域 schema 目录（D3 resolver 真实命中路径；in_memory 无目录 = resolver 未命中）
    /// 目录名带原子计数：cargo test 同进程多线程并行，须保证各测试目录互不冲突
    fn file_store_with_body_schema() -> (RuleStore, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "q12-store-test-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("domain_schemas")).unwrap();
        std::fs::write(
            dir.join("domain_schemas").join("rpsm-body.json"),
            r#"{"$id":"https://evorule.dev/domain/rpsm-body.json","type":"object","required":["mass"],"properties":{"mass":{"type":"number"}},"additionalProperties":false}"#,
        )
        .unwrap();
        let store = RuleStore::open(dir.join("db.sqlite").to_str().unwrap()).unwrap();
        (store, dir)
    }

    fn knowledge_dataset() -> RuleDataset {
        RuleDataset {
            dataset_id: "ds-rpsm-assets".into(),
            name: "RPSM 数据资产".into(),
            description: None,
            dataset_kind: crate::model::DatasetKind::Knowledge,
            domain: vec!["rpsm".into()],
            tags: vec![],
            tenant_id: "org-evorule".into(),
            visibility: Visibility::Private,
            lifecycle: Lifecycle::default(),
            versioning: Default::default(),
            law_ref: None,
            version_selection: None,
            data_dependencies: None,
            meta: crate::model::Meta {
                created_at: "t".into(),
                created_by: "u".into(),
                updated_at: None,
                updated_by: None,
            },
        }
    }

    fn knowledge_entry() -> crate::model::knowledge::KnowledgeEntry {
        crate::model::knowledge::KnowledgeEntry {
            entry_id: "body-001".into(),
            dataset_id: "ds-rpsm-assets".into(),
            version: 1,
            status: Some(LifecycleStatus::Draft),
            provenance: Provenance {
                source: "RPSM 实验记录".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "rpsm".into(),
            tags: vec![],
            payload: serde_json::json!({ "mass": 1.5 }),
            schema_ref: "https://evorule.dev/domain/rpsm-body.json".into(),
            governance: None,
        }
    }

    #[test]
    fn test_knowledge_entry_lifecycle_chain() {
        let (store, dir) = file_store_with_body_schema();
        store.create_dataset(&knowledge_dataset()).unwrap();
        let e = knowledge_entry();
        store.add_knowledge_entry(&e).unwrap();
        // Draft → Candidate → Active（5 态状态机同 RuleEntry 口径）
        store
            .transition_knowledge_entry_status(
                "ds-rpsm-assets", "body-001", LifecycleStatus::Candidate, "eng", "t", "评审通过",
            )
            .unwrap();
        store
            .transition_knowledge_entry_status(
                "ds-rpsm-assets", "body-001", LifecycleStatus::Active, "eng", "t2", "生效",
            )
            .unwrap();
        let got = store
            .get_latest_knowledge_entry("ds-rpsm-assets", "body-001")
            .unwrap()
            .unwrap();
        assert_eq!(got.status, Some(LifecycleStatus::Active));
        // 非法迁移：Active → Draft 拒绝
        let err = store
            .transition_knowledge_entry_status(
                "ds-rpsm-assets", "body-001", LifecycleStatus::Draft, "eng", "t3", "回退",
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
        // Active = frozen：草稿编辑拒绝（快照模式）
        let mut frozen = got.clone();
        frozen.payload = serde_json::json!({ "mass": 9.9 });
        let err = store.update_draft_knowledge_entry(&frozen).unwrap_err();
        assert!(matches!(err, StoreError::EntryFrozen { .. }));
        // 状态迁移历史留痕（只追加）
        let hist = store
            .get_knowledge_entry_state_history("ds-rpsm-assets", "body-001")
            .unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].from, "Draft");
        assert_eq!(hist[0].to, "Candidate");
        assert_eq!(hist[1].to, "Active");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_knowledge_entry_rejects_bad_payload() {
        let (store, dir) = file_store_with_body_schema();
        store.create_dataset(&knowledge_dataset()).unwrap();
        // 坏 payload：mass 为字符串，不过领域 schema → 显式拒绝
        let mut bad = knowledge_entry();
        bad.payload = serde_json::json!({ "mass": "heavy" });
        let err = store.add_knowledge_entry(&bad).unwrap_err();
        assert!(matches!(
            err,
            StoreError::Bundle(BundleError::PayloadSchemaViolation { .. })
        ));
        // 未知字段（additionalProperties=false）→ 拒绝
        let mut extra = knowledge_entry();
        extra.payload = serde_json::json!({ "mass": 1.5, "bogus": 1 });
        let err = store.add_knowledge_entry(&extra).unwrap_err();
        assert!(matches!(
            err,
            StoreError::Bundle(BundleError::PayloadSchemaViolation { .. })
        ));
        // resolver 未命中（schema_ref 指向未注册领域）→ 拒绝，不静默放行
        let mut unknown = knowledge_entry();
        unknown.schema_ref = "no-such-schema".into();
        let err = store.add_knowledge_entry(&unknown).unwrap_err();
        assert!(matches!(
            err,
            StoreError::Bundle(BundleError::SchemaNotResolved { .. })
        ));
        // schema_ref 缺失 → store 层兜底拒绝
        let mut missing = knowledge_entry();
        missing.schema_ref = "  ".into();
        let err = store.add_knowledge_entry(&missing).unwrap_err();
        assert!(matches!(err, StoreError::KnowledgeMissingSchemaRef { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_knowledge_entry_kind_mismatch() {
        let store = RuleStore::in_memory().unwrap();
        // 数据条目进 rule_set 数据集 → 拒绝
        store.create_dataset(&tax_dataset()).unwrap();
        let mut e = knowledge_entry();
        e.dataset_id = "ds-tax-2024".into();
        let err = store.add_knowledge_entry(&e).unwrap_err();
        assert!(matches!(err, StoreError::DatasetKindMismatch { .. }));
        // 规则条目进 knowledge 数据集 → 拒绝
        store.create_dataset(&knowledge_dataset()).unwrap();
        let mut r = draft_entry();
        r.dataset_id = "ds-rpsm-assets".into();
        let err = store.add_entry(&r).unwrap_err();
        assert!(matches!(err, StoreError::DatasetKindMismatch { .. }));
    }

    #[test]
    fn test_knowledge_entry_version_history() {
        let (store, dir) = file_store_with_body_schema();
        store.create_dataset(&knowledge_dataset()).unwrap();
        let v1 = knowledge_entry();
        store.add_knowledge_entry(&v1).unwrap();
        // v2：同 entry_id 新版本（快照模式，内容变更不改历史）
        let mut v2 = knowledge_entry();
        v2.version = 2;
        v2.payload = serde_json::json!({ "mass": 2.5 });
        store.add_knowledge_entry(&v2).unwrap();
        // 同版本重复添加 → 唯一性拒绝
        let err = store.add_knowledge_entry(&v2).unwrap_err();
        assert!(matches!(err, StoreError::EntryExists { .. }));
        // 版本历史升序完整
        let all = store
            .list_knowledge_entry_versions("ds-rpsm-assets", "body-001")
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].version, 1);
        assert_eq!(all[1].version, 2);
        assert_eq!(all[1].payload, serde_json::json!({ "mass": 2.5 }));
        // 最新版本
        let latest = store
            .get_latest_knowledge_entry("ds-rpsm-assets", "body-001")
            .unwrap()
            .unwrap();
        assert_eq!(latest.version, 2);
        // 数据集级列表（None = 各条目取最新）
        let listed = store.list_knowledge_entries("ds-rpsm-assets", None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].version, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_knowledge_entry_llm_draft_boundary() {
        let (store, dir) = file_store_with_body_schema();
        store.create_dataset(&knowledge_dataset()).unwrap();
        // LLM 产出直接进 Active → 拒绝（37 号强约束同口径）
        let mut e = knowledge_entry();
        e.status = Some(LifecycleStatus::Active);
        e.governance = Some(crate::model::Governance {
            llm_generated: Some(crate::model::LlmGenerated {
                flag: true,
                model: Some("deepseek-v4".into()),
                op: Some("draft_rule".into()),
                timestamp: None,
            }),
            ..Default::default()
        });
        let err = store.add_knowledge_entry(&e).unwrap_err();
        assert!(matches!(
            err,
            StoreError::Validation(ValidationError::LlmGeneratedNotDraft { .. })
        ));
        // LLM 产出停留 Draft → 放行
        let mut ok = knowledge_entry();
        ok.governance = e.governance.clone();
        store.add_knowledge_entry(&ok).unwrap();
        // LLM 产出 Draft → Candidate 也拒绝（只能停留 Draft）
        let err = store
            .transition_knowledge_entry_status(
                "ds-rpsm-assets", "body-001", LifecycleStatus::Candidate, "llm", "t", "越权推进",
            )
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::Validation(ValidationError::LlmGeneratedNotDraft { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Q12 交付边界收口 A/B（2026-08-30）----

    #[test]
    fn test_knowledge_fts_search_and_sync() {
        let (store, dir) = file_store_with_body_schema();
        store.create_dataset(&knowledge_dataset()).unwrap();
        // 检索文本放 provenance.source（rpsm-body schema 强校验 payload 只认 mass 字段）
        let mut e = knowledge_entry();
        e.provenance.source = "纳税人年度申报操作指引".into();
        store.add_knowledge_entry(&e).unwrap();
        // 中文 trigram 命中
        let hits = store
            .search_knowledge_entries("org-evorule", None, None, Some("年度申报"), &[], None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry_id, "body-001");
        // ASCII 大小写不敏感命中
        let mut e2 = knowledge_entry();
        e2.entry_id = "body-002".into();
        e2.provenance.source = "Salary Ledger Record".into();
        store.add_knowledge_entry(&e2).unwrap();
        let hits = store
            .search_knowledge_entries("org-evorule", None, None, Some("SALARY"), &[], None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry_id, "body-002");
        // q <3 字符 → contains 兜底路径（trigram 无法索引短串），口径一致
        let hits = store
            .search_knowledge_entries("org-evorule", None, None, Some("年"), &[], None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry_id, "body-001");
        // Draft 原地更新（UPDATE 触发器同步 FTS）：新词命中、旧词不再命中
        let mut draft = store
            .get_latest_knowledge_entry("ds-rpsm-assets", "body-001")
            .unwrap()
            .unwrap();
        draft.provenance.source = "增值税留抵退税指引".into();
        store.update_draft_knowledge_entry(&draft).unwrap();
        let hits = store
            .search_knowledge_entries("org-evorule", None, None, Some("增值税"), &[], None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        let hits = store
            .search_knowledge_entries("org-evorule", None, None, Some("年度申报"), &[], None)
            .unwrap();
        assert!(hits.is_empty(), "旧词不应再命中: {hits:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_knowledge_fts_hits_latest_version_only() {
        let (store, dir) = file_store_with_body_schema();
        store.create_dataset(&knowledge_dataset()).unwrap();
        let mut v1 = knowledge_entry();
        v1.provenance.source = "旧版含磁悬浮参数记录".into();
        store.add_knowledge_entry(&v1).unwrap();
        let mut v2 = knowledge_entry();
        v2.version = 2;
        v2.provenance.source = "新版仅含高铁参数记录".into();
        store.add_knowledge_entry(&v2).unwrap();
        // 旧版本含 q、最新版不含 → 不命中（每条目仅最新版参与检索）
        let hits = store
            .search_knowledge_entries("org-evorule", None, None, Some("磁悬浮"), &[], None)
            .unwrap();
        assert!(hits.is_empty(), "旧版本命中不应泄漏: {hits:?}");
        let hits = store
            .search_knowledge_entries("org-evorule", None, None, Some("高铁参数"), &[], None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].version, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_knowledge_fts_backfill_on_existing_db() {
        // 模拟存量库：裸 rusqlite 建 datasets + knowledge_entries（无 FTS 表），再 RuleStore::open
        let dir = std::env::temp_dir().join(format!("evorule-fts-backfill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("db.sqlite3");
        let raw = rusqlite::Connection::open(&db_path).unwrap();
        raw.execute_batch(
            "CREATE TABLE datasets (
                dataset_id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, name TEXT NOT NULL,
                description TEXT, domain TEXT NOT NULL DEFAULT '[]', tags TEXT NOT NULL DEFAULT '[]',
                visibility TEXT NOT NULL DEFAULT 'private', lifecycle TEXT NOT NULL,
                versioning TEXT NOT NULL, law_ref TEXT, version_selection TEXT,
                data_dependencies TEXT, meta TEXT NOT NULL);
            CREATE TABLE knowledge_entries (
                dataset_id TEXT NOT NULL, entry_id TEXT NOT NULL, version INTEGER NOT NULL,
                status TEXT, provenance TEXT NOT NULL, domain TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]', payload TEXT NOT NULL, schema_ref TEXT NOT NULL,
                governance TEXT, content_hash TEXT NOT NULL,
                PRIMARY KEY (dataset_id, entry_id, version));",
        )
        .unwrap();
        let ds = knowledge_dataset();
        raw.execute(
            "INSERT INTO datasets VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                ds.dataset_id,
                ds.tenant_id,
                ds.name,
                ds.description,
                serde_json::to_string(&ds.domain).unwrap(),
                serde_json::to_string(&ds.tags).unwrap(),
                "\"private\"",
                serde_json::to_string(&ds.lifecycle).unwrap(),
                serde_json::to_string(&ds.versioning).unwrap(),
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                serde_json::to_string(&ds.meta).unwrap(),
            ],
        )
        .unwrap();
        raw.execute(
            "INSERT INTO knowledge_entries VALUES ('ds-rpsm-assets', 'old-001', 1, '\"Draft\"', \
             '{\"source\":\"旧库实验记录\"}', 'rpsm', '[]', \
             '{\"note\":\"存量数据回填检索样本\"}', 'https://evorule.dev/domain/rpsm-body.json', NULL, 'hash')",
            [],
        )
        .unwrap();
        drop(raw);
        // open → init_schema 建 FTS + 幂等回填 → 检索命中存量行
        let store = RuleStore::open(db_path.to_str().unwrap()).unwrap();
        let hits = store
            .search_knowledge_entries(&ds.tenant_id, None, None, Some("回填检索"), &[], None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry_id, "old-001");
        // 再开一次（幂等：回填不重插、不报错）
        drop(store);
        let store = RuleStore::open(db_path.to_str().unwrap()).unwrap();
        let hits = store
            .search_knowledge_entries(&ds.tenant_id, None, None, Some("回填检索"), &[], None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_datasets_browsable_merges_public_published() {
        let store = RuleStore::in_memory().unwrap();
        store.create_dataset(&tax_dataset()).unwrap();
        // 他租户 Private → 不混入
        let mut other_priv = tax_dataset();
        other_priv.dataset_id = "ds-other-private".into();
        other_priv.tenant_id = "org-other".into();
        store.create_dataset(&other_priv).unwrap();
        // 他租户 Public 未发布 → 不混入（与 V1 详情双条件一致）
        let mut other_pub = tax_dataset();
        other_pub.dataset_id = "ds-other-public".into();
        other_pub.tenant_id = "org-other".into();
        other_pub.visibility = crate::model::dataset::Visibility::Public;
        store.create_dataset(&other_pub).unwrap();
        // 推进到 Published → 混入
        store
            .transition_dataset_status("ds-other-public", LifecycleStatus::Candidate, "o", "t", "提交")
            .unwrap();
        store
            .transition_dataset_status("ds-other-public", LifecycleStatus::Active, "o", "t2", "激活")
            .unwrap();
        store
            .publish_dataset("ds-other-public", "o", "t3", "inst-001")
            .unwrap();
        let browsable = store.list_datasets_browsable("org-evorule").unwrap();
        let ids: Vec<&str> = browsable.iter().map(|d| d.dataset_id.as_str()).collect();
        assert!(ids.contains(&"ds-tax-2024"), "{ids:?}");
        assert!(ids.contains(&"ds-other-public"), "{ids:?}");
        assert!(!ids.contains(&"ds-other-private"), "{ids:?}");
        // 内部口径不变：list_datasets 仍仅本租户（检索/内部迭代不扩散）
        let strict = store.list_datasets("org-evorule").unwrap();
        assert_eq!(strict.len(), 1);
        assert_eq!(strict[0].dataset_id, "ds-tax-2024");
    }
}
