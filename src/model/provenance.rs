//! Provenance 溯源（31 号 §5）
//!
//! 溯源直接映射权限链 cause 与审计链回放（28 号 §3.3）。
//! `effective_from` 是版本选择 `auto_by_effective_date` 的依据（决策点③）。
//! 法规更新 → 新条目版本 + 新 effective_from，旧版本保留历史。

use serde::{Deserialize, Serialize};

/// 溯源信息
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// 法规/标准/通用模式名称（必填）
    pub source: String,
    /// 条款定位（可选，通用模式可省）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clause: Option<String>,
    /// 回查锚点：法规原文库 ID（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    /// 生效起始（版本选择依据；日期字符串 YYYY-MM-DD）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
    /// 失效日（法规废止/替换时填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<String>,
    /// 最近核验日期（数据集的"活"维护标记）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    /// 核验人（审计）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
}

impl Provenance {
    /// 是否带生效期（可用于版本选择判断）
    pub fn has_effective_date(&self) -> bool {
        self.effective_from.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provenance_serde_roundtrip() {
        let p = Provenance {
            source: "《企业所得税法》".into(),
            clause: Some("第 X 条第 Y 款".into()),
            document_id: Some("gov-tax-2023-001".into()),
            effective_from: Some("2024-01-01".into()),
            effective_to: None,
            last_verified: Some("2026-08-01".into()),
            verified_by: Some("auditor-01".into()),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Provenance = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
