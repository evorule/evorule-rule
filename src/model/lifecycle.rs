//! 生命周期 5 态（既定设计决策，MVP 仅落地状态枚举 + state_history 审计结构）
//!
//! ```text
//! Draft → Candidate → Active → Published → Rejected
//! ```
//! - Active = 组织内可用；Published = 对外可见可拉取；两者独立。
//! - Published 需独立发布审批（强约束，cause 留痕，见历史批次）。
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
    /// 发布版本标识（设计文档 §4：`{dataset_id}@{version}`，仅 Published 记录）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_as: Option<String>,
    /// 放行闸类型（机器闸行权通路：`machine`=机器闸放行 / `human`=人工审批；
    /// 缺省 human——历史数据与人工路径行为不变）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    /// 机器闸梯位（`T0` 直通 / `T1` 事后追认；T2 走人工→gate=human）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// 事后追认标记（T1 档：Active 可用+入追认队列）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_review_required: Option<bool>,
}

/// 机器闸 T1 事后追认队列项（机器闸行权通路最小版）：待人工追认的 Active 变更事实
/// （两审计表 UNION 查询产物，`to_state='Active' AND tier='T1' AND post_review_required=1`）
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PostReviewItem {
    pub dataset_id: String,
    pub entry_id: String,
    pub version: u32,
    pub from_state: String,
    pub to_state: String,
    pub at: String,
    pub by: String,
    pub cause: String,
    pub tier: String,
}

/// 数据集级生命周期
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lifecycle {
    pub status: LifecycleStatus,
    /// 审计：每次状态变更（只增不改）
    ///
    /// 契约（设计文档 §3 / console-cloud types.ts）：必填字段 —— 空历史也必须输出 `[]`，
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
