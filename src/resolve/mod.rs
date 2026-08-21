//! 版本解析算法（决策点③ · 33 号 §4/§5）：auto_by_effective_date | pinned
//!
//! - `auto_by_effective_date`：按事件业务日期自动选择当时生效的最高主版本（含其最新 Patch）；
//! - `pinned`：显式锁定版本，`pinned_include_patch` 决定 Patch 是否跟进（默认 true）；
//! - **历史回放确定性**：给定事件日期 T 解析出的版本固定，不随未来版本变化（审计可重算）；
//! - **边界（不静默降级）**：T 无生效版本 → 显式报错，不静默用旧版本。
//!
//! 输入说明（33 号 §5）：本模块是纯函数，每主版本的生效范围 `EffectiveRange` 由调用方提供
//! —— 来源：45 号 `dataset_versions` 快照表（effective_from/to 索引）或 36 号快照包内嵌配置。

use std::collections::BTreeMap;

use thiserror::Error;

use crate::model::version::{Version, VersionError, VersionSelection, VersionSelectionMode};

/// 版本解析错误
#[derive(Debug, Error, PartialEq)]
pub enum ResolveError {
    #[error("pinned 模式缺少 pinned_version（必须显式锁定版本）")]
    MissingPinnedVersion,

    #[error("pinned 版本 `{version}` 不在版本链 {chain:?} 中")]
    PinnedNotInChain { version: String, chain: Vec<String> },

    #[error("主版本 v{main} 有生效范围但不在版本链 {chain:?} 中（数据不一致）")]
    MainMissingInChain { main: u32, chain: Vec<String> },

    #[error("日期 `{0}` 非法（需 ISO YYYY-MM-DD）")]
    InvalidDate(String),

    #[error("事件日期 `{date}` 无生效版本（早于/晚于所有版本生效范围）")]
    NoEffectiveVersion { date: String },

    #[error("生效范围重复（版本 `{0}` 的主版本出现多次），解析不确定")]
    DuplicateMain(String),

    #[error("版本错误: {0}")]
    Version(#[from] VersionError),
}

/// 主版本生效范围（33 号 §3：Patch 不改变生效日期，范围按主版本挂载）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRange {
    /// 主版本，如 "v1" / "v2"
    pub version: String,
    /// 生效起始（含），ISO YYYY-MM-DD
    pub effective_from: String,
    /// 失效日（不含）；None = ∞（33 号 §8）
    pub effective_to: Option<String>,
}

/// 版本解析器（纯函数，无状态）
pub struct VersionResolver;

impl VersionResolver {
    /// 按版本选择配置解析应使用的数据集版本
    pub fn resolve(
        selection: &VersionSelection,
        event_date: &str,
        effective: &[EffectiveRange],
        chain: &[String],
    ) -> Result<String, ResolveError> {
        match selection.mode {
            VersionSelectionMode::AutoByEffectiveDate => {
                Self::resolve_auto(event_date, effective, chain)
            }
            VersionSelectionMode::Pinned => Self::resolve_pinned(selection, chain),
        }
    }

    /// `auto_by_effective_date`（33 号 §5 算法）
    ///
    /// 1. 候选主版本 = { v : effective_from ≤ T 且 effective_to > T }（to=None 视为 ∞）；
    /// 2. 取候选最高主版本 V_main；
    /// 3. 输出 = V_main 的当前最新 Patch。
    pub fn resolve_auto(
        event_date: &str,
        effective: &[EffectiveRange],
        chain: &[String],
    ) -> Result<String, ResolveError> {
        validate_date(event_date)?;
        // 主版本生效范围去重（确定性：同一主版本只允许一个生效范围）
        let mut by_main: BTreeMap<u32, &EffectiveRange> = BTreeMap::new();
        for range in effective {
            validate_date(&range.effective_from)?;
            if let Some(to) = &range.effective_to {
                validate_date(to)?;
            }
            let main = Version::parse(&range.version)?.main;
            if by_main.insert(main, range).is_some() {
                return Err(ResolveError::DuplicateMain(range.version.clone()));
            }
        }
        // 1) 候选主版本
        let candidates: Vec<(&u32, &&EffectiveRange)> = by_main
            .iter()
            .filter(|(_, r)| {
                r.effective_from.as_str() <= event_date
                    && r.effective_to
                        .as_ref()
                        .map(|t| t.as_str() > event_date)
                        .unwrap_or(true)
            })
            .collect();
        let Some((main, _)) = candidates.last() else {
            return Err(ResolveError::NoEffectiveVersion {
                date: event_date.into(),
            });
        };
        // 2-3) 最高主版本 + 其最新 Patch
        Self::latest_patch_of(**main, chain).ok_or_else(|| ResolveError::MainMissingInChain {
            main: **main,
            chain: chain.to_vec(),
        })
    }

    /// `pinned`（33 号 §4 合流语义）
    ///
    /// - `pinned_include_patch = true`（默认）：只锁主版本语义，取同主版本下最新 Patch；
    /// - `pinned_include_patch = false`：完全锁定到指定版本（取证/复现）。
    pub fn resolve_pinned(
        selection: &VersionSelection,
        chain: &[String],
    ) -> Result<String, ResolveError> {
        let pinned = selection
            .pinned_version
            .as_deref()
            .ok_or(ResolveError::MissingPinnedVersion)?;
        let version = Version::parse(pinned)?;
        if selection.pinned_include_patch() {
            Self::latest_patch_of(version.main, chain).ok_or_else(|| {
                ResolveError::PinnedNotInChain {
                    version: pinned.into(),
                    chain: chain.to_vec(),
                }
            })
        } else {
            if !chain.iter().any(|v| v == pinned) {
                return Err(ResolveError::PinnedNotInChain {
                    version: pinned.into(),
                    chain: chain.to_vec(),
                });
            }
            Ok(pinned.to_string())
        }
    }

    /// 版本链中某主版本的当前最新版本（含 Patch；链单调递增，取最后一个同主版本）
    fn latest_patch_of(main: u32, chain: &[String]) -> Option<String> {
        chain
            .iter()
            .filter_map(|v| {
                let parsed = Version::parse(v).ok()?;
                (parsed.main == main).then_some(v.clone())
            })
            .next_back()
    }
}

/// ISO YYYY-MM-DD 格式校验（日期为定长数字，字典序即时间序）
fn validate_date(s: &str) -> Result<(), ResolveError> {
    let parts: Vec<&str> = s.split('-').collect();
    let ok = parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()));
    if ok {
        Ok(())
    } else {
        Err(ResolveError::InvalidDate(s.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造：v1 生效 2024-01-01 起；v2 生效 2025-01-01 起（v2.p1 为补丁，沿用 v2 生效期）
    fn ranges() -> Vec<EffectiveRange> {
        vec![
            EffectiveRange {
                version: "v1".into(),
                effective_from: "2024-01-01".into(),
                effective_to: None,
            },
            EffectiveRange {
                version: "v2".into(),
                effective_from: "2025-01-01".into(),
                effective_to: None,
            },
        ]
    }

    fn chain_v2_p1() -> Vec<String> {
        vec!["v1".into(), "v2".into(), "v2.p1".into()]
    }

    fn selection(mode: VersionSelectionMode, pinned: Option<&str>, include_patch: Option<bool>) -> VersionSelection {
        VersionSelection {
            mode,
            pinned_version: pinned.map(String::from),
            pinned_include_patch: include_patch,
        }
    }

    #[test]
    fn test_auto_picks_latest_effective_major() {
        let chain = chain_v2_p1();
        // 2024 年内 → v1
        assert_eq!(
            VersionResolver::resolve_auto("2024-06-01", &ranges(), &chain).unwrap(),
            "v1"
        );
        // 2025 年起 → v2（含其最新 Patch v2.p1）
        assert_eq!(
            VersionResolver::resolve_auto("2025-06-01", &ranges(), &chain).unwrap(),
            "v2.p1"
        );
        // 生效起始日（含）→ 当天已生效
        assert_eq!(
            VersionResolver::resolve_auto("2025-01-01", &ranges(), &chain).unwrap(),
            "v2.p1"
        );
    }

    #[test]
    fn test_auto_half_open_effective_to() {
        // v1 2024-01-01 ~ 2025-01-01（不含）；v2 2025-01-01 起
        let rs = vec![
            EffectiveRange { version: "v1".into(), effective_from: "2024-01-01".into(), effective_to: Some("2025-01-01".into()) },
            EffectiveRange { version: "v2".into(), effective_from: "2025-01-01".into(), effective_to: None },
        ];
        assert_eq!(VersionResolver::resolve_auto("2024-12-31", &rs, &chain_v2_p1()).unwrap(), "v1");
        // 2025-01-01：effective_to 不含 → 已切到 v2
        assert_eq!(VersionResolver::resolve_auto("2025-01-01", &rs, &chain_v2_p1()).unwrap(), "v2.p1");
    }

    #[test]
    fn test_auto_no_effective_version() {
        // 早于所有生效范围
        let err = VersionResolver::resolve_auto("2023-12-31", &ranges(), &chain_v2_p1()).unwrap_err();
        assert!(matches!(err, ResolveError::NoEffectiveVersion { .. }));
    }

    #[test]
    fn test_auto_rejects_invalid_date() {
        let err = VersionResolver::resolve_auto("2024/06/01", &ranges(), &chain_v2_p1()).unwrap_err();
        assert!(matches!(err, ResolveError::InvalidDate(_)));
        // 范围日期非法同样显式报错
        let bad = vec![EffectiveRange {
            version: "v1".into(),
            effective_from: "24-01-01".into(),
            effective_to: None,
        }];
        let err = VersionResolver::resolve_auto("2024-06-01", &bad, &["v1".into()]).unwrap_err();
        assert!(matches!(err, ResolveError::InvalidDate(_)));
    }

    #[test]
    fn test_auto_duplicate_main_rejected() {
        // 同一主版本两个生效范围 → 解析不确定，显式报错
        let dup = vec![
            EffectiveRange { version: "v2".into(), effective_from: "2025-01-01".into(), effective_to: None },
            EffectiveRange { version: "v2".into(), effective_from: "2025-06-01".into(), effective_to: None },
        ];
        let err = VersionResolver::resolve_auto("2025-06-01", &dup, &chain_v2_p1()).unwrap_err();
        assert!(matches!(err, ResolveError::DuplicateMain(_)));
    }

    #[test]
    fn test_pinned_include_patch_default_latest() {
        let chain = chain_v2_p1();
        // 默认 include_patch=true：pinned v2 → 最新 Patch v2.p1
        let s = selection(VersionSelectionMode::Pinned, Some("v2"), None);
        assert_eq!(VersionResolver::resolve_pinned(&s, &chain).unwrap(), "v2.p1");
    }

    #[test]
    fn test_pinned_exact_version() {
        let chain = chain_v2_p1();
        // include_patch=false：完全锁定 v2（不跟进 v2.p1）
        let s = selection(VersionSelectionMode::Pinned, Some("v2"), Some(false));
        assert_eq!(VersionResolver::resolve_pinned(&s, &chain).unwrap(), "v2");
        // 精确锁定某 Patch
        let s = selection(VersionSelectionMode::Pinned, Some("v2.p1"), Some(false));
        assert_eq!(VersionResolver::resolve_pinned(&s, &chain).unwrap(), "v2.p1");
    }

    #[test]
    fn test_pinned_missing_or_unknown() {
        let chain = chain_v2_p1();
        // 缺 pinned_version
        let s = selection(VersionSelectionMode::Pinned, None, None);
        let err = VersionResolver::resolve_pinned(&s, &chain).unwrap_err();
        assert!(matches!(err, ResolveError::MissingPinnedVersion));
        // 精确锁定到链中不存在的版本
        let s = selection(VersionSelectionMode::Pinned, Some("v9"), Some(false));
        let err = VersionResolver::resolve_pinned(&s, &chain).unwrap_err();
        assert!(matches!(err, ResolveError::PinnedNotInChain { .. }));
        // include_patch=true 但主版本不在链中
        let s = selection(VersionSelectionMode::Pinned, Some("v9"), Some(true));
        let err = VersionResolver::resolve_pinned(&s, &chain).unwrap_err();
        assert!(matches!(err, ResolveError::PinnedNotInChain { .. }));
    }

    #[test]
    fn test_resolve_dispatch() {
        let chain = chain_v2_p1();
        // auto 分发
        let s = selection(VersionSelectionMode::AutoByEffectiveDate, None, None);
        assert_eq!(VersionResolver::resolve(&s, "2025-06-01", &ranges(), &chain).unwrap(), "v2.p1");
        // pinned 分发
        let s = selection(VersionSelectionMode::Pinned, Some("v2"), Some(false));
        assert_eq!(VersionResolver::resolve(&s, "2025-06-01", &ranges(), &chain).unwrap(), "v2");
    }
}
