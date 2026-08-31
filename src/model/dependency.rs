//! 数据依赖声明（31 号 §6；完整设计在 35 号 决策点⑤）
//!
//! **T1 决策（2026-08-24）**：依赖模型（DataDependencies/SourceBinding/ServiceDecl/InputDecl/
//! IoContract/ServiceTemplate）唯一来源已迁至 `evorule-bundle`（SSOT），本模块 re-export。
//! **治理专属 `ServiceTemplateRecord`**（44 号 §7 deps/templates 注册）保留在 evorule-rule。
//! 定义与单测见 [evorule-bundle/src/dependency.rs](../../../../evorule-bundle/src/dependency.rs)。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use evorule_bundle::dependency::{
    DataDependencies, EventDirection, EventSchemaDecl, InputDecl, IoContract, ServiceDecl,
    ServiceTemplate, SourceBinding,
};

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
