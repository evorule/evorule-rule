//! 操作级审计记录（37 号 §8）
//!
//! "LLM 每步可审计"是平台核心卖点（00-README 核心结论 1）：数据库记录每个命名操作
//! 请求的 `request_id / operation / model / 耗时 / 产出条目`，供审计查询。
//! 落库由 `RuleStore::record_llm_audit` 承担，本类型是持久化单元（31 号 §8 数据模型层）。

use serde::{Deserialize, Serialize};

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
