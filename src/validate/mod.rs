//! 校验（设计文档 §9 约束与一致性）
//!
//! 校验规则：
//! - **符号三方一致**（§9-3）：`rule_body` 的 io_request.service_name ≡ 条目 binding.service_name
//!   ≡ 数据集 data_dependencies.services，缺失**显式报错**（不静默降级）；
//! - **LLM 边界**（§9-6 / 历史批次强约束）：`llm_generated.flag=true` 的条目 status 只能是 Draft；
//! - **状态机基础**（§9-4，完整在历史批次）：5 态合法迁移（MVP 先落可达性基础）；
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
    LlmGeneratedNotDraft {
        entry: String,
        status: LifecycleStatus,
    },

    #[error("LLM 产出条目 `{entry}` 不可发布（Published 永远人工：机器闸行权上限为 Active，须人工发布审批）")]
    LlmGeneratedNotPublishable { entry: String },

    #[error("rule_body 结构无法解析 transform（需含 type=io_request 且 params.service_name）")]
    InvalidRuleBody,

    #[error("规则体含动态服务引用（__exec__.instruction.params.*）但数据集 `{dataset}` 未声明任何服务：运行所需服务必须显式声明，否则执行侧绑定核对无法覆盖（C1，不静默）")]
    DynamicRefNeedsDeclaration { dataset: String },

    #[error(
        "发布前凭据静态扫描未通过：命中疑似凭据 {hits:?}（设计文档 §6/§9-3：凭据永不入规则资产库）"
    )]
    CredentialScanFailed { hits: Vec<String> },

    /// 通用校验失败（自描述消息；供存储层零散校验点复用，避免为单一场景扩枚举）
    #[error("{0}")]
    Message(String),
}

/// 凭据静态扫描（设计文档 §6/§9-3 强约束 MVP 手段）：
/// 发布前对序列化字符串做**启发式**疑似密钥模式匹配，命中即可疑，交由发布审批人复核。
///
/// 辅助"全模型无凭据字段"的模型设计（两层防线），扫描规则保守（尽可能命中真实凭据而少误伤规则正文）：
/// - AWS 访问密钥 `AKIA<16位大写字母数字>`、GitHub `ghp_/gho_/ghs_`；
/// - 常见密钥键名（api_key/access_token/password/secret/private_key/credential 等）
///   **紧邻** `:`/`=` 且值为非空、非纯占位符的片段。
///   紧邻=键名与分隔符之间仅允许 ≤2 个空白/引号字符（O-235 修复 A：兼容 JSON
///   `"password": "x"`、YAML `password: x`、`password = x`、`password=x` 全部真实键值
///   形态；自然语言「password ...」之后任意距离出现的 :/= 不再误报——修复前任意距离
///   找分隔符会把「password authentication is active; ... port = 8888」这类正文误判为凭据）。
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
        "api_key",
        "apikey",
        "access_token",
        "auth_token",
        "refresh_token",
        "secret",
        "client_secret",
        "private_key",
        "password",
        "passwd",
        "credential",
        "credentials",
        "authorization",
        "bearer",
    ];
    let lower = text.to_ascii_lowercase();
    for name in key_names {
        let mut pos = 0usize;
        while let Some(rel) = lower[pos..].find(name) {
            let idx = pos + rel;
            let key_end = idx + name.len();
            // 紧邻约束（O-235 修复 A）：键名结束与分隔符之间仅允许 ≤2 个空白/引号字符
            // （引号=JSON 键 `"password":` 的闭合引号；全部 ASCII 单字节，字节序=字符序）
            let skip = lower[key_end..]
                .chars()
                .take_while(|c| matches!(c, ' ' | '\t' | '"' | '\''))
                .count()
                .min(2);
            let rest = &lower[key_end..][skip..];
            if rest.starts_with(':') || rest.starts_with('=') {
                // 取分隔符后到值片段（→ 到空白/逗号/右括号/引号/换行）
                let vstart = key_end + skip + 1;
                let vtrim = lower[vstart..]
                    .trim_start()
                    .trim_start_matches('"')
                    .trim_start_matches('\'');
                // 值非空、非布尔/纯数字/对象/占位符 → 疑似凭据
                let non_trivial = !vtrim.is_empty()
                    && !vtrim.starts_with("true")
                    && !vtrim.starts_with("false")
                    && !vtrim.starts_with('{')
                    && !vtrim.starts_with('[')
                    && !vtrim.starts_with('<')
                    && vtrim
                        .chars()
                        .next()
                        .map(|c| !c.is_ascii_digit())
                        .unwrap_or(false);
                if non_trivial {
                    let val: String = vtrim
                        .split([',', '"', '\'', ')', '}', '\n', ' '])
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(['{', ','])
                        .to_string();
                    // 占位符/示例/说明值不算命中（避免误伤规则正文）
                    if !val.is_empty()
                        && !val.contains("提示")
                        && !val.contains("示例")
                        && !val.contains("占位")
                        && !val.contains("PLACEHOLDER")
                        && !val.contains("...")
                        && !val.contains('<')
                        && !val.contains('{')
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

/// 机器闸放行上下文（机器闸行权通路）：来自待落链的状态变更事实
/// （StateChange.gate/tier 字段；审计回查锚=state_change_id）。
/// `None` 闸上下文 = 无机器闸证据，行为与历史版本逐字节一致。
#[derive(Debug, Clone)]
pub struct MachineGateContext {
    /// 机器闸梯位：`T0` 直通 / `T1` 事后追认（T2 走人工，不入此上下文）
    pub tier: String,
    /// 机器闸六项检查结论（执行器纯函数产出；本层只消费通过与否）
    pub checks_passed: bool,
    /// 对应状态变更事实的审计回查锚
    pub state_change_id: String,
}

impl MachineGateContext {
    /// 闸证据有效性：机器梯位（T0/T1）且六检通过
    pub fn is_valid_machine_evidence(&self) -> bool {
        matches!(self.tier.as_str(), "T0" | "T1") && self.checks_passed
    }
}

/// 校验器（纯函数，无状态）
pub struct Validator;

impl Validator {
    /// 从 rule_body 提取所有 `io_request.params.service_name`（锚定 10_role13_demo.json 结构）。
    /// **T1 决策（2026-08-24）**：符号提取唯一来源已迁至 `evorule-bundle`（SSOT），本函数薄包装转发。
    pub fn io_services_from_rule_body(rule_body: &serde_json::Value) -> Vec<String> {
        evorule_bundle::symbols::io_services_from_rule_body(rule_body)
    }

    /// 符号三方一致校验（§9-3）：rule_body ≡ binding ≡ dataset.data_dependencies
    pub fn validate_symbol_consistency(
        dataset: &RuleDataset,
        entry: &RuleEntry,
    ) -> Result<(), ValidationError> {
        let rule_body_services = Self::io_services_from_rule_body(&entry.rule_body);
        let declared_services = dataset
            .data_dependencies
            .as_ref()
            .map(|d| &d.services)
            .cloned()
            .unwrap_or_default();

        // C1（02 方案层1）：动态服务引用（__exec__.instruction.params.*）运行时才解析，
        // 无法静态核对 → 强制要求数据集显式声明服务（声明非空），否则执行侧绑定核对
        // 覆盖不到运行所需服务（不静默）。先于空绑定早退判断，确保动态引用规则也必须声明。
        let has_dynamic = evorule_bundle::symbols::has_dynamic_service_ref(&entry.rule_body);
        if has_dynamic && declared_services.is_empty() {
            return Err(ValidationError::DynamicRefNeedsDeclaration {
                dataset: dataset.dataset_id.clone(),
            });
        }

        // 1) 无绑定条目直接通过（无外部数据的规则）
        if entry.data_source_binding.is_empty() {
            return Ok(());
        }

        for binding in &entry.data_source_binding {
            // a) 必须出现在数据集声明中
            if !declared_services
                .iter()
                .any(|s| s.service_name == binding.service_name)
            {
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

    /// LLM 边界（§9-6 / 历史批次）：llm_generated=true → status 只能是 Draft
    ///
    /// 兼容包装：等价于 [`Self::validate_llm_boundary_gated`] 传 `None` 闸上下文
    /// （无机器闸证据 → 历史行为逐字节保持）。
    pub fn validate_llm_boundary(entry: &RuleEntry) -> Result<(), ValidationError> {
        let is_llm = entry
            .governance
            .as_ref()
            .map(|g| g.is_llm_generated())
            .unwrap_or(false);
        let status = entry.status.unwrap_or(LifecycleStatus::Draft);
        Self::validate_llm_boundary_gated(is_llm, &entry.entry_id, &status, None)
    }

    /// LLM 边界·通用判定（机器闸行权通路）：
    ///
    /// - 非 llm_generated → 放行；
    /// - 目标 `Published` → [`ValidationError::LlmGeneratedNotPublishable`]
    ///   （**Published 永远人工**，机器闸行权上限=Active，结构性立宪不松动）；
    /// - 有有效机器闸证据（gate 上下文：T0/T1 且六检通过）且目标 ∈
    ///   {Draft, Candidate, Active} → 放行；
    /// - 目标 `Rejected`（降权终态，人工终审 reject 腿）→ 放行（无需闸证据）；
    /// - 其余（无闸上下文的晋升向）→ [`ValidationError::LlmGeneratedNotDraft`]
    ///   （历史行为保持，向后兼容）。
    pub fn validate_llm_boundary_gated(
        is_llm: bool,
        entry_id: &str,
        status: &LifecycleStatus,
        gate: Option<&MachineGateContext>,
    ) -> Result<(), ValidationError> {
        if !is_llm {
            return Ok(());
        }
        if *status == LifecycleStatus::Published {
            return Err(ValidationError::LlmGeneratedNotPublishable {
                entry: entry_id.to_string(),
            });
        }
        let machine_allowed = gate.map(|g| g.is_valid_machine_evidence()).unwrap_or(false);
        // Rejected 为降权终态（人工终审 reject 腿）：无闸证据亦放行——反升级立宪
        // 只约束晋升向（Candidate/Active 需机器闸证据）与 Published（永远人工），
        // 拒绝向放行不削弱既有保护（llm 候选的 T2 终审须 approve/reject 双腿齐备）。
        if machine_allowed
            || *status == LifecycleStatus::Draft
            || *status == LifecycleStatus::Rejected
        {
            return Ok(());
        }
        Err(ValidationError::LlmGeneratedNotDraft {
            entry: entry_id.to_string(),
            status: *status,
        })
    }

    /// 状态机合法迁移（设计文档 §2）。返回 Err(Some(from,to)) 表示非法迁移。
    ///
    /// **不含 `Active → Published`**：Published 只能经独立发布审批（`validate_publish` +
    /// `publish_dataset`）显式进入，不能由通用状态迁移顺带完成（设计文档 §3 强约束）。
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
                // 撤销发布（设计文档 §2；审批细节为开放点③）
                | (LifecycleStatus::Published, LifecycleStatus::Rejected)
                // 修订重来（设计文档 §8-3，Rejected 非终态）
                | (LifecycleStatus::Rejected, LifecycleStatus::Draft)
        );
        if ok {
            Ok(())
        } else {
            Err((Some(from), to))
        }
    }

    /// 独立发布审批前置（设计文档 §3 强约束）：**仅 Active 可发布**。
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
    // ===== I15 机器闸可复算演练：闸裁决矩阵双跑一致 =====

    #[test]
    fn i15_gate_verdict_matrix_double_run_identical() {
        use crate::model::lifecycle::LifecycleStatus;
        // 输入矩阵:is_llm × 状态 × 闸证据形态(全枚举)
        let statuses = [
            LifecycleStatus::Draft,
            LifecycleStatus::Candidate,
            LifecycleStatus::Active,
            LifecycleStatus::Published,
            LifecycleStatus::Rejected,
        ];
        let gates: Vec<Option<MachineGateContext>> = vec![
            None,
            Some(MachineGateContext {
                tier: "T0".into(),
                checks_passed: true,
                state_change_id: "sc-1".into(),
            }),
            Some(MachineGateContext {
                tier: "T1".into(),
                checks_passed: true,
                state_change_id: "sc-2".into(),
            }),
            Some(MachineGateContext {
                tier: "T1".into(),
                checks_passed: false,
                state_change_id: "sc-3".into(),
            }),
            Some(MachineGateContext {
                tier: "T2".into(),
                checks_passed: true,
                state_change_id: "sc-4".into(),
            }),
        ];
        let verdict =
            |is_llm: bool, status: &LifecycleStatus, gate: Option<&MachineGateContext>| {
                Validator::validate_llm_boundary_gated(is_llm, "e1", status, gate).is_ok()
            };
        // 双跑全矩阵:逐格一致(可复算),并把预期语义钉死(非快照漂移)
        for run in 1..=2 {
            for is_llm in [true, false] {
                for status in &statuses {
                    for gate in &gates {
                        let v = verdict(is_llm, status, gate.as_ref());
                        let v2 = verdict(is_llm, status, gate.as_ref());
                        assert_eq!(v, v2, "双跑必须一致(run={run})");
                        // 语义钉死:Published 永拒(llm);晋升向无有效机器证据即拒;
                        // Rejected 例外(降权终态,人工终审 reject 腿,无需闸证据)
                        if is_llm && *status == LifecycleStatus::Published {
                            assert!(!v, "llm 条目 Published 永拒");
                        }
                        if is_llm
                            && *status != LifecycleStatus::Draft
                            && *status != LifecycleStatus::Rejected
                            && gate
                                .as_ref()
                                .map(|g| !g.is_valid_machine_evidence())
                                .unwrap_or(true)
                        {
                            assert!(!v, "无有效机器证据的非 Draft llm 迁移必拒");
                        }
                    }
                }
            }
        }
        // 证据权重边界:checks_passed=false 或 T2 梯位=无效证据(与纯函数口径一致)
        assert!(!MachineGateContext {
            tier: "T1".into(),
            checks_passed: false,
            state_change_id: "x".into()
        }
        .is_valid_machine_evidence());
        assert!(!MachineGateContext {
            tier: "T2".into(),
            checks_passed: true,
            state_change_id: "x".into()
        }
        .is_valid_machine_evidence());
    }

    use super::*;
    use crate::model::{Governance, LifecycleStatus, LlmGenerated, Provenance, SourceBinding};

    // ===== 机器闸行权通路（LLM 边界通用判定）测试四组 =====

    fn gate_ctx(tier: &str, passed: bool) -> MachineGateContext {
        MachineGateContext {
            tier: tier.to_string(),
            checks_passed: passed,
            state_change_id: "sc-test".into(),
        }
    }

    #[test]
    fn test_gate_allows_machine_evidence_to_candidate_and_active() {
        // 正向：llm_generated + 机器闸证据（T0 全过）→ Candidate/Active 放行
        for target in [LifecycleStatus::Candidate, LifecycleStatus::Active] {
            let r = Validator::validate_llm_boundary_gated(
                true,
                "e1",
                &target,
                Some(&gate_ctx("T0", true)),
            );
            assert!(
                r.is_ok(),
                "{target:?} should pass with machine gate evidence"
            );
        }
        // T1 同样放行
        assert!(Validator::validate_llm_boundary_gated(
            true,
            "e1",
            &LifecycleStatus::Active,
            Some(&gate_ctx("T1", true)),
        )
        .is_ok());
    }

    #[test]
    fn test_gate_rejects_without_evidence() {
        // 负向·无闸：无机器闸证据 → 历史行为逐字节保持（只能 Draft）
        for target in [LifecycleStatus::Candidate, LifecycleStatus::Active] {
            let r = Validator::validate_llm_boundary_gated(true, "e1", &target, None);
            assert!(matches!(
                r,
                Err(ValidationError::LlmGeneratedNotDraft { .. })
            ));
        }
        // 检查未全过 = 无有效证据
        assert!(matches!(
            Validator::validate_llm_boundary_gated(
                true,
                "e1",
                &LifecycleStatus::Active,
                Some(&gate_ctx("T0", false)),
            ),
            Err(ValidationError::LlmGeneratedNotDraft { .. })
        ));
        // 非法梯位字面量 = 无有效证据
        assert!(matches!(
            Validator::validate_llm_boundary_gated(
                true,
                "e1",
                &LifecycleStatus::Active,
                Some(&gate_ctx("T9", true)),
            ),
            Err(ValidationError::LlmGeneratedNotDraft { .. })
        ));
    }

    #[test]
    fn test_published_always_human() {
        // 负向·Published：任何 gate 上下文（含 T0 全过）都硬拒——结构性立宪
        for gate in [None, Some(gate_ctx("T0", true))] {
            let r = Validator::validate_llm_boundary_gated(
                true,
                "e1",
                &LifecycleStatus::Published,
                gate.as_ref(),
            );
            assert!(matches!(
                r,
                Err(ValidationError::LlmGeneratedNotPublishable { .. })
            ));
        }
    }

    #[test]
    fn test_non_llm_unaffected() {
        // 兼容：非 llm_generated 全路径行为不变（含 Published 直传场景——
        // 真实 Published 入口另有 validate_publish 人工审批，此处仅测本函数语义）
        assert!(Validator::validate_llm_boundary_gated(
            false,
            "e1",
            &LifecycleStatus::Published,
            None,
        )
        .is_ok());
        assert!(Validator::validate_llm_boundary_gated(
            false,
            "e1",
            &LifecycleStatus::Active,
            Some(&gate_ctx("T0", true)),
        )
        .is_ok());
    }

    fn sample_dataset() -> RuleDataset {
        RuleDataset {
            dataset_id: "ds-tax-2024".into(),
            name: "t".into(),
            description: None,
            dataset_kind: Default::default(),
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
                    version: None,
                    io_contract: None,
                    sensitive: false,
                    description: None,
                    template: None,
                }],
            }),
            event_schemas: vec![],
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
                {"type": "io_request", "params": {"io_type": "call_service", "service_name": "payroll_svc", "args": {}}},
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
            "transform": [{"type": "io_request", "params": {"io_type": "call_service", "service_name": "payroll_svc"}}]
        }));
        assert!(Validator::validate_symbol_consistency(&ds, &entry).is_ok());
    }

    #[test]
    fn test_symbol_consistency_not_declared() {
        let ds = sample_dataset();
        let mut entry = sample_entry(serde_json::json!({
            "transform": [{"type": "io_request", "params": {"io_type": "call_service", "service_name": "other_svc"}}]
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
    fn test_dynamic_ref_requires_declared_services() {
        // C1：规则体含动态服务引用 + 数据集未声明任何服务 → 显式拒绝（不静默）
        let mut ds = sample_dataset();
        ds.data_dependencies = Some(crate::model::DataDependencies {
            inputs: vec![],
            services: vec![],
        });
        let entry = sample_entry(serde_json::json!({
            "transform": [{
                "type": "io_request",
                "params": {"io_type": "call_service", "service_name": "__exec__.instruction.params.service_name"}
            }]
        }));
        let err = Validator::validate_symbol_consistency(&ds, &entry).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::DynamicRefNeedsDeclaration { .. }
        ));

        // 声明非空 → 通过（即使无绑定条目，动态引用也可入库）
        // 动态服务引用规则无法静态绑定（运行时才解析），故 data_source_binding 为空
        let mut entry = entry;
        entry.data_source_binding = vec![];
        ds.data_dependencies = Some(crate::model::DataDependencies {
            inputs: vec![],
            services: vec![crate::model::ServiceDecl {
                service_name: "inverse_kinematics_solver".into(),
                version: None,
                io_contract: None,
                sensitive: false,
                description: None,
                template: None,
            }],
        });
        assert!(Validator::validate_symbol_consistency(&ds, &entry).is_ok());
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
        assert!(Validator::validate_transition(
            Some(LifecycleStatus::Draft),
            LifecycleStatus::Candidate
        )
        .is_ok());
        assert!(Validator::validate_transition(
            Some(LifecycleStatus::Candidate),
            LifecycleStatus::Active
        )
        .is_ok());
        // Published 不能经通用迁移进入（设计文档 §3 强约束）
        assert!(Validator::validate_transition(
            Some(LifecycleStatus::Active),
            LifecycleStatus::Published
        )
        .is_err());
        assert!(Validator::validate_transition(
            Some(LifecycleStatus::Draft),
            LifecycleStatus::Published
        )
        .is_err());
        // 驳回路径
        assert!(Validator::validate_transition(
            Some(LifecycleStatus::Candidate),
            LifecycleStatus::Rejected
        )
        .is_ok());
        assert!(Validator::validate_transition(
            Some(LifecycleStatus::Active),
            LifecycleStatus::Rejected
        )
        .is_ok());
        // 撤销发布 + 修订重来
        assert!(Validator::validate_transition(
            Some(LifecycleStatus::Published),
            LifecycleStatus::Rejected
        )
        .is_ok());
        assert!(Validator::validate_transition(
            Some(LifecycleStatus::Rejected),
            LifecycleStatus::Draft
        )
        .is_ok());
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
        assert_eq!(
            scan_credentials("aws key=AKIAIOSFODNN7EXAMPLE"),
            ["AKIA".to_string()]
        );
        assert_eq!(
            scan_credentials("token=ghp_abcdefg12345"),
            ["ghp_".to_string()]
        );
        // 命中键名+引号包裹的 JSON 值
        assert!(scan_credentials(r#"{"api_key":"super-secret-123"}"#)
            .contains(&"api_key=super-secret-123".to_string()));
        // 不误伤：纯占位符/示例/说明（无冒号等号键值、或值为占位符）
        assert_eq!(
            scan_credentials("endpoint=<host>:8080, key=<key>"),
            Vec::<String>::new()
        );
        assert_eq!(
            scan_credentials("备注：password 应为示例占位，勿填真实值"),
            Vec::<String>::new()
        );
        assert_eq!(
            scan_credentials(r#"{"auth": "<token 由执行侧注入>"}"#),
            Vec::<String>::new()
        );
    }

    #[test]
    fn test_scan_credentials_adjacency() {
        // O-235 修复 A：紧邻约束——键名之后任意距离出现的 :/= 不再误报
        // （jupyter-notebook-server 锚立法实测误报形态入库为测试素材）
        assert_eq!(
            scan_credentials(
                "server remains running; password authentication is active; endpoint: https://host"
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            scan_credentials("password/token prompt; verify login page returned"),
            Vec::<String>::new()
        );
        assert_eq!(
            scan_credentials("config contains 'c.NotebookApp.password' (hashed) and certfile"),
            Vec::<String>::new()
        );
        assert_eq!(
            scan_credentials(
                "using the configured password and confirm the response includes a valid token"
            ),
            Vec::<String>::new()
        );
        // 紧邻真实键值形态仍必命中（检出不降级，fail-fast 语义不变）
        assert!(scan_credentials("password: benchmarkpass")
            .contains(&"password=benchmarkpass".to_string()));
        assert!(scan_credentials(r#"password = "s3cret""#).contains(&"password=s3cret".to_string()));
        assert!(scan_credentials("db.password=hunter2").contains(&"password=hunter2".to_string()));
        assert!(scan_credentials(r#"{"access_token": "ya29.abcd1234"}"#)
            .contains(&"access_token=ya29.abcd1234".to_string()));
        assert!(
            scan_credentials("api_key: abcd1234efgh").contains(&"api_key=abcd1234efgh".to_string())
        );
    }
}
