//! 校验（31 号 §9 约束与一致性）
//!
//! 校验规则：
//! - **符号三方一致**（§9-3）：`rule_body` 的 io_request.service_name ≡ 条目 binding.service_name
//!   ≡ 数据集 data_dependencies.services，缺失**显式报错**（不静默降级）；
//! - **LLM 边界**（§9-6 / 37 号强约束）：`llm_generated.flag=true` 的条目 status 只能是 Draft；
//! - **状态机基础**（§9-4，完整在 34 号）：5 态合法迁移（MVP 先落可达性基础）；
//! - **凭据禁止**（§9-5）：全模型无凭据字段，由模型设计保证（此处提供符号级兜底检查）。

use thiserror::Error;

use crate::model::dataset::RuleDataset;
use crate::model::entry::RuleEntry;
use crate::model::lifecycle::LifecycleStatus;

/// 校验错误
#[derive(Debug, Error, PartialEq)]
pub enum ValidationError {
    #[error("绑定服务 `{service}` 未在数据集 `{dataset}` 的 data_dependencies.services 中声明")]
    ServiceNotDeclared { dataset: String, service: String },

    #[error("绑定服务 `{service}` 未在 rule_body 的 io_request.service_name 中出现（规则体无此符号引用）")]
    ServiceNotInRuleBody { service: String },

    #[error("LLM 产出（llm_generated=true）的条目 `{entry}` 状态只能是 Draft，当前为 {status:?}")]
    LlmGeneratedNotDraft { entry: String, status: LifecycleStatus },

    #[error("rule_body 结构无法解析 transform（需含 type=io_request 且 params.service_name）")]
    InvalidRuleBody,

    #[error("发布前凭据静态扫描未通过：命中疑似凭据 {hits:?}（35 号 §6/§9-3：凭据永不入规则资产库）")]
    CredentialScanFailed { hits: Vec<String> },
}

/// 凭据静态扫描（35 号 §6/§9-3 强约束 MVP 手段）：
/// 发布前对序列化字符串做**启发式**疑似密钥模式匹配，命中即可疑，交由发布审批人复核。
///
/// 辅助"全模型无凭据字段"的模型设计（两层防线），扫描规则保守（尽可能命中真实凭据而少误伤规则正文）：
/// - AWS 访问密钥 `AKIA<16位大写字母数字>`、GitHub `ghp_/gho_/ghs_`；
/// - 常见密钥键名（api_key/access_token/password/secret/private_key/credential 等）
///   紧邻 `:`/`=` 且值为非空、非纯占位符的片段。
pub fn scan_credentials(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    // 1) 已知格式的硬编码凭据前缀（AWS 访问密钥、GitHub PAT/OAuth/部署密钥）
    let fmt_prefixes: &[&str] = &["AKIA", "ghp_", "gho_", "ghs_", "ghu_"];
    for p in fmt_prefixes {
        if text.contains(p) {
            hits.push(p.to_string());
        }
    }
    // 2) 密钥键名 + 收集
    let key_names: &[&str] = &[
        "api_key", "apikey", "access_token", "auth_token", "refresh_token",
        "secret", "client_secret", "private_key", "password", "passwd",
        "credential", "credentials", "authorization", "bearer",
    ];
    let lower = text.to_ascii_lowercase();
    for name in key_names {
        let mut pos = 0usize;
        while let Some(rel) = lower[pos..].find(name) {
            let idx = pos + rel;
            // 取键名后到值片段（冒号/等号后 → 到空白/逗号/右括号/引号/右括号/换行）
            if let Some(sep) = lower[idx..].find(|c| c == ':' || c == '=') {
                let vstart = idx + sep + 1;
                let vtrim = lower[vstart..].trim_start().trim_start_matches('"').trim_start_matches('\'');
                // 值非空、非布尔/纯数字/对象/占位符 → 疑似凭据
                let non_trivial = !vtrim.is_empty()
                    && !vtrim.starts_with("true")
                    && !vtrim.starts_with("false")
                    && !vtrim.starts_with('{')
                    && !vtrim.starts_with('[')
                    && !vtrim.starts_with('<')
                    && vtrim.chars().next().map(|c| !c.is_ascii_digit()).unwrap_or(false);
                if non_trivial {
                    let val: String = vtrim
                        .split(|c: char| c == ',' || c == '"' || c == '\'' || c == ')' || c == '}' || c == '\n' || c == ' ')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(|c| c == '{' || c == ',')
                        .to_string();
                    // 占位符/示例/说明值不算命中（避免误伤规则正文）
                    if !val.is_empty() && !val.contains("提示") && !val.contains("示例")
                        && !val.contains("占位") && !val.contains("PLACEHOLDER")
                        && !val.contains("...") && !val.contains('<') && !val.contains('{')
                    {
                        hits.push(format!("{name}={val}"));
                    }
                }
            }
            pos = idx + name.len();
        }
    }
    // 去重，保持顺序
    let mut seen = std::collections::HashSet::new();
    hits.retain(|h| seen.insert(h.clone()));
    hits
}

/// 校验器（纯函数，无状态）
pub struct Validator;

impl Validator {
    /// 从 rule_body 提取所有 `io_request.params.service_name`（锚定 10_role13_demo.json 结构）
    pub fn io_services_from_rule_body(rule_body: &serde_json::Value) -> Vec<String> {
        let mut out = Vec::new();
        let Some(transform) = rule_body.get("transform").and_then(|t| t.as_array()) else {
            return out;
        };
        for step in transform {
            let is_io = step
                .get("type")
                .and_then(|t| t.as_str())
                .map(|s| s == "io_request")
                .unwrap_or(false);
            if !is_io {
                continue;
            }
            if let Some(name) = step
                .get("params")
                .and_then(|p| p.get("service_name"))
                .and_then(|s| s.as_str())
            {
                out.push(name.to_string());
            }
        }
        out
    }

    /// 符号三方一致校验（§9-3）：rule_body ≡ binding ≡ dataset.data_dependencies
    pub fn validate_symbol_consistency(
        dataset: &RuleDataset,
        entry: &RuleEntry,
    ) -> Result<(), ValidationError> {
        // 1) 无绑定条目直接通过（无外部数据的规则）
        if entry.data_source_binding.is_empty() {
            return Ok(());
        }
        let rule_body_services = Self::io_services_from_rule_body(&entry.rule_body);
        let declared_services = dataset
            .data_dependencies
            .as_ref()
            .map(|d| &d.services)
            .cloned()
            .unwrap_or_default();

        for binding in &entry.data_source_binding {
            // a) 必须出现在数据集声明中
            if !declared_services.iter().any(|s| {
                s.service_name == binding.service_name
            }) {
                return Err(ValidationError::ServiceNotDeclared {
                    dataset: dataset.dataset_id.clone(),
                    service: binding.service_name.clone(),
                });
            }
            // b) 必须出现在 rule_body 的 io_request 中
            if !rule_body_services.contains(&binding.service_name) {
                return Err(ValidationError::ServiceNotInRuleBody {
                    service: binding.service_name.clone(),
                });
            }
        }
        Ok(())
    }

    /// LLM 边界（§9-6 / 37 号）：llm_generated=true → status 只能是 Draft
    pub fn validate_llm_boundary(entry: &RuleEntry) -> Result<(), ValidationError> {
        let is_llm = entry
            .governance
            .as_ref()
            .map(|g| g.is_llm_generated())
            .unwrap_or(false);
        if is_llm {
            let status = entry.status.unwrap_or(LifecycleStatus::Draft);
            if status != LifecycleStatus::Draft {
                return Err(ValidationError::LlmGeneratedNotDraft {
                    entry: entry.entry_id.clone(),
                    status,
                });
            }
        }
        Ok(())
    }

    /// 状态机合法迁移（34 号 §2）。返回 Err(Some(from,to)) 表示非法迁移。
    ///
    /// **不含 `Active → Published`**：Published 只能经独立发布审批（`validate_publish` +
    /// `publish_dataset`）显式进入，不能由通用状态迁移顺带完成（34 号 §3 强约束）。
    pub fn validate_transition(
        from: Option<LifecycleStatus>,
        to: LifecycleStatus,
    ) -> Result<(), (Option<LifecycleStatus>, LifecycleStatus)> {
        let from = from.unwrap_or(LifecycleStatus::Draft);
        let ok = matches!(
            (from, to),
            (LifecycleStatus::Draft, LifecycleStatus::Candidate)
                | (LifecycleStatus::Candidate, LifecycleStatus::Active)
                | (LifecycleStatus::Active, LifecycleStatus::Rejected)
                | (LifecycleStatus::Candidate, LifecycleStatus::Rejected)
                | (LifecycleStatus::Draft, LifecycleStatus::Rejected)
                // 撤销发布（34 号 §2；审批细节为开放点③）
                | (LifecycleStatus::Published, LifecycleStatus::Rejected)
                // 修订重来（34 号 §8-3，Rejected 非终态）
                | (LifecycleStatus::Rejected, LifecycleStatus::Draft)
        );
        if ok {
            Ok(())
        } else {
            Err((Some(from), to))
        }
    }

    /// 独立发布审批前置（34 号 §3 强约束）：**仅 Active 可发布**。
    /// Published 只能由显式发布操作（`publish_dataset`）进入，不由激活顺带触发。
    pub fn validate_publish(
        from: Option<LifecycleStatus>,
    ) -> Result<(), (Option<LifecycleStatus>, LifecycleStatus)> {
        match from {
            Some(LifecycleStatus::Active) => Ok(()),
            f => Err((f, LifecycleStatus::Published)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Governance, LifecycleStatus, LlmGenerated, Provenance, SourceBinding};

    fn sample_dataset() -> RuleDataset {
        RuleDataset {
            dataset_id: "ds-tax-2024".into(),
            name: "t".into(),
            description: None,
            domain: vec![],
            tags: vec![],
            tenant_id: "org".into(),
            visibility: crate::model::Visibility::Private,
            lifecycle: Default::default(),
            versioning: Default::default(),
            law_ref: None,
            version_selection: None,
            data_dependencies: Some(crate::model::DataDependencies {
                inputs: vec![],
                services: vec![crate::model::ServiceDecl {
                    service_name: "payroll_svc".into(),
                    io_contract: None,
                    sensitive: false,
                    description: None,
                    template: None,
                }],
            }),
            meta: crate::model::Meta {
                created_at: "t".into(),
                created_by: "u".into(),
                updated_at: None,
                updated_by: None,
            },
        }
    }

    fn sample_entry(rule_body: serde_json::Value) -> RuleEntry {
        RuleEntry {
            entry_id: "tax-001-rule-01".into(),
            dataset_id: "ds-tax-2024".into(),
            version: 1,
            status: Some(LifecycleStatus::Draft),
            provenance: Provenance {
                source: "s".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "tax".into(),
            tags: vec![],
            data_source_binding: vec![SourceBinding {
                rule_ref: "rule_body.transform[0].params.service_name".into(),
                service_name: "payroll_svc".into(),
            }],
            consumed_inputs: vec![],
            rule_body,
            governance: None,
        }
    }

    #[test]
    fn test_io_services_from_rule_body() {
        let rb = serde_json::json!({
            "transform": [
                {"type": "io_request", "params": {"service_name": "payroll_svc", "args": {}}},
                {"type": "set", "params": {"x": 1}}
            ]
        });
        let services = Validator::io_services_from_rule_body(&rb);
        assert_eq!(services, vec!["payroll_svc".to_string()]);
    }

    #[test]
    fn test_symbol_consistency_ok() {
        let ds = sample_dataset();
        let entry = sample_entry(serde_json::json!({
            "transform": [{"type": "io_request", "params": {"service_name": "payroll_svc"}}]
        }));
        assert!(Validator::validate_symbol_consistency(&ds, &entry).is_ok());
    }

    #[test]
    fn test_symbol_consistency_not_declared() {
        let ds = sample_dataset();
        let mut entry = sample_entry(serde_json::json!({
            "transform": [{"type": "io_request", "params": {"service_name": "other_svc"}}]
        }));
        // rule_body 有 other_svc，但数据集未声明
        entry.data_source_binding[0].service_name = "other_svc".into();
        let err = Validator::validate_symbol_consistency(&ds, &entry).unwrap_err();
        assert!(matches!(err, ValidationError::ServiceNotDeclared { .. }));
    }

    #[test]
    fn test_symbol_consistency_not_in_rule_body() {
        let ds = sample_dataset();
        let entry = sample_entry(serde_json::json!({
            "transform": [{"type": "set", "params": {"x": 1}}]
        }));
        // 数据集声明了 payroll_svc，但 rule_body 无 io_request 引用
        let err = Validator::validate_symbol_consistency(&ds, &entry).unwrap_err();
        assert!(matches!(err, ValidationError::ServiceNotInRuleBody { .. }));
    }

    #[test]
    fn test_llm_boundary() {
        let mut entry = sample_entry(serde_json::json!({}));
        entry.governance = Some(Governance {
            llm_generated: Some(LlmGenerated {
                flag: true,
                model: None,
                op: Some("draft_rule".into()),
                timestamp: None,
            }),
            ..Default::default()
        });
        // llm_generated=true + Draft → OK
        entry.status = Some(LifecycleStatus::Draft);
        assert!(Validator::validate_llm_boundary(&entry).is_ok());
        // llm_generated=true + Active → Err
        entry.status = Some(LifecycleStatus::Active);
        assert!(Validator::validate_llm_boundary(&entry).is_err());
    }

    #[test]
    fn test_transition_rules() {
        // 常规闸门
        assert!(Validator::validate_transition(Some(LifecycleStatus::Draft), LifecycleStatus::Candidate).is_ok());
        assert!(Validator::validate_transition(Some(LifecycleStatus::Candidate), LifecycleStatus::Active).is_ok());
        // Published 不能经通用迁移进入（34 号 §3 强约束）
        assert!(Validator::validate_transition(Some(LifecycleStatus::Active), LifecycleStatus::Published).is_err());
        assert!(Validator::validate_transition(Some(LifecycleStatus::Draft), LifecycleStatus::Published).is_err());
        // 驳回路径
        assert!(Validator::validate_transition(Some(LifecycleStatus::Candidate), LifecycleStatus::Rejected).is_ok());
        assert!(Validator::validate_transition(Some(LifecycleStatus::Active), LifecycleStatus::Rejected).is_ok());
        // 撤销发布 + 修订重来
        assert!(Validator::validate_transition(Some(LifecycleStatus::Published), LifecycleStatus::Rejected).is_ok());
        assert!(Validator::validate_transition(Some(LifecycleStatus::Rejected), LifecycleStatus::Draft).is_ok());
    }

    #[test]
    fn test_publish_gate() {
        // 仅 Active 可发布（独立发布审批）
        assert!(Validator::validate_publish(Some(LifecycleStatus::Active)).is_ok());
        for from in [
            Some(LifecycleStatus::Draft),
            Some(LifecycleStatus::Candidate),
            Some(LifecycleStatus::Published),
            Some(LifecycleStatus::Rejected),
            None,
        ] {
            assert!(Validator::validate_publish(from).is_err(), "from={from:?}");
        }
    }

    #[test]
    fn test_scan_credentials_heuristics() {
        // 命中真实凭据格式
        assert_eq!(scan_credentials("aws key=AKIAIOSFODNN7EXAMPLE"), ["AKIA".to_string()]);
        assert_eq!(scan_credentials("token=ghp_abcdefg12345"), ["ghp_".to_string()]);
        // 命中键名+引号包裹的 JSON 值
        assert!(scan_credentials(r#"{"api_key":"super-secret-123"}"#).contains(&"api_key=super-secret-123".to_string()));
        // 不误伤：纯占位符/示例/说明（无冒号等号键值、或值为占位符）
        assert_eq!(scan_credentials("endpoint=<host>:8080, key=<key>"), Vec::<String>::new());
        assert_eq!(scan_credentials("备注：password 应为示例占位，勿填真实值"), Vec::<String>::new());
        assert_eq!(scan_credentials(r#"{"auth": "<token 由执行侧注入>"}"#), Vec::<String>::new());
    }
}
