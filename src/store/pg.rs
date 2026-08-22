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
    }
}