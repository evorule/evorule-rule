//! 生命周期 5 态（决策点④，MVP 仅落地状态枚举 + state_history 审计结构）
//!
//! ```text
//! Draft → Candidate → Active → Published → Rejected
//! ```
//! - Active = 组织内可用；Published = 对外可见可拉取；两者独立。
//! - Published 需独立发布审批（强约束，cause 留痕，见 34 号）。
//! - `state_history` 只增不改（审计即记忆，对齐 05 / 15-24 权限链）。

use serde::{Deserialize, Serialize};

/// 生命周期状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum LifecycleStatus {
    Draft,
    Candidate,
    Active,
    Published,
    Rejected,
}

/// 状态变更审计记录（只增不改）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChange {
    pub from: String,
    pub to: String,
    /// 变更时间（ISO-8601，UTC）
    pub at: String,
    /// 操作者（对齐权限链 caller/author 语义）
    pub by: String,
    /// 变更原因（审批通过/驳回/…，审计 cause）
    pub cause: String,
    /// 发布版本标识（34 号 §4：`{dataset_id}@{version}`，仅 Published 记录）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_as: Option<String>,
}

/// 数据集级生命周期
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lifecycle {
    pub status: LifecycleStatus,
    /// 审计：每次状态变更（只增不改）
    ///
    /// 契约（31 号 §3 / console-cloud types.ts）：必填字段 —— 空历史也必须输出 `[]`，
    /// 不能省略，否则前端 `state_history.length` 崩溃（Phase 2 治理接线实测缺陷）。
    #[serde(default)]
    pub state_history: Vec<StateChange>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            status: LifecycleStatus::Draft,
            state_history: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lifecycle_status_serde() {
        // PascalCase 双向序列化（对齐 schema 写法：Draft/Active/Published）
        let s = serde_json::to_string(&LifecycleStatus::Published).unwrap();
        assert_eq!(s, "\"Published\"");
        let back: LifecycleStatus = serde_json::from_str("\"Active\"").unwrap();
        assert_eq!(back, LifecycleStatus::Active);
    }
}
