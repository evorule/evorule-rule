//! evorule-rule —— JSON 规则资产治理系统（数据模型层 · 决策点①）
//!
//! 对应设计文档 31 号（决策点① 数据模型与 schema）与 30 号设计基线。
//! 本 crate 当前实现 **MVP 数据模型层**：
//! - 数据模型：`RuleDataset` / `RuleEntry` / `Provenance` / 依赖声明 / 治理元数据
//! - 存储：SQLite（索引/元数据）+ rule_body 原样 JSON 保存（零转译）
//! - 校验：符号三方一致、不可变性、LLM 边界、状态机基础
//! - 版本解析：auto_by_effective_date / pinned（33 号 §4/§5）
//! - 快照包消费层：导出/导入校验/裁剪（36 号 决策点⑥）
//!
//! 许可：AGPL-3.0-or-later（双授权模式 = AGPL + 商业，合规底线，不可白标掩盖）

pub mod api;
pub mod auth;
pub mod bundle;
pub mod llm_client;
pub mod model;
pub mod resolve;
pub mod store;
pub mod validate;

pub use api::{AppState, ApiError, AuthContext, router};
pub use auth::{
    ACCESS_TOKEN_TTL_SECS, AuthError, AuthService, AuthTokens, DEFAULT_PBKDF2_ITERATIONS,
    REFRESH_TOKEN_TTL_SECS,
};
pub use bundle::{
    BundleEntry, BundleError, BundleExporter, BundleImporter, BundleTests, BundleTrimmer,
    DatasetBundle, DomainSchemaResolver, EntryKind, ImportResult, TestVerdict, ViewRef,
    BUNDLE_SCHEMA_VERSION,
};
pub use model::auth::{Action, AuthAudit, Role, Tenant, TokenClaims, User, can};
pub use model::dataset::{DatasetKind, RuleDataset};
pub use model::entry::RuleEntry;
pub use model::knowledge::KnowledgeEntry;
pub use model::lifecycle::LifecycleStatus;
pub use model::llm_audit::{LlmAuditFilter, LlmAuditStats, LlmOpAudit, OperationStat};
pub use model::provenance::Provenance;
pub use llm_client::{LlmClient, LlmOpResponse, LlmOpRequest, LlmClientError, Operation};
pub use resolve::{EffectiveRange, ResolveError, VersionResolver};
pub use store::{RuleStore, StoreError};
pub use validate::{ValidationError, Validator};
