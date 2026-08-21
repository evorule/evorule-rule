//! 治理补充信息（31 号 §4 governance 块）
//!
//! author/updater、LLM 产出标记（决策点⑦，只到 Draft）、生命周期时间戳。
//! 非 LLM 产出可 omit-when-None（对齐 31 号 §4 字段要点）。

use serde::{Deserialize, Serialize};

/// LLM 产出标记（决策点⑦ 确定性边界）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGenerated {
    pub flag: bool,
    /// 模型标识（可插拔）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 命名操作：draft_rule / gen_tests / explain_rule / patch_rule / query_corpus
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// 产出时间戳（ISO-8601 UTC）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// 生命周期时间戳
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LifecycleTimestamps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drafted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_at: Option<String>,
}

/// 治理补充信息
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Governance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updater: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_generated: Option<LlmGenerated>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_timestamps: Option<LifecycleTimestamps>,
}

impl Governance {
    /// LLM 产出标记是否为真（校验用：flag=true → status 只能是 Draft，37 号强约束）
    pub fn is_llm_generated(&self) -> bool {
        self.llm_generated.as_ref().map(|g| g.flag).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_governance_omit_when_none() {
        // 非 LLM 产出 → llm_generated 应被省略
        let g = Governance::default();
        let json = serde_json::to_string(&g).unwrap();
        assert!(!json.contains("llm_generated"));
    }

    #[test]
    fn test_is_llm_generated() {
        let g = Governance {
            llm_generated: Some(LlmGenerated {
                flag: true,
                model: Some("deepseek-v4".into()),
                op: Some("draft_rule".into()),
                timestamp: Some("2026-07-01T08:10:00Z".into()),
            }),
            ..Governance::default()
        };
        assert!(g.is_llm_generated());
    }
}
