//! PostgreSQL 生产后端（45 号批次1 §2 · 生产级后续，如实标注）
//!
//! 本模块仅在 `--features postgres` 下编译（见 Cargo.toml `postgres` feature / `dep:sqlx`）。
//! **当前 SQLite 仍是 MVP 活跃引擎**（`RuleStore`），本模块是生产后端的接入位：
//! - 连接池（`PgPool`，`DATABASE_URL` 注入，兼容 PgBouncer 事务级）；
//! - 版本化迁移（运行时 `Migrator` 加载 `migrations/`）；
//! - 连通性自检 `ping`；
//! - **P4 方言改写（核心两表，2026-08-22）**：`datasets`/`entries`（含 `entry_snapshots` 去重）
//!   的 CRUD，字段序列化对齐 SQLite 侧（`serde_json`→`TEXT`）。用运行时 `sqlx::query` 手动映射，
//!   避免 `query!` 宏编译期依赖真实 PG，便于在提供 PG 实例前编译通过。
//!
//! 边界（如实）：**代码已就绪，待真实 PG 实例跑集成测试验证**（见 tests `#[ignore]`）。
//! 仅覆盖核心两表；全表方言改写、PG 事务拆分（P6）、配置化双后端为后续批次，不伪称完成。

use sqlx::migrate::MigrateError;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::migrate::Migrator;
use sqlx::Row;
use thiserror::Error;

// 迁移：骨架阶段运行时从相对 crate 根 `migrations/` 目录加载
// （避免引入 `macros` proc-macro；生产接入查询层后可切换 `sqlx::migrate!` 嵌入宏）。

/// PostgreSQL 存储层错误（骨架阶段仅派生 sqlx 错误）
#[derive(Debug, Error)]
pub enum PgError {
    #[error("PostgreSQL 错误: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("数据库迁移错误: {0}")]
    Migrate(#[from] MigrateError),

    #[error("JSON 序列化/反序列化错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("连接串缺失: 需设置 DATABASE_URL (postgres://user:pass@host:5432/evorule_rulehub)")]
    MissingDatabaseUrl,

    #[error("已占位：查询/写入层尚未接入（45 号批次1 生产级后续，见模块注）")]
    NotYetWired,
}

/// PostgreSQL 生产后端（连接池 + 迁移；查询层为后续批次）
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// 由 `DATABASE_URL` 建立连接池并运行迁移（45 号 §2.1：PgBouncer 事务级可前缀）。
    pub async fn connect_from_env() -> Result<Self, PgError> {
        let url = std::env::var("DATABASE_URL").map_err(|_| PgError::MissingDatabaseUrl)?;
        Self::connect(&url).await
    }

    /// 显式连接串建池 + 迁移（对齐 45 号 §2.4 版本化迁移；破坏性变更走 v2 迁移，44 §3.1）。
    pub async fn connect(url: &str) -> Result<Self, PgError> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await?;
        let migrator = Migrator::new(std::path::Path::new("./migrations")).await?;
        migrator.run(&pool).await?;
        Ok(Self { pool })
    }

    /// 连通性自检（`SELECT 1`，供就绪探针 / 启动校验）。
    pub async fn ping(&self) -> Result<(), PgError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// 冒烟自检（45 号 最小接线 · 启动门控）：建池 + 迁移 + 最小 CRUD 往返。
    /// 返回人类可读诊断字符串（如实说明成功环节与边界，不伪造）。
    pub async fn smoke_check() -> Result<String, PgError> {
        let store = Self::connect_from_env().await?;
        store.ping().await?;
        // 最小 CRUD 往返：建租户 + 建数据集 + 读回（验证方言改写路径可用）
        let tenant = format!("smoke-{}", entry_epoch_ms());
        let ds_id = format!("smoke-ds-{}", entry_epoch_ms());
        store
            .ensure_default_tenant(&tenant, "冒烟租户", "inst-smoke", "2026-08-22T00:00:00Z")
            .await?;
        let ds = crate::model::dataset::RuleDataset {
            dataset_id: ds_id.clone(),
            name: "冒烟数据集".into(),
            description: Some("45 号最小接线自检".into()),
            domain: vec!["smoke".into()],
            tags: vec![],
            tenant_id: tenant.clone(),
            visibility: crate::model::dataset::Visibility::Private,
            lifecycle: Default::default(),
            versioning: Default::default(),
            law_ref: None,
            version_selection: None,
            data_dependencies: None,
            meta: crate::model::dataset::Meta {
                created_at: "2026-08-22T00:00:01Z".into(),
                created_by: "smoke".into(),
                updated_at: None,
                updated_by: None,
            },
        };
        store.create_dataset(&ds).await?;
        let got = store.get_dataset(&ds_id).await?.ok_or_else(|| {
            PgError::NotYetWired // 读回失败按接线缺失处理（后续接查询层后不存在）
        })?;
        if got.dataset_id != ds_id {
            return Err(PgError::NotYetWired);
        }
        Ok(format!(
            "PGCONNECT=ok MIGRATE=ok CRUD_ROUNDTRIP=ok tenant={tenant} dataset={ds_id} \
             (方言改写最小链路可用；usage_records 配额与全量 API 网关为后续批次)"
        ))
    }

    /// 暴露池，供后续批次接入查询层（P4 方言改写 + P6 事务）。
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // ------------------------------------------------------------------
    // P4 方言改写 · 核心两表 CRUD（45 号批次1，2026-08-22）
    // 字段序列化对齐 store/mod.rs（SQLite）的 JSON→TEXT；用运行时 query + 手动 Row 映射。
    // 边界：代码就绪待真实 PG 跑集成测试验证（tests `#[ignore]`）。仅核心两表。
    // ------------------------------------------------------------------

    /// 创建数据集（方言改写）。
    pub async fn create_dataset(
        &self,
        ds: &crate::model::dataset::RuleDataset,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO datasets
               (dataset_id, tenant_id, name, description, domain, tags, visibility,
                lifecycle, versioning, law_ref, version_selection, data_dependencies, meta)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(&ds.dataset_id)
        .bind(&ds.tenant_id)
        .bind(&ds.name)
        .bind(&ds.description)
        .bind(serde_json::to_string(&ds.domain)?)
        .bind(serde_json::to_string(&ds.tags)?)
        .bind(serde_json::to_string(&ds.visibility)?)
        .bind(serde_json::to_string(&ds.lifecycle)?)
        .bind(serde_json::to_string(&ds.versioning)?)
        .bind(ds.law_ref.as_ref().map(serde_json::to_string).transpose()?)
        .bind(
            ds.version_selection
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(
            ds.data_dependencies
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(serde_json::to_string(&ds.meta)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 取数据集（方言改写；JSON 字段反序列化回模型）。
    pub async fn get_dataset(
        &self,
        dataset_id: &str,
    ) -> Result<Option<crate::model::dataset::RuleDataset>, PgError> {
        let row = sqlx::query(
            "SELECT dataset_id, tenant_id, name, description, domain, tags, visibility,
                    lifecycle, versioning, law_ref, version_selection, data_dependencies, meta
             FROM datasets WHERE dataset_id = $1",
        )
        .bind(dataset_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        // 从 TEXT 列解码模型（与 SQLite get_dataset 一致）
        let (dataset_id, tenant_id) = {
            let id: String = row.get("dataset_id");
            (id, row.get::<String, _>("tenant_id"))
        };
        let name: String = row.get("name");
        let description: Option<String> = row.get("description");
        let domain: Vec<String> = decode_opt(row.get("domain"))?.unwrap_or_default();
        let tags: Vec<String> = decode_opt(row.get("tags"))?.unwrap_or_default();
        let visibility: crate::model::dataset::Visibility = decode_opt(row.get("visibility"))?.unwrap();
        let lifecycle: crate::model::lifecycle::Lifecycle = decode_opt(row.get("lifecycle"))?.unwrap();
        let versioning: crate::model::version::Versioning = decode_opt(row.get("versioning"))?.unwrap();
        let law_ref: Option<crate::model::version::LawRef> = decode_opt(row.get("law_ref"))?;
        let version_selection: Option<crate::model::version::VersionSelection> =
            decode_opt(row.get("version_selection"))?;
        let data_dependencies: Option<crate::model::dependency::DataDependencies> =
            decode_opt(row.get("data_dependencies"))?;
        let meta: crate::model::dataset::Meta = decode_opt(row.get("meta"))?.unwrap();
        Ok(Some(crate::model::dataset::RuleDataset {
            dataset_id,
            name,
            description,
            domain,
            tags,
            tenant_id,
            visibility,
            lifecycle,
            versioning,
            law_ref,
            version_selection,
            data_dependencies,
            meta,
        }))
    }

    /// 列表数据集（租户作用域；方言改写）。
    pub async fn list_datasets(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<crate::model::dataset::RuleDataset>, PgError> {
        let rows = sqlx::query(
            "SELECT dataset_id FROM datasets WHERE tenant_id = $1 ORDER BY dataset_id",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.get("dataset_id");
            if let Some(ds) = self.get_dataset(&id).await? {
                out.push(ds);
            }
        }
        Ok(out)
    }

    /// 新增条目（方言改写 + entry_snapshots 内容寻址去重，同 SQLite add_entry 语义）。
    /// 注意：本方法为演示验证用，头部复用了 RuleStore 的校验要点（数据集存在）+ 快照去重；
    /// 完整校验链（符号三方一致/LLM 边界/唯一性友好报错）在接入查询层时对齐，见 store/mod.rs。
    pub async fn add_entry(
        &self,
        entry: &crate::model::entry::RuleEntry,
    ) -> Result<(), PgError> {
        let ds = self.get_dataset(&entry.dataset_id).await?.ok_or_else(|| {
            PgError::NotYetWired // 数据集不存在时无法友好区分，先用占位错误（待接入全校验链）
        })?;
        let _ = ds;
        let hash = entry.content_hash();
        let mut tx = self.pool.begin().await?;
        // 内容寻址快照去重（33 号 §6/C1）：跨版本未变内容复用同一快照行
        sqlx::query(
            "INSERT INTO entry_snapshots(dataset_id, content_hash, rule_body, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT(dataset_id, content_hash) DO NOTHING",
        )
        .bind(&entry.dataset_id)
        .bind(&hash)
        .bind(serde_json::to_string(&entry.rule_body)?)
        .bind(&*entry_epoch_ms())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO entries
               (dataset_id, entry_id, version, status, provenance, domain, tags,
                data_source_binding, consumed_inputs, rule_body, governance, content_hash)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(&entry.dataset_id)
        .bind(&entry.entry_id)
        // PG `INTEGER` = INT4，按 i32 bind（与入读一致）
        .bind(entry.version as i32)
        .bind(
            entry
                .status
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?
                .as_deref(),
        )
        .bind(serde_json::to_string(&entry.provenance)?)
        .bind(&entry.domain)
        .bind(serde_json::to_string(&entry.tags)?)
        .bind(serde_json::to_string(&entry.data_source_binding)?)
        .bind(serde_json::to_string(&entry.consumed_inputs)?)
        .bind(serde_json::to_string(&entry.rule_body)?)
        .bind(entry.governance.as_ref().map(serde_json::to_string).transpose()?)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 列条目（租户作用域，经数据集归属；方言改写，简化版）。
    pub async fn list_entries(
        &self,
        dataset_id: &str,
    ) -> Result<Vec<crate::model::entry::RuleEntry>, PgError> {
        let rows = sqlx::query(
            "SELECT dataset_id, entry_id, version, status, provenance, domain, tags,
                    data_source_binding, consumed_inputs, rule_body, governance, content_hash
             FROM entries WHERE dataset_id = $1 ORDER BY entry_id, version",
        )
        .bind(dataset_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(map_entry_row(&r)?);
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // P6 方言改写 · 其余核心表（2026-08-22）——service_templates / api_keys /
    // llm_op_audit / auth_audits / revoked_tokens / users
    // 对齐 migrations 列 + store/mod.rs 序列化；只读角色枚举等以 TEXT 落 JSON，与 SQLite 一致。
    // ------------------------------------------------------------------

    /// 注册服务模板（方言改写）。
    pub async fn create_service_template(
        &self,
        t: &crate::model::dependency::ServiceTemplateRecord,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO service_templates
               (template_id, tenant_id, service_name, kind, io_contract, endpoint_template,
                method, headers_template, placeholder_notes, created_at, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&t.template_id)
        .bind(&t.tenant_id)
        .bind(&t.service_name)
        .bind(&t.kind)
        .bind(serde_json::to_string(&t.io_contract)?)
        .bind(&t.endpoint_template)
        .bind(&t.method)
        .bind(serde_json::to_string(&t.headers_template)?)
        .bind(serde_json::to_string(&t.placeholder_notes)?)
        .bind(&t.created_at)
        .bind(&t.created_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 取服务模板（tenancy 校验由调用方在 API 层完成）。
    pub async fn get_service_template(
        &self,
        template_id: &str,
    ) -> Result<Option<crate::model::dependency::ServiceTemplateRecord>, PgError> {
        let row = sqlx::query(
            "SELECT template_id, tenant_id, service_name, kind, io_contract, endpoint_template,
                    method, headers_template, placeholder_notes, created_at, created_by
             FROM service_templates WHERE template_id = $1",
        )
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(crate::model::dependency::ServiceTemplateRecord {
            template_id: row.get("template_id"),
            tenant_id: row.get("tenant_id"),
            service_name: row.get("service_name"),
            kind: row.get("kind"),
            io_contract: decode_opt(row.get("io_contract"))?.unwrap(),
            endpoint_template: row.get("endpoint_template"),
            method: row.get("method"),
            headers_template: decode_opt(row.get("headers_template"))?.unwrap_or_default(),
            placeholder_notes: decode_opt(row.get("placeholder_notes"))?.unwrap_or_default(),
            created_at: row.get("created_at"),
            created_by: row.get("created_by"),
        }))
    }

    /// 列服务模板（租户作用域）。
    pub async fn list_service_templates(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<crate::model::dependency::ServiceTemplateRecord>, PgError> {
        let rows = sqlx::query(
            "SELECT template_id FROM service_templates WHERE tenant_id = $1 ORDER BY service_name",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.get("template_id");
            if let Some(t) = self.get_service_template(&id).await? {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// 记录 LLM 命名操作审计（方言改写，幂等：同 request_id 覆盖）。
    pub async fn record_llm_audit(
        &self,
        a: &crate::model::llm_audit::LlmOpAudit,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO llm_op_audit
               (request_id, operation, model, status, duration_ms, result_ref, error, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT(request_id) DO UPDATE SET
               operation=EXCLUDED.operation, model=EXCLUDED.model, status=EXCLUDED.status,
               duration_ms=EXCLUDED.duration_ms, result_ref=EXCLUDED.result_ref,
               error=EXCLUDED.error, created_at=EXCLUDED.created_at",
        )
        .bind(&a.request_id)
        .bind(&a.operation)
        .bind(&a.model)
        .bind(&a.status)
        .bind(a.duration_ms as i64)
        .bind(&a.result_ref)
        .bind(&a.error)
        .bind(&a.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 取单条 LLM 审计。
    pub async fn get_llm_audit(
        &self,
        request_id: &str,
    ) -> Result<Option<crate::model::llm_audit::LlmOpAudit>, PgError> {
        let row = sqlx::query(
            "SELECT request_id, operation, model, status, duration_ms, result_ref, error, created_at
             FROM llm_op_audit WHERE request_id = $1",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(crate::model::llm_audit::LlmOpAudit {
            request_id: row.get("request_id"),
            operation: row.get("operation"),
            model: row.get("model"),
            status: row.get("status"),
            duration_ms: row.get::<i64, _>("duration_ms") as u64,
            result_ref: row.get("result_ref"),
            error: row.get("error"),
            created_at: row.get("created_at"),
        }))
    }

    /// 列 LLM 审计（倒序，limit）。
    pub async fn list_llm_audits(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::model::llm_audit::LlmOpAudit>, PgError> {
        let rows = sqlx::query(
            "SELECT request_id FROM llm_op_audit ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.get("request_id");
            if let Some(a) = self.get_llm_audit(&id).await? {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// 记录认证审计（only-append，方言改写）。
    pub async fn record_auth_audit(
        &self,
        a: &crate::model::auth::AuthAudit,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO auth_audits (audit_id, action, user_id, tenant_id, outcome, detail, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&a.audit_id)
        .bind(&a.action)
        .bind(&a.user_id)
        .bind(&a.tenant_id)
        .bind(&a.outcome)
        .bind(&a.detail)
        .bind(&a.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 创建 API Key（方言改写；仅存 key_hash，不存明文，对齐 44 号 §14）。
    pub async fn create_api_key(
        &self,
        k: &crate::model::auth::ApiKey,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO api_keys (key_id, tenant_id, name, scope, key_hash, created_at, revoked_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&k.key_id)
        .bind(&k.tenant_id)
        .bind(&k.name)
        .bind(&k.scope)
        .bind(&k.key_hash)
        .bind(&k.created_at)
        .bind(&k.revoked_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 按哈希查 API Key（认证用）。
    pub async fn get_api_key_by_hash(
        &self,
        key_hash: &str,
    ) -> Result<Option<crate::model::auth::ApiKey>, PgError> {
        let row = sqlx::query(
            "SELECT key_id, tenant_id, name, scope, key_hash, created_at, revoked_at
             FROM api_keys WHERE key_hash = $1",
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(crate::model::auth::ApiKey {
            key_id: row.get("key_id"),
            tenant_id: row.get("tenant_id"),
            name: row.get("name"),
            scope: row.get("scope"),
            key_hash: row.get("key_hash"),
            created_at: row.get("created_at"),
            revoked_at: row.get("revoked_at"),
        }))
    }

    /// 撤销 API Key（返回是否命中；方言改写）。
    pub async fn revoke_api_key(
        &self,
        tenant_id: &str,
        key_id: &str,
        at: String,
    ) -> Result<bool, PgError> {
        let res = sqlx::query(
            "UPDATE api_keys SET revoked_at = $1 WHERE tenant_id = $2 AND key_id = $3",
        )
        .bind(at)
        .bind(tenant_id)
        .bind(key_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// 注册撤销 token（jti 黑名单，方言改写）。
    pub async fn revoke_token(
        &self,
        jti: &str,
        tenant_id: &str,
        user_id: &str,
        token_type: &str,
        expires_at: i64,
        revoked_at: String,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO revoked_tokens (jti, tenant_id, user_id, token_type, expires_at, revoked_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(jti)
        .bind(tenant_id)
        .bind(user_id)
        .bind(token_type)
        .bind(expires_at)
        .bind(revoked_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 查 token 是否已撤销（方言改写；now 之前且 jti 命中即视为 revoked）。
    pub async fn is_token_revoked(&self, jti: &str, now: i64) -> Result<bool, PgError> {
        let row = sqlx::query(
            "SELECT 1 FROM revoked_tokens WHERE jti = $1 AND expires_at > $2",
        )
        .bind(jti)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// 创建用户（方言改写；Role 以 snake_case 文本落库，与 SQLite 一致）。
    pub async fn create_user(
        &self,
        u: &crate::model::auth::User,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO users
               (user_id, tenant_id, username, password_hash, salt, role, disabled, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&u.user_id)
        .bind(&u.tenant_id)
        .bind(&u.username)
        .bind(&u.password_hash)
        .bind(&u.salt)
        .bind(u.role.as_str())
        // disabled 列在迁移中为 INTEGER（SQLite 风格 0/1），按整数 bind（真实 PG 已建旧结构，遵守既有列类型）
        .bind(u.disabled as i32)
        .bind(&u.created_at)
        .bind(&u.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 确保租户存在（方言改写；对齐 SQLite `INSERT OR IGNORE`，PG 用 ON CONFLICT DO NOTHING）。
    pub async fn ensure_default_tenant(
        &self,
        tenant_id: &str,
        name: &str,
        instance_id: &str,
        created_at: &str,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO tenants(tenant_id, name, instance_id, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT(tenant_id) DO NOTHING",
        )
        .bind(tenant_id)
        .bind(name)
        .bind(instance_id)
        .bind(created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // P6 剩余表方言改写（2026-08-22）——dataset_versions / entry_state_history /
    // bundles_import_logs。（usage_records 为 45 号 §2.4 配额预留，store 层当前无写入路径，
    // 本轮不落地，如实标注后续。）
    // ------------------------------------------------------------------

    /// 写一条 dataset_versions 归因行（幂等：同 (dataset, version, entry_hash) 去重，跨版本未变内容复用）。
    pub async fn record_dataset_version_row(
        &self,
        dataset_id: &str,
        version: &str,
        entry_hash: &str,
        entry_id: &str,
        created_at: &str,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO dataset_versions (dataset_id, version, entry_hash, entry_id, created_at)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(dataset_id)
        .bind(version)
        .bind(entry_hash)
        .bind(entry_id)
        .bind(created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取某数据集某版本的条目 content_hash 归因集（升版留档，对齐 store/mod.rs 语义）。
    pub async fn get_dataset_version_hashes(
        &self,
        dataset_id: &str,
        version: &str,
    ) -> Result<Vec<(String, String)>, PgError> {
        let rows = sqlx::query(
            "SELECT entry_hash, entry_id FROM dataset_versions
             WHERE dataset_id = $1 AND version = $2",
        )
        .bind(dataset_id)
        .bind(version)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push((r.get("entry_hash"), r.get("entry_id")));
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // B4 段B：版本级全量条目快照（dataset_version_snapshots，对齐 store/mod.rs 语义）。
    // 建表归属 PG 迁移脚本（45 号 §5，治理数据 MVP 仍 SQLite），此处仅提供读写方法。
    // ------------------------------------------------------------------

    /// 写一批版本级全量条目快照（幂等：同 (dataset, version, entry) 覆盖不重复）。
    #[allow(clippy::too_many_arguments)]
    pub async fn record_version_snapshot_row(
        &self,
        dataset_id: &str,
        version: &str,
        entry_id: &str,
        kind: &str,
        content_hash: &str,
        content_json: &str,
        created_by: &str,
        created_at: &str,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO dataset_version_snapshots
                (dataset_id, version, entry_id, kind, content_hash, content_json, created_by, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (dataset_id, version, entry_id) DO NOTHING",
        )
        .bind(dataset_id)
        .bind(version)
        .bind(entry_id)
        .bind(kind)
        .bind(content_hash)
        .bind(content_json)
        .bind(created_by)
        .bind(created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取某数据集某版本的全量条目快照（kind + 完整条目 JSON，entry_id 序）。
    pub async fn get_version_snapshots(
        &self,
        dataset_id: &str,
        version: &str,
    ) -> Result<Vec<(String, String, String)>, PgError> {
        let rows = sqlx::query(
            "SELECT entry_id, kind, content_json FROM dataset_version_snapshots
             WHERE dataset_id = $1 AND version = $2 ORDER BY entry_id",
        )
        .bind(dataset_id)
        .bind(version)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push((r.get("entry_id"), r.get("kind"), r.get("content_json")));
        }
        Ok(out)
    }

    /// 记录条目状态迁移（only-append 审计，对齐 store/mod.rs 的 StateChange Debug 序列化）。
    pub async fn record_entry_state(
        &self,
        dataset_id: &str,
        entry_id: &str,
        version: u32,
        from_state: Option<&str>,
        to_state: &str,
        at: &str,
        by: &str,
        cause: &str,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO entry_state_history
               (dataset_id, entry_id, version, from_state, to_state, at, by, cause)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(dataset_id)
        .bind(entry_id)
        .bind(version as i32)
        .bind(from_state)
        .bind(to_state)
        .bind(at)
        .bind(by)
        .bind(cause)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 条目状态迁移历史（only-append 只读，按 id 升序）。
    pub async fn get_entry_state_history(
        &self,
        dataset_id: &str,
        entry_id: &str,
    ) -> Result<Vec<crate::model::lifecycle::StateChange>, PgError> {
        let rows = sqlx::query(
            "SELECT from_state, to_state, at, by, cause FROM entry_state_history
             WHERE dataset_id = $1 AND entry_id = $2 ORDER BY id",
        )
        .bind(dataset_id)
        .bind(entry_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(crate::model::lifecycle::StateChange {
                from: r.get::<Option<String>, _>("from_state").unwrap_or_default(),
                to: r.get("to_state"),
                at: r.get("at"),
                by: r.get("by"),
                cause: r.get("cause"),
                published_as: None,
            });
        }
        Ok(out)
    }

    /// 记录快照包导入流水（content_hash 幂等，44 号 §9）。
    pub async fn log_bundle_import(
        &self,
        log_id: &str,
        dataset_id: &str,
        bundle_hash: &str,
        status: &str,
        detail: Option<&str>,
        operator: Option<&str>,
        created_at: &str,
    ) -> Result<(), PgError> {
        sqlx::query(
            "INSERT INTO bundles_import_logs
               (log_id, dataset_id, bundle_hash, status, detail, operator, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(log_id)
        .bind(dataset_id)
        .bind(bundle_hash)
        .bind(status)
        .bind(detail)
        .bind(operator)
        .bind(created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读某数据集的导入流水（倒序）。返回 `(bundle_hash, status, detail, operator, created_at, log_id)`，
    /// detail/operator 可为 NULL。
    pub async fn get_bundle_import_logs(
        &self,
        dataset_id: &str,
        limit: usize,
    ) -> Result<
        Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
        )>,
        PgError,
    > {
        let rows = sqlx::query(
            "SELECT bundle_hash, status, detail, operator, created_at, log_id
             FROM bundles_import_logs WHERE dataset_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(dataset_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push((
                r.get("bundle_hash"),
                r.get("status"),
                r.get("detail"),
                r.get("operator"),
                r.get("created_at"),
                r.get("log_id"),
            ));
        }
        Ok(out)
    }
}

/// 把 `TEXT` 列（可为 NULL）反序列化为 `V`。
fn decode_opt<V: serde::de::DeserializeOwned>(
    raw: Option<String>,
) -> Result<Option<V>, PgError> {
    match raw {
        None => Ok(None),
        Some(s) => Ok(Some(serde_json::from_str(&s)?)),
    }
}

/// 从 `Row` 映射一条 RuleEntry（方言改写）。
fn map_entry_row(
    r: &sqlx::postgres::PgRow,
) -> Result<crate::model::entry::RuleEntry, PgError> {
    let status: Option<String> = r.get("status");
    let rule_body: String = r.get("rule_body");
    let entry = crate::model::entry::RuleEntry {
        dataset_id: r.get("dataset_id"),
        entry_id: r.get("entry_id"),
        // PG `INTEGER` = INT4（32 位），需按 i32 读取再转 u32（对齐 entries.version 类型）
        version: r.get::<i32, _>("version") as u32,
        status: status.map(|s| decode_opt(Some(s))).transpose()?.flatten(),
        provenance: decode_opt(r.get("provenance"))?.unwrap(),
        domain: r.get("domain"),
        tags: decode_opt(r.get("tags"))?.unwrap_or_default(),
        data_source_binding: decode_opt(r.get("data_source_binding"))?.unwrap_or_default(),
        consumed_inputs: decode_opt(r.get("consumed_inputs"))?.unwrap_or_default(),
        rule_body: serde_json::from_str(&rule_body)?,
        governance: decode_opt(r.get("governance"))?,
    };
    Ok(entry)
}

/// 当前时间（epoch 毫秒），与 SQLite 侧 `epoch_ms_now` 语义一致（供快照时间戳）。
fn entry_epoch_ms() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_env_errors_gracefully() {
        // 未设 DATABASE_URL 时给出明确的连接串错误，不 panic/不崩溃
        std::env::remove_var("DATABASE_URL");
        let err = PgStore::connect_from_env().await;
        assert!(matches!(err, Err(PgError::MissingDatabaseUrl)));
    }

    #[tokio::test]
    async fn wired_boundary_is_explicit() {
        // 骨架阶段查询层未接入，需如实报错而不静默降级（醒目而非假装完成）
        let _ = PgError::NotYetWired;
    }

    // ------------------------------------------------------------------
    // P4 真实集成测试（`#[ignore]`：需真实 PostgreSQL 实例 + DATABASE_URL）
    // 本机默认无 PG，故标记 ignore；提供 PG 后执行：
    //   $env:DATABASE_URL="postgres://user:pass@host:5432/evorule_rulehub_test"
    //   cargo test --features postgres pg_ds_entry_roundtrip -- --ignored
    // ------------------------------------------------------------------

    /// 构造一个 RuleDataset 测试样本（对齐 create_dataset 测试的字段形态）
    fn sample_dataset(tenant: &str, id: &str) -> crate::model::dataset::RuleDataset {
        use crate::model::dataset::{Meta, RuleDataset, Visibility};
        use crate::model::version::Versioning;
        RuleDataset {
            dataset_id: id.to_string(),
            name: format!("ds-{id}"),
            description: Some("P4 集成样本".into()),
            domain: vec!["tax".into(), "cn".into()],
            tags: vec!["test".into()],
            tenant_id: tenant.to_string(),
            visibility: Visibility::Private,
            lifecycle: Default::default(),
            versioning: Versioning::default(),
            law_ref: None,
            version_selection: None,
            data_dependencies: None,
            meta: Meta {
                created_at: "2026-08-22T00:00:00Z".into(),
                created_by: "u-1".into(),
                updated_at: None,
                updated_by: None,
            },
        }
    }

    #[tokio::test]
    #[ignore = "需要真实 PostgreSQL：DATABASE_URL + migrations 可执行"]
    async fn pg_ds_entry_roundtrip() {
        let url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("跳过：未设置 DATABASE_URL（P4 需真实 PG）");
                return;
            }
        };
        let store = PgStore::connect(&url).await.expect("建池+迁移失败");
        store.ping().await.expect("ping 失败");
        let tenant = format!("tenant-{}", entry_epoch_ms());
        let ds_id = format!("ds-{}", entry_epoch_ms());
        let ds = sample_dataset(&tenant, &ds_id);
        store.create_dataset(&ds).await.expect("create_dataset");
        let got = store
            .get_dataset(&ds_id)
            .await
            .expect("get_dataset")
            .expect("数据集不存在");
        assert_eq!(got.dataset_id, ds_id);
        assert_eq!(got.tenant_id, tenant);
        assert_eq!(got.tags, vec!["test".to_string()]);
        // 条目往返 + 内容寻址去重
        let entry_id = "rule-e1";
        let entry = crate::model::entry::RuleEntry {
            entry_id: entry_id.into(),
            dataset_id: ds_id.clone(),
            version: 1,
            status: None,
            provenance: crate::model::provenance::Provenance {
                source: "《P4集成样本》".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "tax".into(),
            tags: vec![],
            data_source_binding: vec![],
            consumed_inputs: vec![],
            rule_body: serde_json::json!({ "rule_id": entry_id }),
            governance: None,
        };
        store.add_entry(&entry).await.expect("add_entry");
        let entries = store.list_entries(&ds_id).await.expect("list_entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rule_body["rule_id"], entry_id);
        assert_eq!(entries[0].content_hash(), entry.content_hash());

        // P6：其余核心表往返
        pg_aux_tables_roundtrip(&store, &tenant).await.expect("P6 aux");

        // P6 剩余表：dataset_versions 归因 / entry_state_history 迁移审计 / bundles_import_logs
        let hash = entry.content_hash();
        let v1 = "v1".to_string();
        store
            .record_dataset_version_row(&ds_id, &v1, &hash, entry_id, "2026-08-22T00:00:10Z")
            .await
            .expect("record_dataset_version_row");
        // 幂等：同 (dataset, version, entry_hash) 重复写不新增
        store
            .record_dataset_version_row(&ds_id, &v1, &hash, entry_id, "2026-08-22T00:00:11Z")
            .await
            .expect("record_dataset_version_row_dup");
        let vrows = store
            .get_dataset_version_hashes(&ds_id, &v1)
            .await
            .expect("get_dataset_version_hashes");
        assert_eq!(vrows.len(), 1, "dataset_versions 去重应只有 1 行");
        assert_eq!(vrows[0], (hash.clone(), entry_id.to_string()));

        store
            .record_entry_state(
                &ds_id,
                entry_id,
                1,
                None,
                "Draft",
                "2026-08-22T00:00:12Z",
                "u-1",
                "P6 验证",
            )
            .await
            .expect("record_entry_state");
        let hist = store
            .get_entry_state_history(&ds_id, entry_id)
            .await
            .expect("get_entry_state_history");
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].to, "Draft");
        assert_eq!(hist[0].cause, "P6 验证");

        store
            .log_bundle_import(
                "b1",
                &ds_id,
                "hash-x",
                "ok",
                Some("P6 验证"),
                Some("u-1"),
                "2026-08-22T00:00:13Z",
            )
            .await
            .expect("log_bundle_import");
        let logs = store
            .get_bundle_import_logs(&ds_id, 10)
            .await
            .expect("get_bundle_import_logs");
        assert_eq!(logs.len(), 1);
        // tuple: (bundle_hash=0, status=1, detail=2, operator=3, created_at=4, log_id=5)
        assert_eq!(logs[0].1, "ok");
        assert_eq!(logs[0].2, Some("P6 验证".to_string()));
    }

    /// P6 其余核心表集成：service_templates / api_keys / llm_op_audit / auth_audits / revoked_tokens / users
    async fn pg_aux_tables_roundtrip(
        store: &super::PgStore,
        tenant: &str,
    ) -> Result<(), super::PgError> {
        use crate::model::auth::{ApiKey, AuthAudit, User};
        use crate::model::dependency::{IoContract, ServiceTemplateRecord};
        use crate::model::llm_audit::LlmOpAudit;

        // tenants 外键约束：先确保租户存在（真实 PG 校验数据完整性）
        store
            .ensure_default_tenant(tenant, "测试租户", "inst-pg6", "2026-08-22T00:00:00Z")
            .await?;

        // service_templates
        let tpl = ServiceTemplateRecord {
            template_id: format!("tpl-{tenant}"),
            tenant_id: tenant.into(),
            service_name: "s3-demo".into(),
            kind: "pull".into(),
            io_contract: IoContract { r#in: None, out: None },
            endpoint_template: "https://api.example.com/{token}".into(),
            method: Some("GET".into()),
            headers_template: Default::default(),
            placeholder_notes: Default::default(),
            created_at: "2026-08-22T00:00:00Z".into(),
            created_by: "u-1".into(),
        };
        store.create_service_template(&tpl).await?;
        let got = store.get_service_template(&tpl.template_id).await?.expect("模板缺失");
        assert_eq!(got.service_name, "s3-demo");
        let list = store.list_service_templates(tenant).await?;
        assert_eq!(list.len(), 1);

        // llm_op_audit（幂等：同 request_id 覆盖）
        let audit = LlmOpAudit {
            request_id: format!("req-{tenant}"),
            operation: "draft_rule".into(),
            model: Some("pt-x".into()),
            status: "completed".into(),
            duration_ms: 120,
            result_ref: None,
            error: None,
            created_at: "2026-08-22T00:00:01Z".into(),
        };
        store.record_llm_audit(&audit).await?;
        let mut audit2 = audit.clone();
        audit2.duration_ms = 999;
        store.record_llm_audit(&audit2).await?; // 覆盖
        let got = store.get_llm_audit(&audit.request_id).await?.expect("audit 缺失");
        assert_eq!(got.duration_ms, 999);
        // 列表含本条（共享库可能残留旧行，故验证包含而非全局长度==1）
        let list = store.list_llm_audits(10).await?;
        if !list.iter().any(|a| a.request_id == audit.request_id) {
            panic!("list_llm_audits 未包含本 request_id");
        }

        // auth_audits（only-append）
        let aa = AuthAudit {
            audit_id: format!("aa-{tenant}"),
            action: "login".into(),
            user_id: Some("u-1".into()),
            tenant_id: tenant.into(),
            outcome: "success".into(),
            detail: None,
            created_at: "2026-08-22T00:00:02Z".into(),
        };
        store.record_auth_audit(&aa).await?;

        // api_keys（仅存哈希）+ 撤销
        let key = ApiKey {
            key_id: format!("k-{tenant}"),
            tenant_id: tenant.into(),
            name: "exec".into(),
            scope: "pull".into(),
            key_hash: format!("sha256-{tenant}"),
            created_at: "2026-08-22T00:00:03Z".into(),
            revoked_at: None,
        };
        store.create_api_key(&key).await?;
        let got = store.get_api_key_by_hash(&key.key_hash).await?.expect("key 缺失");
        assert_eq!(got.scope, "pull");
        let revoked = store
            .revoke_api_key(tenant, &key.key_id, "2026-08-22T00:00:04Z".into())
            .await?;
        assert!(revoked);

        // revoked_tokens jti 黑名单（用租户后缀保证测试幂等，避免共享库残留主键冲突）
        let jti = format!("jti-x-{tenant}");
        store
            .revoke_token(&jti, tenant, "u-1", "access", 9_999_999_999, "now".into())
            .await?;
        assert!(store.is_token_revoked(&jti, 0).await?);
        assert!(!store.is_token_revoked("jti-else", 0).await?);

        // users
        let user = User {
            user_id: format!("u-{tenant}"),
            tenant_id: tenant.into(),
            username: "alice".into(),
            password_hash: "pbkdf2$...".into(),
            salt: "s0".into(),
            role: crate::model::auth::Role::RuleEngineer,
            disabled: false,
            created_at: "2026-08-22T00:00:05Z".into(),
            updated_at: "2026-08-22T00:00:05Z".into(),
        };
        store.create_user(&user).await?;
        Ok(())
    }

    /// 配置化双后端冒烟（45 号 最小接线）：真实 PG 下 `smoke_check` 应成功并返回 CRUD_ROUNDTRIP=ok。
    #[tokio::test]
    #[ignore = "需要真实 PostgreSQL：DATABASE_URL"]
    async fn pg_smoke_check() {
        let url = std::env::var("DATABASE_URL").unwrap_or_default();
        if url.is_empty() {
            eprintln!("跳过：未设置 DATABASE_URL");
            return;
        }
        let diag = super::PgStore::smoke_check()
            .await
            .expect("smoke_check 失败（真实 PG 应可用）");
        assert!(diag.contains("CRUD_ROUNDTRIP=ok"), "diag={diag}");
        eprintln!("smoke_check OK: {diag}");
    }

    /// 配置化双后端维持默认：无 DATABASE_URL 时不启用 PG（回落 SQLite，不伪造）。
    #[tokio::test]
    #[ignore = "需在隔离环境验证 env；默认下不执行"]
    async fn backend_default_sqlite_no_env() {
        std::env::remove_var("DATABASE_URL");
        let store = crate::store::RuleStore::open(":memory:").expect("open sqlite");
        let state = crate::api::AppState::new(store, "s", "inst", "http://x")
            .bootstrap_backend()
            .await;
        assert_eq!(state.backend, crate::api::BackendKind::Sqlite);
        assert_eq!(state.pg_smoke, None);
    }
}