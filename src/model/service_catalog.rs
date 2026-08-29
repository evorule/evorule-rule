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

/// 官方预置原生服务（SSOT：与 evorule-server demo-services `NATIVE_SERVICE_NAMES` 对齐）。
/// (service_name, sensitive, description)
pub const OFFICIAL_NATIVE_SERVICES: &[(&str, bool, &str)] = &[
    ("inverse_kinematics_solver", false, "机器人逆运动学求解（Phase 1 原生）"),
    ("robot_move_joints", false, "机器人关节移动（确定性，Phase 1 原生）"),
    ("llm_advisor", true, "LLM 建议服务（sensitive：涉及外部 LLM API）"),
    ("shadow_ik_solver", false, "影子 IK 求解（对照验证）"),
    ("sampling_service", false, "采样服务"),
    ("rule_sandbox", false, "规则沙箱验证服务"),
    ("config_persist", false, "规则热加载持久化服务"),
];

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
