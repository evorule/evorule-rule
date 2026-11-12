//! 提案验收闸（LLM 提案 → 机器闸报告；发布链接线件）
//!
//! 复用加固①闸门统一成果（共享校验面+否定构造双仓锁定扫描）：
//! 每行提案过机器可判面（结构九要素/公式分离符/否定构造+时间锚），
//! 逐行出 verdict 报告（JSONL），批级汇总 pass/reject 计数。
//! 拒收走 side-list 留痕（吸收原则 3/6），不静默丢弃。
//!
//! L2 真值面口径（可计算子集裁定）：量纲一致性/单位冒烟=server 分布消费面
//! （core/law_schema unit engine）；rule 侧闸=结构+否定构造（本模块）。

use crate::validate::{scan_negation_constructs, Validator};
use serde_json::{json, Value};

/// 单行闸判定
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalGateItem {
    /// 提案 JSONL 行号（1 起）
    pub line: usize,
    /// 条目 id（可提取时；解析失败为 "<no-id>"）
    pub entry_id: String,
    /// verdict: pass | reject
    pub verdict: String,
    /// 分项检查名（通过项；拒收时为首个失败项名）
    pub checks: Vec<String>,
    /// 拒收原因（pass 时为空）
    pub reason: Option<String>,
}

/// 批级闸汇总
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalGateSummary {
    pub total: usize,
    pub passed: usize,
    pub rejected: usize,
}

/// 提案批闸报告
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalGateReport {
    pub items: Vec<ProposalGateItem>,
    pub summary: ProposalGateSummary,
}

/// 从一行提案提取 law 载荷：裸 law 载荷或 {law:{...}} 信封均接受
fn law_payload_from_line(line_value: &Value) -> Result<&Value, String> {
    if let Some(law) = line_value.get("law") {
        return law
            .as_object()
            .map(|_| law)
            .ok_or_else(|| "信封 law 载荷须为对象".to_string());
    }
    line_value
        .as_object()
        .map(|_| line_value)
        .ok_or_else(|| "提案行须为 JSON 对象（裸 law 载荷或 {law:...} 信封）".to_string())
}

/// 单行机器闸（机器可判面全集：结构九要素含公式分离符+否定构造+时间锚）
fn gate_one(line_value: &Value) -> ProposalGateItem {
    let entry_id = line_value
        .get("law")
        .and_then(|l| l.get("id"))
        .or_else(|| line_value.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("<no-id>")
        .to_string();
    let payload = match law_payload_from_line(line_value) {
        Ok(p) => p,
        Err(reason) => {
            return ProposalGateItem {
                entry_id,
                verdict: "reject".into(),
                checks: vec!["envelope".into()],
                reason: Some(reason),
                line: 0,
            }
        }
    };
    match Validator::validate_law_structure(payload) {
        Ok(()) => ProposalGateItem {
            entry_id,
            verdict: "pass".into(),
            checks: vec![
                "structure_nine".into(),
                "formula_separator".into(),
                "negation_scan".into(),
            ],
            reason: None,
            line: 0,
        },
        Err(e) => {
            // 归因首个失败面（报告可读性；完整原因在 reason）
            let formula = payload
                .get("formula")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let failed_check = if scan_negation_constructs(formula).is_empty() {
                "structure_nine"
            } else {
                "negation_scan"
            };
            ProposalGateItem {
                entry_id,
                verdict: "reject".into(),
                checks: vec![failed_check.to_string()],
                reason: Some(e.to_string()),
                line: 0,
            }
        }
    }
}

/// 提案批机器闸：逐行判定+批级汇总（报告行 line 字段在此统一回填）
pub fn gate_proposals(lines: &[Value]) -> ProposalGateReport {
    let mut items = Vec::with_capacity(lines.len());
    for (idx, line_value) in lines.iter().enumerate() {
        let mut item = gate_one(line_value);
        item.line = idx + 1;
        items.push(item);
    }
    let passed = items.iter().filter(|i| i.verdict == "pass").count();
    let rejected = items.len() - passed;
    ProposalGateReport {
        summary: ProposalGateSummary {
            total: items.len(),
            passed,
            rejected,
        },
        items,
    }
}

/// 报告 → JSONL 行序列（逐行判定件，供 side-list/评审队列消费）
pub fn report_to_jsonl(report: &ProposalGateReport) -> Vec<Value> {
    report
        .items
        .iter()
        .map(|i| {
            json!({
                "line": i.line,
                "entry_id": i.entry_id,
                "verdict": i.verdict,
                "checks": i.checks,
                "reason": i.reason,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_proposal(id: &str) -> Value {
        json!({
            "law": {
                "id": id,
                "formula": "F = m * a",
                "symbols": {"F": "force", "m": "mass", "a": "acceleration"},
                "si_units": {"F": "N", "m": "kg", "a": "m/s^2"},
                "domain": "mechanics",
                "solvable": ["F", "m", "a"],
                "derivation": ["seed excerpt s2"],
                "tier": "full",
                "max_version_depth": 1,
                "monotone_form": true,
                "metadata": {"source": "seed excerpt s2"}
            },
            "provenance": {"llm_model": "GLM", "batch": "example"}
        })
    }

    #[test]
    fn test_gate_pass_and_reject_mix() {
        let mut bad = good_proposal("bad_law");
        bad["law"].as_object_mut().unwrap().remove("monotone_form");
        let report = gate_proposals(&[good_proposal("good_law"), bad]);
        assert_eq!(report.summary.total, 2);
        assert_eq!(report.summary.passed, 1);
        assert_eq!(report.summary.rejected, 1);
        assert_eq!(report.items[0].verdict, "pass");
        assert_eq!(report.items[1].line, 2);
        assert_eq!(report.items[1].entry_id, "bad_law");
        assert!(report.items[1]
            .reason
            .as_deref()
            .unwrap()
            .contains("monotone_form"));
    }

    #[test]
    fn test_gate_negation_reject_attributed_to_negation_scan() {
        let mut neg = good_proposal("neg_law");
        // 一元负号=否定构造（G19 同款）：结构面其余全过、无 time_anchor → 拒收归因 negation_scan
        neg["law"]["formula"] = json!("F = -m * a");
        let report = gate_proposals(&[neg]);
        assert_eq!(report.items[0].verdict, "reject");
        assert_eq!(report.items[0].checks[0], "negation_scan");
        assert!(report.items[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("否定类构造"));
    }

    #[test]
    fn test_gate_bare_payload_accepted() {
        let bare = good_proposal("bare_law")["law"].clone();
        let report = gate_proposals(&[bare]);
        assert_eq!(report.summary.passed, 1);
    }

    #[test]
    fn test_report_jsonl_shape() {
        let report = gate_proposals(&[good_proposal("a"), good_proposal("b")]);
        let lines = report_to_jsonl(&report);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["verdict"], "pass");
        assert!(lines[0]["line"].as_u64().unwrap() >= 1);
    }
}
