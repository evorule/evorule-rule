//! 数据依赖声明（31 号 §6；完整设计在 35 号 决策点⑤）
//!
//! - 数据集级 `data_dependencies`：声明规则运行所需数据来源（推入式 inputs / 拉取式 services）；
//! - 条目级 `data_source_binding`：将 `rule_body` 内的 service_name 符号映射到具体服务；
//! - 凭据强约束：声明不存端点/凭据（凭据永不入库，只走执行侧密钥管理）。
//!
//! 符号三方一致（31 号 §9-3）：
//! `rule_body.io_request.service_name` ≡ 条目 binding.service_name ≡ 数据集声明 services[].service_name

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 推入式输入声明（事件/Fact 形态，供沙箱生成合成事件；35 号 §4 完整 schema）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputDecl {
    pub name: String,
    /// 输入形态 schema（JSON Schema 子集）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 事件长期为空是否视为异常（35 号 §4，默认 false；开放点② 建议 false）
    #[serde(default)]
    pub empty_allowed: bool,
}

/// 无凭据服务模板（35 号 §5，决策点⑤ 方案 B）
///
/// 模板 = 端点形状 + 参数占位 + 说明，**不含真实端点/密钥**；实际值由消费者在
/// 执行侧 service_registry 填写（层 2 绑定动作，§3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ServiceTemplate {
    /// 允许占位符（如 "https://{base}/api/payroll"），实际值由消费者填
    pub url: String,
    /// 默认 POST（35 号 §3）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// 鉴权头模板（占位符形式，如 `{client_id}`）；BTreeMap 保证序列化确定性（36 号内容哈希）
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers_templates: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms_hint: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// 拉取式服务声明
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceDecl {
    pub service_name: String,
    /// 输入输出契约
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_contract: Option<IoContract>,
    /// 是否涉及凭据/敏感数据
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 无凭据服务模板（可选，帮助消费者配置；35 号 §5）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<ServiceTemplate>,
}

/// 服务输入输出契约
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct IoContract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#in: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<Value>,
}

/// 数据集级数据依赖声明
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DataDependencies {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceDecl>,
}

impl DataDependencies {
    /// 是否声明了指定服务
    pub fn has_service(&self, service_name: &str) -> bool {
        self.services.iter().any(|s| s.service_name == service_name)
    }
}

/// 条目级绑定：rule_body 内符号 → 具体服务（rule_ref 记录符号在 rule_body 中的路径）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceBinding {
    pub rule_ref: String,
    pub service_name: String,
}

/// 服务模板注册记录（44 号 §7 deps/templates；35 号 §5 无凭据模板）
///
/// 模板 = 端点形状 + 参数占位 + 说明，**不含真实端点/密钥**；实际值由消费者在
/// 执行侧 service_registry 填写（层 2 绑定动作）。`bind` 端点做占位符填充。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceTemplateRecord {
    pub template_id: String,
    pub tenant_id: String,
    pub service_name: String,
    /// pull | push
    pub kind: String,
    #[serde(default)]
    pub io_contract: IoContract,
    /// 允许占位符（如 "http://{host}:{port}/api/payroll"）
    pub endpoint_template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers_template: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub placeholder_notes: BTreeMap<String, String>,
    pub created_at: String,
    pub created_by: String,
}

impl ServiceTemplateRecord {
    /// 占位符填充：把 `values` 中的值替换到 endpoint_template 与 headers_template。
    /// 未提供的占位符保留原样（剩余值由消费者在执行侧填写，35 号 §5）。
    pub fn bind(&self, values: &BTreeMap<String, String>) -> crate::model::dependency::ServiceTemplate {
        let fill = |s: &str| -> String {
            let mut out = s.to_string();
            for (k, v) in values {
                out = out.replace(&format!("{{{k}}}"), v);
            }
            out
        };
        let headers_templates: BTreeMap<String, String> = self
            .headers_template
            .iter()
            .map(|(k, v)| (k.clone(), fill(v)))
            .collect();
        crate::model::dependency::ServiceTemplate {
            url: fill(&self.endpoint_template),
            method: self.method.clone(),
            headers_templates,
            timeout_ms_hint: None,
            notes: Some(format!(
                "源自模板 {}（{}，未填占位符见 placeholder_notes）",
                self.template_id, self.service_name
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_dependencies_serde() {
        let dd = DataDependencies {
            inputs: vec![InputDecl {
                name: "payroll_event".into(),
                schema: Some(serde_json::json!({"emp_id": "string"})),
                description: Some("工资计算触发事件".into()),
                empty_allowed: false,
            }],
            services: vec![ServiceDecl {
                service_name: "payroll_svc".into(),
                io_contract: Some(IoContract {
                    r#in: Some(serde_json::json!({"emp_id": "string"})),
                    out: Some(serde_json::json!({"amount": "number"})),
                }),
                sensitive: false,
                description: Some("工资发放数据服务".into()),
                template: Some(ServiceTemplate {
                    url: "https://{base}/api/payroll".into(),
                    method: Some("POST".into()),
                    headers_templates: BTreeMap::from([("X-Client-Id".into(), "{client_id}".into())]),
                    timeout_ms_hint: Some(5000),
                    notes: Some("base/鉴权按客户环境填写".into()),
                }),
            }],
        };
        let json = serde_json::to_string(&dd).unwrap();
        assert!(json.contains("payroll_svc"));
        assert!(json.contains("{client_id}")); // 模板占位符保留
        let back: DataDependencies = serde_json::from_str(&json).unwrap();
        assert_eq!(dd, back);
        assert!(back.has_service("payroll_svc"));
        assert!(!back.has_service("nope"));
        // 缺省字段回退（老数据兼容：无 template/description/empty_allowed 反序列化成功）
        let old: DataDependencies = serde_json::from_str(
            r#"{"services":[{"service_name":"s","sensitive":false}]}"#,
        )
        .unwrap();
        assert_eq!(old.services[0].template, None);
        assert_eq!(old.services[0].description, None);
        assert_eq!(old.inputs, vec![]);
    }
}
