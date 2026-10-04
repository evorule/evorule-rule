//! 机器闸执行器（六检确定性纯函数）——机器闸行权通路的闸强度承载层
//!
//! 定位（机器闸行权通路实施设计 §六检从宽首版口径）：
//! - 六检全为确定性纯函数（无 IO、无时钟、无随机）——同输入同结论，可回放可复算；
//! - M1 复用既有校验 SSOT（rule=符号三方一致；knowledge=入账契约完整性）；
//! - M2 黄金样本回归以「快照连续性」替代首版（样本复算待执行引擎接入，随机器闸演进收紧）；
//! - M3 词法域冲突（同数据集同内容 Active=重复提案拒绝；同 domain 重叠=警告不拦截）；
//! - M4/M5 影响面与信誉只做梯位降级（超阈→T1 事后追认），默认通过档；
//! - 梯位裁决：全过且低影响→T0（直通）；全过→T1（事后追认）；任一不过→T2（拒绝机器放行）。
//!
//! 执行器只产报告不落库：写入路径（store 机器闸迁移方法 + machine-gate-promote 端点）
//! 消费报告并在同事务落链（先校验后落链，防先放行后补票）。报告全字段以摘要形态
//! 进 StateChange.cause，审计链可回查。

use serde::Serialize;

use crate::model::dataset::RuleDataset;
use crate::model::entry::RuleEntry;
use crate::model::knowledge::KnowledgeEntry;
use crate::validate::Validator;

/// 六项检查名（M1-M6，对齐机器闸施工面命名）
pub const CHECK_NAMES: [&str; 6] = [
    "M1_schema",
    "M2_snapshot_continuity",
    "M3_lexical_conflict",
    "M4_impact_radius",
    "M5_llm_reputation",
    "M6_rollback_path",
];

/// 从宽首版阈值（常数显式声明；随机器闸演进收紧，改动须随治理批）
pub mod thresholds {
    /// M4：数据集条目总数超过此值 → 高影响降 T2
    pub const MAX_DATASET_ENTRIES_T0: u64 = 50;
    /// M4：条目服务绑定数超过此值 → 高影响降 T2
    pub const MAX_SERVICE_BINDINGS_T0: usize = 3;
    /// M5：LLM 操作失败率超过此值（且样本充分）→ 降 T2
    pub const LLM_FAIL_RATE: f64 = 0.5;
    /// M5：失败率判据生效的最小样本数（小样本不判信誉）
    pub const LLM_MIN_SAMPLES: u64 = 4;
}

/// 单项检查结论
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckItem {
    /// 检查名（M1-M6）
    pub name: &'static str,
    /// 通过与否（false = 检不过 → 梯位 T2 = 拒绝机器放行，走人工）
    pub passed: bool,
    /// 结论注记（审计回查用；降级/警告语义也在注记内声明）
    pub note: String,
}

/// 机器闸报告（六检结论 + 梯位裁决）
#[derive(Debug, Clone, Serialize)]
pub struct MachineGateReport {
    pub checks: Vec<CheckItem>,
    /// 梯位：T0 直通 / T1 事后追认 / T2 拒绝机器放行（走人工）
    pub tier: String,
}

impl MachineGateReport {
    /// 六检是否全过
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    /// 报告摘要（追加进 StateChange.cause 的审计注记形态）
    pub fn summary(&self) -> String {
        let items: Vec<String> = self
            .checks
            .iter()
            .map(|c| format!("{}={}", c.name, if c.passed { "pass" } else { "fail" }))
            .collect();
        format!("tier={} {}", self.tier, items.join(","))
    }
}

/// 执行器输入探针（存储层现场采集 M2-M6 所需事实；执行器本体保持纯函数）
#[derive(Debug, Clone)]
pub struct GateProbe {
    /// 数据集条目总数（去重 entry_id 口径；M4）
    pub dataset_entry_count: u64,
    /// 同数据集同内容 Active 条目数（去重 entry_id，不含自身；M3 重复提案）
    pub active_same_hash: u64,
    /// 同数据集同 domain Active 条目数（去重 entry_id，不含自身；M3 词法域重叠警告）
    pub active_same_domain: u64,
    /// 上一版本条目行的内容哈希（None = 首版本或上一版本行缺失；M2/M6）
    pub prev_version_hash: Option<String>,
    /// 上一版本内容寻址快照是否在位（None = 无前序版本可比；M2）
    pub prev_snapshot_exists: Option<bool>,
}

/// 执行器输入（由调用方从存储层现场组装；本模块不做任何 IO）
pub struct GateInput {
    pub entry_id: String,
    pub version: u32,
    /// rule | knowledge 分流标记
    pub is_rule: bool,
    /// 数据集（M1 符号一致 / M4 影响面）
    pub dataset: RuleDataset,
    /// rule 条目（M1 = 符号三方一致；M4 = 服务绑定数）
    pub rule_entry: Option<RuleEntry>,
    /// knowledge 条目（M1 = 入账契约完整性）
    pub knowledge_entry: Option<KnowledgeEntry>,
    /// 存储层探针（M2/M3/M4 事实）
    pub probe: GateProbe,
    /// LLM 操作审计总量（M5）
    pub llm_total: u64,
    /// LLM 操作审计失败量（M5）
    pub llm_failed: u64,
}

/// 六检执行 + 梯位裁决（确定性纯函数）
pub fn evaluate(input: &GateInput) -> MachineGateReport {
    let mut checks: Vec<CheckItem> = Vec::with_capacity(6);
    let mut downgrade = false;

    // M1 schema 校验（复用既有校验 SSOT）
    let m1 = if input.is_rule {
        match (&input.dataset, &input.rule_entry) {
            (ds, Some(e)) => match Validator::validate_symbol_consistency(ds, e) {
                Ok(()) => (true, "符号三方一致通过".to_string()),
                Err(err) => (false, format!("符号三方一致未通过: {err}")),
            },
            _ => (false, "rule 条目载荷缺失（执行器输入不完整）".to_string()),
        }
    } else {
        match &input.knowledge_entry {
            Some(e) => match e.validate_ingest_contract() {
                Ok(()) => (true, "入账契约完整（kind/trust/license/contract）".to_string()),
                Err(reason) => (false, format!("入账契约不完整: {reason}")),
            },
            None => (false, "knowledge 条目载荷缺失（执行器输入不完整）".to_string()),
        }
    };
    checks.push(item(CHECK_NAMES[0], m1.0, m1.1));

    // M2 黄金样本（快照连续性替代首版）
    if input.version == 1 {
        checks.push(item(
            CHECK_NAMES[1],
            true,
            "首版本：无前序版本，快照连续性不适用（样本复算待执行引擎接入，随机器闸演进收紧）".into(),
        ));
    } else {
        match (&input.probe.prev_version_hash, input.probe.prev_snapshot_exists) {
            (Some(hash), Some(true)) => checks.push(item(
                CHECK_NAMES[1],
                true,
                format!(
                    "上一版本 v{} 内容快照在案（{hash}），连续性成立",
                    input.version - 1
                ),
            )),
            (Some(_), _) => checks.push(item(
                CHECK_NAMES[1],
                false,
                format!(
                    "上一版本 v{} 内容寻址快照缺失，连续性不成立",
                    input.version - 1
                ),
            )),
            (None, _) => checks.push(item(
                CHECK_NAMES[1],
                false,
                format!(
                    "上一版本 v{} 条目行缺失，连续性不可判定",
                    input.version - 1
                ),
            )),
        }
    }

    // M3 词法域冲突
    if input.probe.active_same_hash > 0 {
        checks.push(item(
            CHECK_NAMES[2],
            false,
            format!(
                "同数据集存在 {} 条相同内容 Active 条目（重复提案）",
                input.probe.active_same_hash
            ),
        ));
    } else if input.probe.active_same_domain > 0 {
        checks.push(item(
            CHECK_NAMES[2],
            true,
            format!(
                "同 domain 已有 {} 条 Active 条目（词法域重叠警告，不拦截）",
                input.probe.active_same_domain
            ),
        ));
    } else {
        checks.push(item(
            CHECK_NAMES[2],
            true,
            "无同内容/同域 Active 条目".into(),
        ));
    }

    // M4 影响面（只降级不拒绝）
    let binding_count = input
        .rule_entry
        .as_ref()
        .map(|e| e.data_source_binding.len())
        .unwrap_or(0);
    let mut m4_notes: Vec<String> = Vec::new();
    if input.probe.dataset_entry_count > thresholds::MAX_DATASET_ENTRIES_T0 {
        downgrade = true;
        m4_notes.push(format!(
            "数据集条目总数 {} > {}（高影响降梯位）",
            input.probe.dataset_entry_count,
            thresholds::MAX_DATASET_ENTRIES_T0
        ));
    }
    if binding_count > thresholds::MAX_SERVICE_BINDINGS_T0 {
        downgrade = true;
        m4_notes.push(format!(
                "服务绑定数 {binding_count} > {}（高影响降梯位）",
                thresholds::MAX_SERVICE_BINDINGS_T0
            ));
    }
    checks.push(item(
        CHECK_NAMES[3],
        true,
        if m4_notes.is_empty() {
            format!(
                "低影响（数据集条目 {}，服务绑定 {binding_count}）",
                input.probe.dataset_entry_count
            )
        } else {
            m4_notes.join("；")
        },
    ));

    // M5 信誉（默认通过档；只降级不拒绝）
    let fail_rate = if input.llm_total > 0 {
        input.llm_failed as f64 / input.llm_total as f64
    } else {
        0.0
    };
    let rate_note = format!(
        "LLM 操作失败率 {:.0}%（{}/{}）",
        fail_rate * 100.0,
        input.llm_failed,
        input.llm_total
    );
    if input.llm_total >= thresholds::LLM_MIN_SAMPLES && fail_rate > thresholds::LLM_FAIL_RATE {
        downgrade = true;
        checks.push(item(
            CHECK_NAMES[4],
            true,
            format!("{rate_note} 超阈 → 降 T2"),
        ));
    } else {
        checks.push(item(CHECK_NAMES[4], true, format!("{rate_note}（默认通过档）")));
    }

    // M6 可回滚
    if input.version > 1 {
        checks.push(item(
            CHECK_NAMES[5],
            input.probe.prev_version_hash.is_some(),
            if input.probe.prev_version_hash.is_some() {
                format!("上一版本 v{} 行在案，可回退重审", input.version - 1)
            } else {
                format!("上一版本 v{} 行缺失，回退锚点缺失", input.version - 1)
            },
        ));
    } else {
        checks.push(item(
            CHECK_NAMES[5],
            true,
            "首版本：结构性回退路径 = Active→Rejected 撤销（迁移表在位）".into(),
        ));
    }

    let tier = if !checks.iter().all(|c| c.passed) {
        "T2"
    } else if downgrade {
        "T1"
    } else {
        "T0"
    };
    MachineGateReport {
        checks,
        tier: tier.to_string(),
    }
}

fn item(name: &'static str, passed: bool, note: String) -> CheckItem {
    CheckItem { name, passed, note }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::lifecycle::LifecycleStatus;
    use crate::model::{DataDependencies, Provenance, ServiceDecl, SourceBinding, Visibility};

    fn sample_dataset() -> RuleDataset {
        RuleDataset {
            dataset_id: "ds-tax-2024".into(),
            name: "税务合规".into(),
            description: None,
            dataset_kind: Default::default(),
            domain: vec!["tax".into()],
            tags: vec![],
            tenant_id: "org-evorule".into(),
            visibility: Visibility::Private,
            lifecycle: Default::default(),
            versioning: Default::default(),
            law_ref: None,
            version_selection: None,
            data_dependencies: Some(DataDependencies {
                inputs: vec![],
                services: vec![ServiceDecl {
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

    fn sample_rule_entry() -> RuleEntry {
        RuleEntry {
            entry_id: "tax-001".into(),
            dataset_id: "ds-tax-2024".into(),
            version: 1,
            status: Some(LifecycleStatus::Draft),
            provenance: Provenance {
                source: "《企业所得税法》".into(),
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
            rule_body: serde_json::json!({
                "transform": [{"type": "io_request", "params": {"io_type": "call_service", "service_name": "payroll_svc"}}]
            }),
            governance: None,
        }
    }

    fn sample_knowledge_entry() -> KnowledgeEntry {
        KnowledgeEntry {
            entry_id: "kb-001".into(),
            dataset_id: "ds-tax-2024".into(),
            version: 1,
            status: Some(LifecycleStatus::Draft),
            provenance: Provenance {
                source: "《企业所得税法》".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "tax".into(),
            tags: vec![],
            payload: serde_json::json!({"rate": 0.25}),
            schema_ref: "domain:tax#rate".into(),
            governance: None,
            knowledge_kind: None,
            trust_level: None,
            license_ref: None,
            execution_contract: None,
        }
    }

    fn probe_v1() -> GateProbe {
        GateProbe {
            dataset_entry_count: 1,
            active_same_hash: 0,
            active_same_domain: 0,
            prev_version_hash: None,
            prev_snapshot_exists: None,
        }
    }

    fn gate_input(ds: RuleDataset, entry: RuleEntry, probe: GateProbe) -> GateInput {
        let is_rule = true;
        GateInput {
            entry_id: entry.entry_id.clone(),
            version: entry.version,
            is_rule,
            dataset: ds,
            rule_entry: Some(entry),
            knowledge_entry: None,
            probe,
            llm_total: 0,
            llm_failed: 0,
        }
    }

    #[test]
    fn test_tier_t0_all_pass_low_impact() {
        // 全过 + 低影响 → T0 直通
        let report = evaluate(&gate_input(sample_dataset(), sample_rule_entry(), probe_v1()));
        assert_eq!(report.tier, "T0");
        assert!(report.all_passed());
        assert_eq!(report.checks.len(), 6);
    }

    #[test]
    fn test_tier_t1_high_impact_downgrades() {
        // M4 数据集条目总数超阈 → 全过但降 T1
        let mut probe = probe_v1();
        probe.dataset_entry_count = thresholds::MAX_DATASET_ENTRIES_T0 + 1;
        let report = evaluate(&gate_input(sample_dataset(), sample_rule_entry(), probe));
        assert_eq!(report.tier, "T1");
        assert!(report.all_passed());
    }

    #[test]
    fn test_tier_t1_llm_fail_rate_downgrades() {
        // M5 失败率超阈（样本充分）→ 降 T1
        let mut input = gate_input(sample_dataset(), sample_rule_entry(), probe_v1());
        input.llm_total = thresholds::LLM_MIN_SAMPLES;
        input.llm_failed = 3; // 75% > 50%
        let report = evaluate(&input);
        assert_eq!(report.tier, "T1");
        assert!(report.all_passed());
        // 小样本不判信誉：total=3 failed=3 → 仍 T0
        let mut small = gate_input(sample_dataset(), sample_rule_entry(), probe_v1());
        small.llm_total = 3;
        small.llm_failed = 3;
        assert_eq!(evaluate(&small).tier, "T0");
    }

    #[test]
    fn test_tier_t2_duplicate_content_rejected() {
        // M3 同内容 Active 条目在场 = 重复提案 → 拒绝机器放行
        let mut probe = probe_v1();
        probe.active_same_hash = 1;
        let report = evaluate(&gate_input(sample_dataset(), sample_rule_entry(), probe));
        assert_eq!(report.tier, "T2");
        assert!(!report.all_passed());
        let m3 = report
            .checks
            .iter()
            .find(|c| c.name == CHECK_NAMES[2])
            .unwrap();
        assert!(!m3.passed);
    }

    #[test]
    fn test_tier_t2_m1_rule_symbol_failure() {
        // M1 符号三方一致失败 → 拒绝
        let mut entry = sample_rule_entry();
        entry.rule_body = serde_json::json!({
            "transform": [{"type": "set", "params": {"x": 1}}]
        });
        let report = evaluate(&gate_input(sample_dataset(), entry, probe_v1()));
        assert_eq!(report.tier, "T2");
    }

    #[test]
    fn test_tier_t2_m2_prev_snapshot_missing() {
        // M2 上一版本快照缺失 → 拒绝
        let mut entry = sample_rule_entry();
        entry.version = 2;
        let mut probe = probe_v1();
        probe.prev_version_hash = Some("sha3:abc".into());
        probe.prev_snapshot_exists = Some(false);
        let report = evaluate(&gate_input(sample_dataset(), entry, probe));
        assert_eq!(report.tier, "T2");
        let m2 = report
            .checks
            .iter()
            .find(|c| c.name == CHECK_NAMES[1])
            .unwrap();
        assert!(!m2.passed);
    }

    #[test]
    fn test_tier_t2_m6_prev_row_missing() {
        // M6 上一版本行缺失 → 回退锚点缺失 → 拒绝
        let mut entry = sample_rule_entry();
        entry.version = 3;
        let mut probe = probe_v1();
        probe.prev_version_hash = None;
        probe.prev_snapshot_exists = None;
        let report = evaluate(&gate_input(sample_dataset(), entry, probe));
        assert_eq!(report.tier, "T2");
    }

    #[test]
    fn test_tier_t2_knowledge_contract_failure() {
        // M1 knowledge 入账契约失败（非法 kind）→ 拒绝
        let mut kb = sample_knowledge_entry();
        kb.knowledge_kind = Some("bogus".into());
        let input = GateInput {
            entry_id: kb.entry_id.clone(),
            version: kb.version,
            is_rule: false,
            dataset: sample_dataset(),
            rule_entry: None,
            knowledge_entry: Some(kb),
            probe: probe_v1(),
            llm_total: 0,
            llm_failed: 0,
        };
        let report = evaluate(&input);
        assert_eq!(report.tier, "T2");
        let m1 = report
            .checks
            .iter()
            .find(|c| c.name == CHECK_NAMES[0])
            .unwrap();
        assert!(!m1.passed);
    }

    #[test]
    fn test_summary_contains_tier_and_checks() {
        let report = evaluate(&gate_input(sample_dataset(), sample_rule_entry(), probe_v1()));
        let s = report.summary();
        assert!(s.starts_with("tier=T0"));
        assert!(s.contains("M1_schema=pass"));
        assert!(s.contains("M6_rollback_path=pass"));
    }
}
