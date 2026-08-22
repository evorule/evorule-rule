//! PostgreSQL 生产后端骨架（45 号批次1 §2 · 生产级后续，如实标注）
//!
//! 本模块仅在 `--features postgres` 下编译（见 Cargo.toml `postgres` feature / `dep:sqlx`）。
//! **当前 SQLite 仍是 MVP 活跃引擎**（`RuleStore`），本骨架是生产后端的接入位：
//! - 连接池（`PgPool`，`DATABASE_URL` 注入，兼容 PgBouncer 事务级）；
//! - 版本化迁移（`sqlx::migrate!` 嵌入 `migrations/`）；
//! - 连通性自检 `ping`。
//!
//! 清晰的边界：本骨架**未接入任何查询/写入**，纯建池 + 迁移 + ping。
//! 逐表迁移查询层（P4 方言改写 + P6 事务）由后续批次在 `PgStore` 上展开，本文件不伪称完成。

use sqlx::migrate::MigrateError;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::migrate::Migrator;
use thiserror::Error;

/// 骨架阶段运行时从相对 crate 根 `migrations/` 目录加载迁移
/// （避免为骨架引入 `macros` proc-macro；生产接入查询层后可切换 `sqlx::migrate!` 嵌入宏）。

/// PostgreSQL 存储层错误（骨架阶段仅派生 sqlx 错误）
#[derive(Debug, Error)]
pub enum PgError {
    #[error("PostgreSQL 错误: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("数据库迁移错误: {0}")]
    Migrate(#[from] MigrateError),

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
}