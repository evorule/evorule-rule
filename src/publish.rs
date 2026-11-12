//! 候选批 → law_set v1.1 发布链（accepted→入池；B-293 T3 工程化件）
//!
//! 转换+验收+组装一体：
//! - 每条候选取 `law` 载荷，注入批级 form/intake_channel 后过
//!   [`Validator::validate_law_intake_v11`]（九要素+形态四型+出身通道从严面）
//! - **all-or-nothing**：任一条拒收则整批不产文档（批完整性），拒收带因分条留报告
//! - 产出 law_set/v1.1 文档 + 发布 manifest（digest 锚定，审计链对接）
//!
//! 语料吸收原则落地面：三态纪律（不强行 accepted）、拒收带因、双轨分离
//! （intake_channel 一经写入永不回写）。

use crate::validate::Validator;
use serde_json::{json, Value};

/// v1.1 文档 $schema（SSOT=evorule-system-rules schemas/law_set/v1.1.json）
pub const LAW_SET_V11_SCHEMA_ID: &str = "https://evorule.org/schemas/law_set/v1.1.json";

/// law 条目允许键集（域 schema v1.1 properties + additionalProperties=false 同向收紧）
const LAW_ALLOWED_KEYS: [&str; 16] = [
    "id",
    "formula",
    "symbols",
    "si_units",
    "domain",
    "domains",
    "cross_refs",
    "solvable",
    "derivation",
    "tier",
    "max_version_depth",
    "monotone_form",
    "time_anchor",
    "form",
    "intake_channel",
    "metadata",
];

/// 发布批元数据（批级：同批全部条目同形态/同出身通道）
#[derive(Debug, Clone)]
pub struct CandidateBatch {
    pub dataset_id: String,
    pub dataset_version: String,
    pub form: String,
    pub intake_channel: String,
    pub title: String,
    pub description: String,
    pub source: String,
    /// tier 授予（L2 口径定案：accepted 默认 light；full 需服务家族对答通道，
    /// B′ 后回填——提案期 tier 只是提案意图，入池 tier=验证级授予）
    pub tier_grant: TierGrant,
}

/// tier 授予档（51 号 L2 口径：公式面可执行=底线，服务家族对答=full 前置）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierGrant {
    /// 默认授予：公式面可执行（结构+冒烟底线下通过即可）
    Light,
    /// 显式授予：须声明服务家族对答通道在位（B′ 后回填期使用）
    Full,
}

impl TierGrant {
    pub fn as_str(&self) -> &'static str {
        match self {
            TierGrant::Light => "light",
            TierGrant::Full => "full",
        }
    }
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "light" => Ok(TierGrant::Light),
            "full" => Ok(TierGrant::Full),
            other => Err(format!("tier_grant 非法（须 light|full）: {other}")),
        }
    }
}

/// 单条拒收记录（带行号与原因；吸收原则 3：拒收带因）
#[derive(Debug, Clone, PartialEq)]
pub struct PublishRejection {
    /// 候选 JSONL 行号（1 起）
    pub line: usize,
    /// 条目 id（可提取时）
    pub entry_id: String,
    pub reason: String,
}

/// 发布错误：拒收清单（all-or-nothing）或消息
#[derive(Debug, Clone)]
pub enum PublishError {
    /// 任一候选未过验收面 → 整批拒绝（附全部分条原因）
    Rejected(Vec<PublishRejection>),
    /// 批级参数/结构错误
    Message(String),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Rejected(rejections) => {
                write!(
                    f,
                    "候选批验收未过（all-or-nothing，{} 条拒收）：",
                    rejections.len()
                )?;
                for r in rejections {
                    write!(f, "\n  line {}: {} — {}", r.line, r.entry_id, r.reason)?;
                }
                Ok(())
            }
            PublishError::Message(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for PublishError {}

/// 形态四型合法值（与 [`crate::validate::Validator::validate_law_intake_v11`] 单源同表）
pub const LAW_FORMS_V11: [&str; 4] = ["equation_identity", "constraint", "procedure", "definition"];
/// 出身通道合法值（与 [`crate::validate::Validator::validate_law_intake_v11`] 单源同表）
pub const INTAKE_CHANNELS_V11: [&str; 2] = ["human_curated", "llm_proposal"];

/// 候选 JSONL 行解析：取 `law` 载荷并注入批级 form/intake_channel
fn law_payload_from_candidate(candidate: &Value, batch: &CandidateBatch) -> Result<Value, String> {
    let law = candidate
        .get("law")
        .ok_or_else(|| "候选缺 law 载荷".to_string())?
        .as_object()
        .ok_or_else(|| "候选 law 载荷须为对象".to_string())?
        .clone();
    let mut obj = law;
    for key in ["form", "intake_channel"] {
        if obj.contains_key(key) {
            return Err(format!(
                "候选自带 {key} 字段——出身通道与形态由发布批级声明注入，候选件不得自带（防双轨混写）"
            ));
        }
    }
    obj.insert("form".into(), Value::String(batch.form.clone()));
    obj.insert(
        "intake_channel".into(),
        Value::String(batch.intake_channel.clone()),
    );
    // tier 授予（L2 口径）：入池 tier=验证级授予，覆盖提案期 tier（提案意图
    // 保留在候选件与 verdict 留痕里，审计链不丢）
    obj.insert(
        "tier".into(),
        Value::String(batch.tier_grant.as_str().to_string()),
    );
    Ok(Value::Object(obj))
}

/// 候选批 → law_set/v1.1 文档（all-or-nothing；失败返回逐条拒收清单）
pub fn build_law_set_document(
    candidates: &[Value],
    batch: &CandidateBatch,
    provenance_note: Option<&str>,
) -> Result<Value, PublishError> {
    if !LAW_FORMS_V11.contains(&batch.form.as_str()) {
        return Err(PublishError::Message(format!(
            "批级 form 非法（须形态四型枚举值）: {}",
            batch.form
        )));
    }
    if !INTAKE_CHANNELS_V11.contains(&batch.intake_channel.as_str()) {
        return Err(PublishError::Message(format!(
            "批级 intake_channel 非法（须出身通道枚举值）: {}",
            batch.intake_channel
        )));
    }

    let mut laws: Vec<Value> = Vec::with_capacity(candidates.len());
    let mut rejections: Vec<PublishRejection> = Vec::new();
    for (idx, candidate) in candidates.iter().enumerate() {
        let line = idx + 1;
        let entry_id = candidate
            .get("law")
            .and_then(|l| l.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("<no-id>")
            .to_string();
        match law_payload_from_candidate(candidate, batch) {
            Err(reason) => rejections.push(PublishRejection {
                line,
                entry_id,
                reason,
            }),
            Ok(payload) => {
                // additionalProperties=false 同向收紧：键集白名单
                if let Some(obj) = payload.as_object() {
                    let unknown: Vec<String> = obj
                        .keys()
                        .filter(|k| !LAW_ALLOWED_KEYS.contains(&k.as_str()))
                        .map(|k| k.to_string())
                        .collect();
                    if !unknown.is_empty() {
                        rejections.push(PublishRejection {
                            line,
                            entry_id,
                            reason: format!("law 载荷含白名单外字段: {}", unknown.join(", ")),
                        });
                        continue;
                    }
                }
                if let Err(e) = Validator::validate_law_intake_v11(&payload) {
                    rejections.push(PublishRejection {
                        line,
                        entry_id,
                        reason: e.to_string(),
                    });
                    continue;
                }
                laws.push(payload);
            }
        }
    }

    if !rejections.is_empty() {
        return Err(PublishError::Rejected(rejections));
    }

    let mut intake = json!({
        "form": batch.form,
        "intake_channel": batch.intake_channel,
        "tier_grant": batch.tier_grant.as_str(),
        "tier_grant_basis": match batch.tier_grant {
            TierGrant::Light => "L2 ruling: computable = formula-face executable; light is the default grant",
            TierGrant::Full => "service-family answer channel verified in place (post-B' backfill)",
        },
        "candidate_count": candidates.len(),
    });
    if let Some(note) = provenance_note {
        intake["provenance_note"] = json!(note);
    }

    Ok(json!({
        "$schema": LAW_SET_V11_SCHEMA_ID,
        "kind": "law_set",
        "id": batch.dataset_id,
        "version": batch.dataset_version,
        "metadata": {
            "title": batch.title,
            "description": batch.description,
            "source": batch.source,
            "verified": "publication chain v1.1: per-entry intake face passed (nine elements + form + intake_channel); batch all-or-nothing",
            "intake": intake,
        },
        "laws": laws,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch() -> CandidateBatch {
        CandidateBatch {
            dataset_id: "physics-dim".into(),
            dataset_version: "1.0.0".into(),
            form: "equation_identity".into(),
            intake_channel: "llm_proposal".into(),
            title: "Physics dimensional-algebra laws".into(),
            description: "B-287 candidate batch published as law_set v1.1".into(),
            source: "seed excerpt; LLM proposals machine-gated + human spot-check".into(),
            tier_grant: TierGrant::Light,
        }
    }

    fn candidate(id: &str) -> Value {
        json!({
            "digest": "deadbeef",
            "law": {
                "id": id,
                "formula": "F = m * a",
                "symbols": {"F": "force", "m": "mass", "a": "acceleration"},
                "si_units": {"F": "N", "m": "kg", "a": "m/s^2"},
                "domain": "mechanics",
                "solvable": ["F", "m", "a"],
                "derivation": ["seed excerpt section 2"],
                "tier": "full",
                "max_version_depth": 1,
                "monotone_form": true,
                "metadata": {"source": "seed-excerpt.md s2", "verified": "machine gate candidate"}
            },
            "provenance": {"llm_model": "GLM"},
            "verdict": {"l1_struct": "pass", "l2_dim": "pass"}
        })
    }

    #[test]
    fn test_publish_two_candidates_ok() {
        let doc = build_law_set_document(&[candidate("a_law"), candidate("b_law")], &batch(), None)
            .unwrap();
        assert_eq!(doc["$schema"], LAW_SET_V11_SCHEMA_ID);
        assert_eq!(doc["kind"], "law_set");
        assert_eq!(doc["laws"].as_array().unwrap().len(), 2);
        assert_eq!(doc["laws"][0]["form"], "equation_identity");
        assert_eq!(doc["laws"][0]["intake_channel"], "llm_proposal");
    }

    #[test]
    fn test_publish_rejects_structurally_bad_candidate_with_line() {
        let mut bad = candidate("bad_law");
        bad["law"].as_object_mut().unwrap().remove("monotone_form");
        let err =
            build_law_set_document(&[candidate("good_law"), bad], &batch(), None).unwrap_err();
        match err {
            PublishError::Rejected(rejections) => {
                assert_eq!(rejections.len(), 1);
                assert_eq!(rejections[0].line, 2);
                assert_eq!(rejections[0].entry_id, "bad_law");
            }
            other => panic!("应拒收清单，实际: {other:?}"),
        }
    }

    #[test]
    fn test_publish_rejects_candidate_carrying_channel() {
        // 双轨混写防炸：候选自带 intake_channel → 拒收（出身由批级声明注入）
        let mut bad = candidate("sneaky_law");
        bad["law"]
            .as_object_mut()
            .unwrap()
            .insert("intake_channel".into(), json!("human_curated"));
        assert!(matches!(
            build_law_set_document(&[bad], &batch(), None),
            Err(PublishError::Rejected(_))
        ));
    }

    #[test]
    fn test_publish_rejects_unknown_key() {
        let mut bad = candidate("weird_law");
        bad["law"]
            .as_object_mut()
            .unwrap()
            .insert("textbook_chapter".into(), json!("3.2"));
        assert!(matches!(
            build_law_set_document(&[bad], &batch(), None),
            Err(PublishError::Rejected(_))
        ));
    }

    #[test]
    fn test_publish_bad_batch_args() {
        let mut b = batch();
        b.form = "textbook_chapter".into();
        assert!(matches!(
            build_law_set_document(&[candidate("a")], &b, None),
            Err(PublishError::Message(_))
        ));
    }

    #[test]
    fn test_publish_tier_grant_overrides_proposal_tier() {
        // L2 口径：提案 tier=full 只是提案意图；入池 tier=验证级授予（默认 light）
        let doc = build_law_set_document(&[candidate("a_law")], &batch(), None).unwrap();
        assert_eq!(doc["laws"][0]["tier"], "light");
        assert_eq!(doc["metadata"]["intake"]["tier_grant"], "light");
        let mut full = batch();
        full.tier_grant = TierGrant::Full;
        let doc_full = build_law_set_document(&[candidate("a_law")], &full, None).unwrap();
        assert_eq!(doc_full["laws"][0]["tier"], "full");
        assert_eq!(
            doc_full["metadata"]["intake"]["tier_grant_basis"],
            "service-family answer channel verified in place (post-B' backfill)"
        );
    }
}
