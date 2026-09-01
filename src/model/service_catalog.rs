//! 服务目录（02 方案 C2：服务名/契约治理侧 SSOT）
//!
//! 定位：治理侧「已知服务」参考目录 —— 服务名、版本、契约、敏感标记、绑定提示。
//! 作用：
//! - 依赖声明事前预检（`PUT /deps/datasets/{id}` 校验 service_name ∈ 目录）；
//! - 快照包导出时补齐服务契约（io_contract/sensitive/version 随包携带）；
//! - 与执行侧 `bound_services`（原生 + registry）衔接的三层绑定层 0。
//!
//! 边界：目录存「服务描述」，不存端点/凭据（凭据永不入库，走执行侧密钥管理）。

use serde::{Deserialize, Serialize};

use crate::model::dependency::IoContract;

/// 服务绑定提示（执行侧如何提供该服务；D9 服务独立化演进方向）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BindingHint {
    /// 原生内嵌（demo-services 等插件）
    #[default]
    Native,
    /// service_registry.json 显式绑定
    Registry,
    /// 独立服务进程（D9 演进：独立开发/部署/演进）
    Standalone,
}

/// 服务目录条目
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceCatalogEntry {
    pub service_name: String,
    /// 服务业务版本（C4）
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 输入输出契约（C6）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_contract: Option<IoContract>,
    /// 是否涉及凭据/敏感数据（C6）
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub binding_hint: BindingHint,
    /// official | org:<tenant_id>
    pub managed_by: String,
    /// platform | tenant:<tenant_id>
    pub scope: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

fn default_version() -> String {
    "1.0.0".into()
}

/// 嵌入副本登记表（UV-035 泛化）：新增插件 = 在此追加一项
/// `(插件 id, include_str! 嵌入副本)`，聚合函数与守卫测试零改动
/// （副本由 evorule-server `scripts/sync-native-services.ps1` 同步）。
/// 声明序 = 聚合序，与执行侧挂载链声明序一致。
const EMBEDDED_SERVICE_FILES: &[(&str, &str)] = &[
    (
        "demo-services",
        include_str!("official_native_services.demo-services.embedded.json"),
    ),
    (
        "physics-services",
        include_str!("official_native_services.physics-services.embedded.json"),
    ),
    (
        "indicator-services",
        include_str!("official_native_services.indicator-services.embedded.json"),
    ),
];

/// 官方预置原生服务种子（UV-029 声明文件化；UV-035 泛化至多插件聚合）。
///
/// SSOT = evorule-server 仓各插件目录 `plugins/<id>/official_native_services.json`；
/// 本仓持有各插件嵌入副本（由 evorule-server `scripts/sync-native-services.ps1`
/// 从源仓同步），守卫测试锁定副本合法性与顺序。聚合序 = [`EMBEDDED_SERVICE_FILES`]
/// 声明序（与执行侧挂载链声明序一致）；跨插件服务名全局唯一，冲突即 fail-fast
/// （嵌入副本属仓内完整性问题，如实报错、不静默跳过）。
/// 返回 `(service_name, sensitive, description)`。
pub fn official_native_services() -> Vec<(String, bool, String)> {
    let mut all: Vec<(String, bool, String)> = Vec::new();
    for (plugin_id, raw) in EMBEDDED_SERVICE_FILES {
        let file: serde_json::Value = serde_json::from_str(raw)
            .unwrap_or_else(|e| {
                panic!(
                    "official_native_services.{plugin_id}.embedded.json 非法 JSON: {e} — \
                     请在 evorule-server 仓运行 scripts/sync-native-services.ps1 重新同步"
                )
            });
        let services = file.get("services").and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("official_native_services.{plugin_id}.embedded.json 缺 services 数组 — 请重新同步嵌入副本"));
        for s in services {
            let name = s
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("official_native_services.{plugin_id}.embedded.json 条目缺 name — 请重新同步嵌入副本"))
                .to_string();
            let sensitive = s
                .get("sensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| panic!("official_native_services.{plugin_id}.embedded.json 条目缺 sensitive — 请重新同步嵌入副本"));
            let description = s
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("official_native_services.{plugin_id}.embedded.json 条目缺 description — 请重新同步嵌入副本"))
                .to_string();
            if all.iter().any(|(n, _, _)| *n == name) {
                panic!(
                    "服务名 {name} 跨插件重复（plugin_id={plugin_id}）— \
                     全局服务名必须唯一，请修正源仓声明文件后重新同步"
                );
            }
            all.push((name, sensitive, description));
        }
    }
    all
}

/// 由官方种子生成目录条目（version=1.0.0，binding_hint=Native，宽松契约摘要）
pub fn official_entry(name: &str, sensitive: bool, description: &str, now: &str) -> ServiceCatalogEntry {
    ServiceCatalogEntry {
        service_name: name.to_string(),
        version: default_version(),
        description: Some(description.to_string()),
        io_contract: Some(IoContract {
            r#in: Some(serde_json::json!({"$comment": format!("{name} 输入契约见执行侧实现")})),
            out: Some(serde_json::json!({"$comment": format!("{name} 输出契约见执行侧实现")})),
        }),
        sensitive,
        binding_hint: BindingHint::Native,
        managed_by: "official".to_string(),
        scope: "platform".to_string(),
        created_at: now.to_string(),
        updated_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UV-029 嵌入副本守卫（UV-035 泛化至多插件聚合）：各副本可解析且 schema
    /// 合法（字段齐全、非空、name 全局唯一、聚合序 = 登记表声明序）。
    /// 与源仓 SSOT 的内容一致性由同步脚本
    /// （evorule-server `scripts/sync-native-services.ps1`：复制 + 双侧守卫）保证。
    #[test]
    fn test_embedded_native_services_copy_valid() {
        let seed = official_native_services();
        assert!(!seed.is_empty(), "聚合种子不应为空");
        let mut names: Vec<String> = Vec::new();
        for (name, _, description) in &seed {
            assert!(!name.is_empty(), "存在空 name 条目");
            assert!(!description.is_empty(), "{name} 缺 description");
            names.push(name.clone());
        }
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "服务名重复 — 嵌入副本与 SSOT 漂移,请重新同步"
        );
        // 顺序锁定:聚合种子必须含各插件声明表首服务,且按登记表声明序排列
        // (demo-services 先于 physics-services,与执行侧挂载链同源)。
        let expect_heads = ["inverse_kinematics_solver", "physics_simulate"];
        let mut pos = 0usize;
        for head in expect_heads {
            let found = names[pos..]
                .iter()
                .position(|n| n == head)
                .unwrap_or_else(|| panic!("聚合种子缺插件首服务 {head} — 嵌入副本漂移,请重新同步"));
            pos += found;
            assert_eq!(
                names[pos], head,
                "聚合序漂移: {head} 应按登记表声明序出现,请重新同步"
            );
            pos += 1;
        }
    }
}
