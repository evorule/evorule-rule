//! 操作级审计记录（37 号 §8）
//!
//! "LLM 每步可审计"是平台核心卖点（00-README 核心结论 1）：数据库记录每个命名操作
//! 请求的 `request_id / operation / model / 耗时 / 产出条目`，供审计查询。
//! 落库由 `RuleStore::record_llm_audit` 承担，本类型是持久化单元（31 号 §8 数据模型层）。
//! 查询/统计类型（`LlmAuditFilter` / `LlmAuditStats`）为对外展示接口（37 号 §8 对外层）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 审计查询过滤条件（对外展示接口入参）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmAuditFilter {
    /// 按命名操作过滤（draft_rule / gen_tests / explain_rule；None = 全部）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// 按状态过滤（completed / failed；None = 全部）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// 返回上限（默认 100）
    #[serde(default = "default_audit_limit")]
    pub limit: usize,
}

impl Default for LlmAuditFilter {
    fn default() -> Self {
        Self {
            operation: None,
            status: None,
            limit: default_audit_limit(),
        }
    }
}

fn default_audit_limit() -> usize {
    100
}

/// 审计统计摘要（对外展示接口：聚合报表）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmAuditStats {
    /// 审计总条数
    pub total: u64,
    /// 成功（completed）条数
    pub completed: u64,
    /// 失败（failed）条数
    pub failed: u64,
    /// 平均耗时（毫秒，取整）
    pub avg_duration_ms: u64,
    /// 按操作聚合
    pub by_operation: BTreeMap<String, OperationStat>,
}

/// 单个操作维度的统计
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationStat {
    /// 该 op 调用总次数
    pub count: u64,
    /// 成功次数
    pub completed: u64,
    /// 失败次数
    pub failed: u64,
    /// 平均耗时（毫秒）
    pub avg_duration_ms: u64,
}

/// LLM 命名操作审计记录
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmOpAudit {
    /// 幂等 / 审计主键（调用方生成，`llm_client::make_request_id`）
    pub request_id: String,
    /// 命名操作：draft_rule / gen_tests / explain_rule（37 号 §3）
    pub operation: String,
    /// 模型标识（可插拔，随请求传）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 调用结果：completed | failed
    pub status: String,
    /// 耗时（毫秒）
    pub duration_ms: u64,
    /// 产出条目引用（如 dataset/entry 的 entry_id；可选，便于审计溯源到条目）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    /// failed 时的错误信息
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 调用时间（ISO-8601 UTC）
    pub created_at: String,
}
