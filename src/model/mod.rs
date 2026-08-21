//! 数据模型模块（31 号 决策点①）
//!
//! 实体关系：
//! ```text
//! RuleDataset（数据集：治理/版本单元） 1 : N RuleEntry（规则条目）
//! ```
//! 数据集级挂载：生命周期（④）、版本语义（③）、数据依赖声明（⑤）、可见性（⑧）。
//! 条目级：rule_body（evorule 原生 JSON，零转译）+ provenance + governance + dependencies。

pub mod auth;
pub mod dataset;
pub mod dependency;
pub mod entry;
pub mod governance;
pub mod lifecycle;
pub mod llm_audit;
pub mod provenance;
pub mod version;

pub use auth::{Action, ApiKey, AuthAudit, Role, Tenant, TokenClaims, User, can};

pub use dataset::{Meta, RuleDataset, Visibility};
pub use dependency::{DataDependencies, InputDecl, IoContract, ServiceDecl, SourceBinding};
pub use entry::RuleEntry;
pub use governance::{Governance, LifecycleTimestamps, LlmGenerated};
pub use lifecycle::{Lifecycle, LifecycleStatus, StateChange};
pub use llm_audit::{LlmAuditFilter, LlmAuditStats, LlmOpAudit, OperationStat};
pub use provenance::Provenance;
pub use version::{
    BumpKind, LawRef, Version, VersionError, VersionSelection, VersionSelectionMode, Versioning,
};
