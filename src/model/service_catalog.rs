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

/// 官方预置原生服务种子（UV-029 声明文件化）。
///
/// SSOT = evorule-server 仓 `plugins/demo-services/official_native_services.json`；
/// 本仓持有嵌入副本 `official_native_services.embedded.json`（由 evorule-server
/// `scripts/sync-native-services.ps1` 从源仓同步），守卫测试锁定副本合法性与顺序。
/// 返回 `(service_name, sensitive, description)`；副本损坏即 fail-fast
/// （嵌入副本属仓内完整性问题，如实报错、不静默跳过）。
pub fn official_native_services() -> Vec<(String, bool, String)> {
    let raw = include_str!("official_native_services.embedded.json");
    let file: serde_json::Value = serde_json::from_str(raw)
        .expect("official_native_services.embedded.json 非法 JSON — 请在 evorule-server 仓运行 scripts/sync-native-services.ps1 重新同步");
    let services = file
        .get("services")
        .and_then(|v| v.as_array())
        .expect("嵌入副本缺 services 数组 — 请重新同步嵌入副本");
    services
        .iter()
        .map(|s| {
            let name = s
                .get("name")
                .and_then(|v| v.as_str())
                .expect("嵌入副本条目缺 name — 请重新同步嵌入副本")
                .to_string();
            let sensitive = s
                .get("sensitive")
                .and_then(|v| v.as_bool())
                .expect("嵌入副本条目缺 sensitive — 请重新同步嵌入副本");
            let description = s
                .get("description")
                .and_then(|v| v.as_str())
                .expect("嵌入副本条目缺 description — 请重新同步嵌入副本")
                .to_string();
            (name, sensitive, description)
        })
        .collect()
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

    /// UV-029 嵌入副本守卫：副本可解析且 schema 合法（字段齐全、非空、name 唯一、
    /// 顺序稳定 = SSOT 文件序）。与源仓 SSOT 的内容一致性由同步脚本
    /// （evorule-server `scripts/sync-native-services.ps1`：复制 + 双侧守卫）保证。
    #[test]
    fn test_embedded_native_services_copy_valid() {
        let seed = official_native_services();
        assert!(!seed.is_empty(), "嵌入副本不应为空");
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
        // 顺序锁定:首项必须是声明表首服务(路由查找序与目录种子序同源)
        assert_eq!(
            names.first().map(String::as_str),
            Some("inverse_kinematics_solver"),
            "嵌入副本顺序漂移,请重新同步"
        );
    }
}
