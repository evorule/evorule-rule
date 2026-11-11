//! 域目录（律式/常数 schema 扩域：域 id 的治理侧 SSOT）
//!
//! 定位：跨仓「知识域」注册目录 —— 域 id、名称、描述。
//! 作用：
//! - law_set 律式 `domains`（多域归属）/`cross_refs`（跨域引用边落点域）与
//!   constant_set 常数 `domain` 的域 id 合法性校验基准（server 侧嵌入副本
//!   同步自本件，由 evorule-server `scripts/sync-domain-catalog.ps1` 同步——
//!   同步脚本另批接线）；
//! - 跨域引用边落点域判定（D2=A：注册时显式声明，禁自动发现）。
//!
//! 边界：目录只存域描述，不存域间换算/映射规则（那是律式本体的职责）。

use std::collections::BTreeMap;

use serde::Deserialize;

/// 域目录条目（全部字段必填；数据件静态输入，缺字段即解析失败 fail-fast）
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DomainCatalogEntry {
    pub domain_id: String,
    pub name: String,
    pub description: String,
}

/// 域目录数据件（治理侧 SSOT）
const DOMAIN_CATALOG_JSON: &str = include_str!("domain_catalog.v1.json");

/// 域目录（`domain_id -> 条目`；重复 id 属静态输入不变量破坏，fail-fast 不静默）
///
/// 惰性解析数据件一次（OnceLock 单例，std 等价 once_cell 语义——本仓未直依
/// once_cell，不为单一目录引入依赖面变更）。
pub fn domain_catalog() -> &'static BTreeMap<String, DomainCatalogEntry> {
    static CATALOG: std::sync::OnceLock<BTreeMap<String, DomainCatalogEntry>> =
        std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        let file: serde_json::Value = serde_json::from_str(DOMAIN_CATALOG_JSON)
            .unwrap_or_else(|e| panic!("domain_catalog.v1.json 非法 JSON: {e} — 请修正治理侧 SSOT 数据件"));
        let entries: Vec<DomainCatalogEntry> = serde_json::from_value(
            file.get("domains").cloned().unwrap_or(serde_json::Value::Null),
        )
        .unwrap_or_else(|e| {
            panic!(
                "domain_catalog.v1.json 缺 domains 数组或条目形态非法: {e} — 请修正治理侧 SSOT 数据件"
            )
        });
        let mut map = BTreeMap::new();
        for entry in entries {
            if map.insert(entry.domain_id.clone(), entry).is_some() {
                panic!(
                    "域目录 domain_id 重复 — 静态输入不变量破坏，请修正治理侧 SSOT 数据件"
                );
            }
        }
        map
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 域 id 命名规约（与律式/常数 id 同族：小写起头，小写字母/数字/下划线）
    const DOMAIN_ID_PATTERN: &str = "^[a-z][a-z0-9_]*$";

    /// 域 id 是否符合命名规约（测试侧形态检查，与数据件 pattern 同规约）
    fn matches_domain_pattern(id: &str) -> bool {
        let bytes = id.as_bytes();
        match bytes.first() {
            Some(b) if b.is_ascii_lowercase() => {}
            _ => return false,
        }
        bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
    }

    /// 域目录守卫：数据件可解析、目录非空、domain_id 唯一且符合命名规约、
    /// 条目字段齐全、五 seed 域在册。与 server 侧嵌入副本的字节一致性由
    /// 同步脚本（evorule-server scripts/sync-domain-catalog.ps1，另批接线）把关。
    #[test]
    fn test_embedded_domain_catalog_copy_valid() {
        let file: serde_json::Value =
            serde_json::from_str(DOMAIN_CATALOG_JSON).expect("domain_catalog.v1.json 非法 JSON");
        let domains = file
            .get("domains")
            .and_then(|v| v.as_array())
            .expect("domain_catalog.v1.json 缺 domains 数组");
        assert!(!domains.is_empty(), "域目录不应为空");

        let mut ids: Vec<String> = Vec::new();
        for d in domains {
            let id = d
                .get("domain_id")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("域目录条目缺 domain_id"));
            assert!(
                matches_domain_pattern(id),
                "domain_id `{id}` 不符合命名规约 {DOMAIN_ID_PATTERN}"
            );
            assert!(
                d.get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty()),
                "域 {id} 缺 name"
            );
            assert!(
                d.get("description")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty()),
                "域 {id} 缺 description"
            );
            ids.push(id.to_string());
        }
        let unique: HashSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "domain_id 重复 — 静态输入不变量破坏"
        );

        // 聚合函数与数据件一致（条目数与键集）
        let catalog = domain_catalog();
        assert_eq!(catalog.len(), ids.len(), "聚合目录与数据件条目数漂移");

        // 五 seed 域在册（物理/工程/经济/数学/化学）
        for seed in ["physical", "engineering", "economic", "math", "chemistry"] {
            assert!(catalog.contains_key(seed), "seed 域 {seed} 应在册");
        }
    }
}
