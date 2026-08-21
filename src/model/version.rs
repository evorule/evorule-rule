//! 版本与法规锚（31 号 §3 versioning / law_ref / version_selection；决策点③）
//!
//! - 版本号：两级版本号（v1 / v2 / v2.p1），主版本 + Patch；
//! - 版本选择双模式：`auto_by_effective_date`（按生效日期自动切换，合规默认）| `pinned`（显式锁定）；
//! - `law_ref` 是合规数据集的核心锚——法规升版 → 新数据集版本。

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 版本错误（决策点②/③：版本号格式、链单调性、可复用性）
#[derive(Debug, Error, PartialEq)]
pub enum VersionError {
    #[error("非法版本号 `{0}`（需形如 v1 / v2.p1）")]
    BadFormat(String),

    #[error("新版本 `{new}` 不高于当前版本 `{current}`（版本链只追加，不可复用/回退）")]
    NotAdvancing { current: String, new: String },

    #[error("版本链尾 `{chain_tail}` 与 current `{current}` 不一致（链损坏）")]
    ChainTailMismatch { chain_tail: String, current: String },
}

/// 两级版本号（决策点③）：主版本 + Patch
///
/// - 主版本：法规条款级变化 → 升版（v1 → v2）；
/// - Patch：内部小改（法规语义未变）→ Patch +1（v2 → v2.p1）；
/// - 序：`v1 < v2 < v2.p1 < v3`（裸主版本先于其首个 Patch）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub main: u32,
    pub patch: Option<u32>,
}

impl Version {
    /// 解析 "v1" / "v2.p1"
    pub fn parse(s: &str) -> Result<Self, VersionError> {
        let raw = s.trim();
        let body = raw
            .strip_prefix('v')
            .ok_or_else(|| VersionError::BadFormat(raw.into()))?;
        let (main_s, patch_s) = match body.split_once(".p") {
            Some((m, p)) => (m, Some(p)),
            None => (body, None),
        };
        let main: u32 = main_s
            .parse()
            .map_err(|_| VersionError::BadFormat(raw.into()))?;
        let patch = match patch_s {
            Some("") => return Err(VersionError::BadFormat(raw.into())),
            Some(p) => Some(
                p.parse()
                    .map_err(|_| VersionError::BadFormat(raw.into()))?,
            ),
            None => None,
        };
        Ok(Self { main, patch })
    }

    /// 升版：主版本 +1，Patch 清零（v2.p1 → v3）
    pub fn bump_major(&self) -> Self {
        Self {
            main: self.main + 1,
            patch: None,
        }
    }

    /// Patch：主版本不变，Patch +1（v2 → v2.p1；v2.p1 → v2.p2）
    pub fn bump_patch(&self) -> Self {
        Self {
            main: self.main,
            patch: Some(self.patch.unwrap_or(0) + 1),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.patch {
            Some(p) => write!(f, "v{}.p{}", self.main, p),
            None => write!(f, "v{}", self.main),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.main
            .cmp(&other.main)
            .then_with(|| self.patch.cmp(&other.patch)) // None < Some(patch)：v2 < v2.p1
    }
}

/// 变更线（决策点③）：升版 / Patch
///
/// 判定标准（33 号 §3）：**是否改变规则对外业务语义**——
/// 生效日期/阈值/条款内容变更 → Major（升版）；措辞/格式/连接器引用/bug 修复 → Patch。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BumpKind {
    /// 升版：主版本 +1（法规条款级变化）
    Major,
    /// Patch：主版本不变，Patch +1（内部小改）
    Patch,
}

/// 版本选择模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionSelectionMode {
    AutoByEffectiveDate,
    Pinned,
}

/// 版本选择配置
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionSelection {
    pub mode: VersionSelectionMode,
    /// pinned 时锁定的版本（如 "v2" / "v2.p1"）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_version: Option<String>,
    /// pinned 时 Patch 是否跟进（33 号 §4 合流语义）：默认 `true`
    /// （同主版本下采用最新 Patch）；`false` = 完全锁定到指定版本（取证/复现）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_include_patch: Option<bool>,
}

impl VersionSelection {
    /// `pinned_include_patch` 默认值 = `true`（33 号 §4）
    pub fn pinned_include_patch(&self) -> bool {
        self.pinned_include_patch.unwrap_or(true)
    }
}

/// 法规锚（合规数据集核心锚）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LawRef {
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub law_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<String>,
}

/// 数据集级版本链
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versioning {
    /// 当前版本（如 "v2.p1"）
    pub current: String,
    /// 版本链（按时间升序，最新在末位；历史可回查、可 diff）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
}

impl Default for Versioning {
    fn default() -> Self {
        Self {
            current: "v1".to_string(),
            chain: vec!["v1".to_string()],
        }
    }
}

impl Versioning {
    /// 生成新版本（决策点③ 两级变更线）：升版（Major）或 Patch。
    ///
    /// 版本链只追加、不可复用：新版本必须高于当前且不在链中。
    /// 返回新版本号；更新 `current` 并追加 `chain`。
    pub fn bump(&self, kind: BumpKind) -> Result<Self, VersionError> {
        let current = Version::parse(&self.current)?;
        let next = match kind {
            BumpKind::Major => current.bump_major(),
            BumpKind::Patch => current.bump_patch(),
        };
        let next_str = next.to_string();
        // 版本号不可复用 + 链单调（32 号 §3 / 33 号 §8）
        if next <= current || self.chain.iter().any(|v| v == &next_str) {
            return Err(VersionError::NotAdvancing {
                current: self.current.clone(),
                new: next_str,
            });
        }
        let mut chain = self.chain.clone();
        chain.push(next_str.clone());
        Ok(Self {
            current: next_str,
            chain,
        })
    }

    /// 版本链一致性校验（32 号 §3 / 33 号 §8）：
    /// - 格式合法；- 链单调递增、无重复；- `current` 必须是链尾。
    pub fn validate(&self) -> Result<(), VersionError> {
        let mut prev: Option<Version> = None;
        for v in &self.chain {
            let parsed = Version::parse(v)?;
            if let Some(p) = prev {
                if parsed <= p {
                    return Err(VersionError::NotAdvancing {
                        current: p.to_string(),
                        new: v.clone(),
                    });
                }
            }
            prev = Some(parsed);
        }
        if let Some(tail) = prev {
            if tail.to_string() != self.current {
                return Err(VersionError::ChainTailMismatch {
                    chain_tail: tail.to_string(),
                    current: self.current.clone(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_versioning_serde() {
        let v = Versioning {
            current: "v2.p1".into(),
            chain: vec!["v1".into(), "v2".into(), "v2.p1".into()],
        };
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("\"v2.p1\""));
        let back: Versioning = serde_json::from_str(&json).unwrap();
        assert_eq!(back.chain.len(), 3);
    }

    #[test]
    fn test_version_selection_mode() {
        let m = VersionSelectionMode::AutoByEffectiveDate;
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            "\"auto_by_effective_date\""
        );
        let pinned = VersionSelectionMode::Pinned;
        assert_eq!(serde_json::to_string(&pinned).unwrap(), "\"pinned\"");
    }

    #[test]
    fn test_version_parse_display() {
        assert_eq!(Version::parse("v1").unwrap(), Version { main: 1, patch: None });
        assert_eq!(Version::parse("v2.p1").unwrap(), Version { main: 2, patch: Some(1) });
        assert_eq!(Version::parse("v2.p1").unwrap().to_string(), "v2.p1");
        assert_eq!(Version::parse("v1").unwrap().to_string(), "v1");
        // 非法格式
        assert!(matches!(Version::parse("1"), Err(VersionError::BadFormat(_))));
        assert!(matches!(Version::parse("v"), Err(VersionError::BadFormat(_))));
        assert!(matches!(Version::parse("v2.p"), Err(VersionError::BadFormat(_))));
        assert!(matches!(Version::parse("vx"), Err(VersionError::BadFormat(_))));
    }

    #[test]
    fn test_version_ordering() {
        let v1 = Version::parse("v1").unwrap();
        let v2 = Version::parse("v2").unwrap();
        let v2p1 = Version::parse("v2.p1").unwrap();
        let v3 = Version::parse("v3").unwrap();
        // v1 < v2 < v2.p1 < v3
        assert!(v1 < v2 && v2 < v2p1 && v2p1 < v3);
        assert!(v2 < v2p1); // 裸主版本先于其首个 Patch
    }

    #[test]
    fn test_bump_major_and_patch() {
        // v1 → 升版 v2 → Patch v2.p1 → Patch v2.p2 → 升版 v3
        let mut v = Versioning::default();
        assert_eq!(v.current, "v1");
        v = v.bump(BumpKind::Major).unwrap();
        assert_eq!(v.current, "v2");
        assert_eq!(v.chain, vec!["v1", "v2"]);
        v = v.bump(BumpKind::Patch).unwrap();
        assert_eq!(v.current, "v2.p1");
        v = v.bump(BumpKind::Patch).unwrap();
        assert_eq!(v.current, "v2.p2");
        v = v.bump(BumpKind::Major).unwrap();
        assert_eq!(v.current, "v3"); // Patch 清零
        assert_eq!(v.chain, vec!["v1", "v2", "v2.p1", "v2.p2", "v3"]);
        v.validate().unwrap();
    }

    #[test]
    fn test_bump_reuse_rejected() {
        // 版本号不可复用：计算出的新版本已在链中 → 拒绝（即使链已损坏，也绝不重复版本号）
        let dup = Versioning {
            current: "v1".into(),
            chain: vec!["v1".into(), "v2".into()],
        };
        let err = dup.bump(BumpKind::Major).unwrap_err(); // v1 → v2，而 v2 已在链中
        assert!(matches!(err, VersionError::NotAdvancing { .. }));
    }

    #[test]
    fn test_validate_catches_corruption() {
        // current 与链尾不一致
        let bad = Versioning {
            current: "v1".into(),
            chain: vec!["v1".into(), "v2".into()],
        };
        assert!(matches!(
            bad.validate(),
            Err(VersionError::ChainTailMismatch { .. })
        ));
        // 链乱序（v2 出现在 v1 之前）
        let unsorted = Versioning {
            current: "v2".into(),
            chain: vec!["v2".into(), "v1".into()],
        };
        assert!(matches!(
            unsorted.validate(),
            Err(VersionError::NotAdvancing { .. })
        ));
    }
}
