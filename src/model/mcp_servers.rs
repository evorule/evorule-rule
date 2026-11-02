// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 EvoRule Project
// This file is part of EvoRule, licensed under GNU Affero General Public License v3 or later.
//! MCP server 拉起批准清单（治理数据集约定 `org.evorule.mcp.servers`）
//!
//! 治理原生线：MCP server 接入从「配置文件写了就生效」升级为治理行为——
//! **意愿在项目目录（mcp.toml），许可在治理面（本数据集）**。数据集生效条目
//! = 全局批准清单；执行侧（agent 装载门）拉取生效清单，与项目意愿（mcp.toml
//! 的 stdio server）求交判定可拉起集合。会话能写 mcp.toml，写不进本数据集的
//! 审批链——这是信任根基。
//!
//! ## 条目契约（payload 即批准条目；schema_ref 固定 [`MCP_SERVER_SCHEMA_REF`]）
//!
//! ```json
//! {
//!   "server_key": "blake3:<64hex>",
//!   "command": "npx",
//!   "args_pattern": ["-y", "@modelcontextprotocol/server-*"],
//!   "comment": "谁为何登记（必填）",
//!   "approved_by": "<治理账号>",
//!   "approved_at": "<ISO-8601>",
//!   "approval_ref": "<提案/条目版本锚>"
//! }
//! ```
//!
//! - `server_key` = command+args_pattern 的规范哈希（[`canonical_server_key`]，
//!   与执行侧同形实现+黄金向量对齐测试）；录入时重算比对，防手打错键。
//! - `entry_id` 约定 = server_key（同键同条目，版本递增=重新批准）。
//! - **归因两态语义（P2b 提案回流）**：`comment` 恒必填；批准归因三态
//!   （approved_by/approved_at/approval_ref）在**提案态（Draft/Candidate）可
//!   缺席**——提案是请求不是批准，机器提案通路的条目不得预填批准归因（伪造
//!   归因=绕过信任根基）；归因在人工批准动作时按审批者身份补全（approve 通路
//!   [`fill_approval_attribution`]），**晋升 Active 的任何通路闸死完备性**
//!   （写核对落账即 Active/Published 的写入强制齐全；迁移闸对 →Active 晋升
//!   全量复检）。给出归因即必须完整（半空归因=损坏，拒绝）。
//! - 生命周期照常走数据集五态（条目落 Draft → Candidate → Active 组织内生效
//!   → Published 对外可拉取）；执行侧拉取端点 = `GET /v1/datasets/{id}/bundle`
//!   （X-Api-Key scope=pull，同租户 Active 即可拉，跨租户须 public+Published）。
//!   提案上行端点 = `POST /v1/invoke/propose_mcp_server`（X-Api-Key
//!   scope=entries:propose；落无 llm 标记的普通 Draft——与 propose_knowledge_entry
//!   的 trust=llm 强制语义分岔：本数据集的批准必须人工可达，而 llm 产出被
//!   迁移闸锁死在 Draft 只能机器闸晋升）。
//!
//! ## 哈希域归属
//!
//! `server_key` 属**治理资产（条目指纹）域**：serde_json canonical 紧凑输入 +
//! `blake3:` 前缀 + evorule-hash `json_digest`+`prefixed` 原语。规范序列 =
//! JSON 数组 `[command, args_pattern 段...]`（args_pattern 缺省 = 空数组）；
//! 输入构造即域口径的一部分，两侧实现必须逐字节一致（黄金向量测试锚定）。
//!
//! ## 双侧对齐契约（执行侧为镜像实现，本模块为语义权威）
//!
//! 以下三项在执行侧（agent 装载门）存在同形实现，任何修改必须双侧同步 +
//! 黄金向量测试同步更新：
//! 1. [`BARE_INTERPRETERS`] 清单；
//! 2. [`canonical_server_key`] 规范序列与哈希；
//! 3. [`validate_approval_entry`] 形状规则（裸解释器/段通配/空数组）。

use serde_json::Value;

/// MCP 拉起批准清单数据集约定 id（dataset_id 租户内唯一；各租户可各有一份）
pub const MCP_SERVERS_DATASET_ID: &str = "org.evorule.mcp.servers";

/// 批准条目固定 schema_ref（builtin 严格壳：三必填恒在+批准归因三态在生效面
/// 强制（代码闸），见 `src/store/schemas/mcp/server.json`）
pub const MCP_SERVER_SCHEMA_REF: &str = "builtin:mcp/server";

/// 裸解释器清单：这类 command 是包管理器/运行时（npx 拉任意包=任意代码执行），
/// **不允许**以无 args_pattern 的形态进入批准清单（否则清单形同虚设）。
/// 与执行侧镜像清单对齐；修改须双侧同步。
pub const BARE_INTERPRETERS: &[&str] = &["npx", "uvx", "node", "python", "python3", "deno", "bun"];

/// 批准条目身份键：command+args_pattern 的规范哈希（治理资产域条目指纹）。
///
/// 规范序列 = JSON 数组 `[command, 段...]`（args_pattern 缺省 = 空数组），
/// serde_json 紧凑序列化后 `json_digest`，`blake3:` 前缀。command 不做归一化
/// （匹配语义为字面精确，规范序列同样字面精确——" npx" 与 "npx" 是两个键）。
pub fn canonical_server_key(command: &str, args_pattern: Option<&[String]>) -> String {
    let mut canon = Vec::with_capacity(1 + args_pattern.map_or(0, <[String]>::len));
    canon.push(Value::String(command.to_string()));
    if let Some(segs) = args_pattern {
        canon.extend(segs.iter().map(|s| Value::String(s.clone())));
    }
    evorule_hash::prefixed(&evorule_hash::json_digest(&canon))
}

/// 批准条目形状规则（command/args_pattern 语义面；schema 管不到的条件闸）。
///
/// - command 非空；
/// - args_pattern 缺省 = 仅匹配 command（固定二进制）；给出 = 非空段序列，
///   段内仅支持段尾 `*` 前缀通配（含 `*` 但不在段尾的段永不匹配，录入即拒）；
/// - 裸解释器必须带非空 args_pattern。
pub fn validate_approval_entry(
    command: &str,
    args_pattern: Option<&[String]>,
) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err("command 不能为空".to_string());
    }
    if let Some(segs) = args_pattern {
        if segs.is_empty() {
            return Err(
                "args_pattern 空数组无意义：请省略该字段（仅匹配 command），或填写实际段序列"
                    .to_string(),
            );
        }
        for s in segs {
            if s.is_empty() {
                return Err("args_pattern 含空段".to_string());
            }
            if s.contains('*') && !s.ends_with('*') {
                return Err(format!(
                    "args_pattern 段 `{s}` 含通配符但不在段尾（仅支持段尾 `*` 前缀通配，其余形态永不匹配，录入即拒）"
                ));
            }
        }
    }
    let bare = BARE_INTERPRETERS
        .iter()
        .any(|i| command.eq_ignore_ascii_case(i));
    if bare && args_pattern.is_none_or(<[String]>::is_empty) {
        return Err(format!(
            "裸解释器 `{command}` 必须登记非空 args_pattern（锁死包名/脚本参数，否则批准形同虚设）"
        ));
    }
    Ok(())
}

/// 批准条目 payload 校验（完备形态）：形状 + 批准归因三态齐全 + server_key
/// 重算比对。→Active 迁移闸（任何晋升通路）与执行侧镜像校验共用——执行侧
/// 拉到的生效条目（Active/Published）必为完备形态。
pub fn validate_mcp_server_payload(payload: &Value) -> Result<(), String> {
    validate_mcp_server_entry(payload, true)
}

/// 写核形态校验（按落账状态条件化）：归因三态在提案态可缺席（见模块文档
/// 「归因两态语义」）；`require_approval_attribution=true` 用于落账即
/// Active/Published 的写入。其余闸（形状/comment/server_key 重算）两态同闸。
pub fn validate_mcp_server_entry(
    payload: &Value,
    require_approval_attribution: bool,
) -> Result<(), String> {
    let obj = payload
        .as_object()
        .ok_or_else(|| "payload 须为对象".to_string())?;
    let command = obj
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "缺 command（或非字符串）".to_string())?;
    let args_pattern: Option<Vec<String>> = match obj.get("args_pattern") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "args_pattern 含非字符串段".to_string())
                })
                .collect::<Result<Vec<String>, String>>()?,
        ),
        Some(_) => return Err("args_pattern 须为字符串数组".to_string()),
    };
    validate_approval_entry(command, args_pattern.as_deref())?;
    // comment 恒必填（登记/提案都必须带可读理由）
    match obj.get("comment").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => {}
        _ => return Err("缺 comment（或非非空字符串；登记必须带可读理由）".to_string()),
    }
    // 批准归因三态：生效形态必须齐全；提案态可缺席，但给出即必须完整
    for key in ["approved_by", "approved_at", "approval_ref"] {
        let present = obj.get(key).and_then(|v| v.as_str());
        match (present, require_approval_attribution) {
            (Some(s), _) if !s.trim().is_empty() => {}
            (None, false) => {}
            (None, true) => {
                return Err(format!("缺 {key}（生效条目必须带完整批准归因）"));
            }
            (Some(_), _) => {
                return Err(format!(
                    "{key} 为空（归因给出即必须完整；提案态请整体省略）"
                ));
            }
        }
    }
    // server_key 重算比对（录入校验与执行侧共用规范函数的对齐锚点）
    let expected = canonical_server_key(command, args_pattern.as_deref());
    match obj.get("server_key").and_then(|v| v.as_str()) {
        Some(k) if k == expected => Ok(()),
        Some(other) => Err(format!(
            "server_key 不匹配：期望 {expected}，实际 {other}（以 canonical_server_key 重算为准）"
        )),
        None => Err(format!("缺 server_key（本条目计算值：{expected}）")),
    }
}

/// 提案态判定：payload 是否缺批准归因（approve 通路据此决定是否按审批者身份
/// 补全——见 [`fill_approval_attribution`]；归因齐全的条目零改动直迁）。
pub fn approval_attribution_missing(payload: &Value) -> bool {
    ["approved_by", "approved_at", "approval_ref"]
        .iter()
        .any(|key| {
            payload
                .get(key)
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.trim().is_empty())
        })
}

/// 批准归因补全（人工批准动作的语义组成）：逐字段填补缺席/空白项，已有
/// 归因零改动（补全的是「批准」这个事实的归属，不覆盖任何既有声明）。
pub fn fill_approval_attribution(
    payload: &mut Value,
    approved_by: &str,
    approved_at: &str,
    approval_ref: &str,
) {
    if let Some(obj) = payload.as_object_mut() {
        let fill = |obj: &mut serde_json::Map<String, Value>, key: &str, v: &str| {
            let blank = obj
                .get(key)
                .and_then(|x| x.as_str())
                .is_none_or(|s| s.trim().is_empty());
            if blank {
                obj.insert(key.to_string(), Value::String(v.to_string()));
            }
        };
        fill(obj, "approved_by", approved_by);
        fill(obj, "approved_at", approved_at);
        fill(obj, "approval_ref", approval_ref);
    }
}

/// 扫描报告完整性闸（提案 intake，mcp-inspect 报告随提案上行时的校验）。
///
/// 报告形态（执行侧 `mcp-inspect` 产出）：`{scanner_version, scanned_at,
/// tools_listed, ioc_hits[], llm_advisory, rules_source, report_hash}`。
/// `report_hash` = `blake3:` 前缀哈希（evorule-hash `json_digest`+`prefixed`
/// 原语），对去除 `report_hash` 键后的报告对象重算——服务端重算比对，签约后
/// 的任何篡改即拒（调用侧键值不采信，与 server_key 同一信任纪律）。
/// `llm_advisory` 是 advisory 块：只进报告与审批展示，不参与任何自动裁决。
pub fn validate_scan_report(report: &Value) -> Result<(), String> {
    let obj = report
        .as_object()
        .ok_or_else(|| "scan_report 须为对象".to_string())?;
    for key in ["scanner_version", "scanned_at"] {
        match obj.get(key).and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => {}
            _ => return Err(format!("scan_report 缺 {key}（或非非空字符串）")),
        }
    }
    match obj.get("tools_listed") {
        Some(v) if v.is_u64() => {}
        _ => return Err("scan_report 缺 tools_listed（或非非负整数）".to_string()),
    }
    let hits = obj
        .get("ioc_hits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "scan_report 缺 ioc_hits（或非数组）".to_string())?;
    for hit in hits {
        let h = hit
            .as_object()
            .ok_or_else(|| "ioc_hits 含非对象项".to_string())?;
        for key in [
            "rule_id", "category", "severity", "tool", "field", "excerpt",
        ] {
            match h.get(key).and_then(|v| v.as_str()) {
                Some(_) => {}
                _ => return Err(format!("ioc_hits 项缺 {key}（或非字符串）")),
            }
        }
        match h.get("severity").and_then(|v| v.as_str()) {
            Some("high" | "medium" | "low") => {}
            other => {
                return Err(format!(
                    "ioc_hits 项 severity 非法（{other:?}；只接受 high|medium|low）"
                ))
            }
        }
    }
    match obj.get("llm_advisory").and_then(|v| v.as_object()) {
        Some(a) => match a.get("status").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => {}
            _ => return Err("llm_advisory 缺 status（或非非空字符串）".to_string()),
        },
        _ => return Err("scan_report 缺 llm_advisory（或非对象）".to_string()),
    }
    match obj.get("rules_source").and_then(|v| v.as_object()) {
        Some(r) => match r.get("baseline_version").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => {}
            _ => return Err("rules_source 缺 baseline_version（或非非空字符串）".to_string()),
        },
        _ => return Err("scan_report 缺 rules_source（或非对象）".to_string()),
    }
    match obj.get("report_hash").and_then(|v| v.as_str()) {
        Some(h) if h.len() == 71 && h.starts_with("blake3:") => {}
        _ => return Err("scan_report 缺 report_hash（或非 blake3:<64hex> 形态）".to_string()),
    }
    // 完整性重算：去除 report_hash 后的规范化摘要必须逐字节一致。
    let mut content = obj.clone();
    content.remove("report_hash");
    let expected = evorule_hash::prefixed(&evorule_hash::json_digest(&content));
    if obj["report_hash"] != expected {
        return Err(format!(
            "scan_report report_hash 不匹配：期望 {expected}，实际 {}（去除 report_hash 后重算为准）",
            obj["report_hash"]
        ));
    }
    Ok(())
}

/// 审批展示标（approve 响应附带的确定性扫描状态，三色语义）：
/// `unscanned`（灰：提案未附扫描报告）/ `ioc_hit`（红：确定性命中≥1）/
/// `advisory`（黄：LLM advisory 置 flagged）/ `clean`（绿：已扫描零命中）。
/// 纯展示分类：不改变条目语义，裁决权始终在人。
pub fn scan_marker(payload: &Value) -> Value {
    match payload.get("scan_report") {
        None => serde_json::json!({ "scan_status": "unscanned" }),
        Some(report) => {
            let hits = report
                .get("ioc_hits")
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            if hits > 0 {
                return serde_json::json!({ "scan_status": "ioc_hit", "ioc_hits": hits });
            }
            let advisory = report
                .get("llm_advisory")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if advisory == "flagged" {
                serde_json::json!({ "scan_status": "advisory" })
            } else {
                serde_json::json!({ "scan_status": "clean" })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    // ===== 黄金向量（与执行侧逐字面量对齐；任一侧修改必须双侧同步） =====

    #[test]
    fn canonical_server_key_golden_vectors() {
        let segs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 固定二进制（无 args_pattern）
        assert_eq!(
            canonical_server_key("my-fixed-server.exe", None),
            "blake3:d5c3446a9fd2ae9dc68e6ffbaeb619e0e675f0572bffd04de9a9afce2d28ed87"
        );
        // 包管理器 + 通配段
        assert_eq!(
            canonical_server_key(
                "npx",
                Some(&segs(&["-y", "@modelcontextprotocol/server-*"]))
            ),
            "blake3:f890f2b86546855e48b14db28ab97d7ef06e411ed0a21fb003911a3e1a36c953"
        );
        // 字面精确（不做归一化）：前导空格 = 不同键
        assert_ne!(
            canonical_server_key("npx", Some(&segs(&["-y"]))),
            canonical_server_key("npx ", Some(&segs(&["-y"])))
        );
    }

    // ===== 形状规则 =====

    #[test]
    fn approval_entry_shape_rules() {
        // 裸解释器无 args_pattern 拒绝
        assert!(validate_approval_entry("npx", None).is_err());
        assert!(validate_approval_entry("NPX", Some(&[] as &[String])).is_err());
        // 裸解释器带段序列通过
        assert!(validate_approval_entry(
            "npx",
            Some(&["@modelcontextprotocol/server-*".to_string()])
        )
        .is_ok());
        // 固定二进制无 args_pattern 通过
        assert!(validate_approval_entry("my-server.exe", None).is_ok());
        // 空数组 / 空段 / 空命令 / 中段通配拒绝
        assert!(validate_approval_entry("foo", Some(&[] as &[String])).is_err());
        assert!(validate_approval_entry("foo", Some(&["a".to_string(), String::new()])).is_err());
        assert!(validate_approval_entry("  ", None).is_err());
        assert!(validate_approval_entry("foo", Some(&["a*b".to_string()])).is_err());
    }

    // ===== payload 全量校验 =====

    fn payload_json(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn payload_validates_full_entry() {
        let key =
            canonical_server_key("npx", Some(&["-y".to_string(), "@scope/srv-*".to_string()]));
        let p = payload_json(&format!(
            r#"{{"server_key":"{key}","command":"npx","args_pattern":["-y","@scope/srv-*"],
                "comment":"平台组登记","approved_by":"alice","approved_at":"2026-11-01T00:00:00Z",
                "approval_ref":"org.evorule.mcp.servers@v1"}}"#
        ));
        assert!(validate_mcp_server_payload(&p).is_ok());
    }

    #[test]
    fn payload_rejects_missing_or_wrong_fields() {
        let key = canonical_server_key("my-server.exe", None as Option<&[String]>);
        // 缺 server_key（错误信息携带本条目正确键，兼作录入助手）
        let p = payload_json(
            r#"{"command":"my-server.exe","comment":"c","approved_by":"a","approved_at":"t","approval_ref":"r"}"#,
        );
        let err = validate_mcp_server_payload(&p).unwrap_err();
        assert!(err.contains("缺 server_key") && err.contains(&key));
        // server_key 打错
        let p = payload_json(&format!(
            r#"{{"server_key":"blake3:{:0>64}","command":"my-server.exe","comment":"c","approved_by":"a","approved_at":"t","approval_ref":"r"}}"#,
            "0"
        ));
        assert!(validate_mcp_server_payload(&p)
            .unwrap_err()
            .contains("server_key 不匹配"));
        // 缺归因字段
        let p = payload_json(&format!(
            r#"{{"server_key":"{key}","command":"my-server.exe","comment":"c","approved_by":"a","approval_ref":"r"}}"#
        ));
        assert!(validate_mcp_server_payload(&p)
            .unwrap_err()
            .contains("缺 approved_at"));
        // 裸解释器无 args_pattern（跨字段闸先于 server_key 比对）
        let p = payload_json(
            r#"{"command":"uvx","comment":"c","approved_by":"a","approved_at":"t","approval_ref":"r"}"#,
        );
        assert!(validate_mcp_server_payload(&p)
            .unwrap_err()
            .contains("裸解释器"));
    }

    // ===== 归因两态（P2b 提案回流）=====

    #[test]
    fn proposal_form_attribution_conditional() {
        let key = canonical_server_key("my-server.exe", None as Option<&[String]>);
        // 提案形态（无归因三态）：写核形态合法，完备形态拒绝
        let proposal = payload_json(&format!(
            r#"{{"server_key":"{key}","command":"my-server.exe","comment":"自动提案：会话首次尝试"}}"#
        ));
        assert!(validate_mcp_server_entry(&proposal, false).is_ok());
        let err = validate_mcp_server_entry(&proposal, true).unwrap_err();
        assert!(err.contains("缺 approved_by"));
        // 半空归因：无论何态都拒绝
        let half = payload_json(&format!(
            r#"{{"server_key":"{key}","command":"my-server.exe","comment":"c","approved_by":""}}"#
        ));
        assert!(validate_mcp_server_entry(&half, false)
            .unwrap_err()
            .contains("approved_by 为空"));
        // 补全闭环：缺席判定 → 按审批者身份补全 → 完备形态通过
        let mut p = proposal.clone();
        assert!(approval_attribution_missing(&p));
        fill_approval_attribution(&mut p, "alice", "2026-11-01T00:00:00Z", "ref@v1");
        assert!(!approval_attribution_missing(&p));
        assert!(validate_mcp_server_entry(&p, true).is_ok());
        // 已有归因零覆盖：approved_by 保留原值
        let mut keep = payload_json(&format!(
            r#"{{"server_key":"{key}","command":"my-server.exe","comment":"c","approved_by":"bob"}}"#
        ));
        fill_approval_attribution(&mut keep, "alice", "t", "r");
        assert_eq!(keep["approved_by"], "bob");
        assert_eq!(keep["approval_ref"], "r");
    }

    // ===== 扫描报告 intake（确定性层；fixture 与执行侧逐字面量对齐） =====

    /// 跨仓黄金向量 fixture（无 report_hash 的报告本体；执行侧 mcp-inspect
    /// 测试持同一字面量，任一侧修改必须双侧同步）。
    fn report_fixture() -> Value {
        serde_json::from_str(
            r#"{
                "scanner_version": "1",
                "scanned_at": "2026-11-02T00:00:00Z",
                "tools_listed": 2,
                "ioc_hits": [
                    {"rule_id": "TP-002", "category": "instruction_override", "severity": "high",
                     "tool": "summarize", "field": "description", "excerpt": "never tell the user"}
                ],
                "llm_advisory": {"status": "unavailable",
                                 "summary": "LLM semantic layer not wired in this scanner version; IOC findings stand alone"},
                "rules_source": {"baseline_version": "1.0.0",
                                 "extensions_file": null, "extensions_file_hash": null}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn scan_report_intake_round_trip() {
        let mut report = report_fixture();
        let digest = evorule_hash::prefixed(&evorule_hash::json_digest(&report));
        report["report_hash"] = serde_json::json!(digest.clone());
        assert!(validate_scan_report(&report).is_ok());
        // 签约后篡改即拒
        let mut tampered = report.clone();
        tampered["tools_listed"] = serde_json::json!(7);
        assert!(validate_scan_report(&tampered)
            .unwrap_err()
            .contains("report_hash 不匹配"));
        // 缺 report_hash 拒
        let mut missing = report.clone();
        missing.as_object_mut().unwrap().remove("report_hash");
        assert!(validate_scan_report(&missing)
            .unwrap_err()
            .contains("缺 report_hash"));
        // 非法 severity 拒
        let mut bad_sev = report.clone();
        bad_sev["ioc_hits"][0]["severity"] = serde_json::json!("loud");
        assert!(validate_scan_report(&bad_sev)
            .unwrap_err()
            .contains("severity 非法"));
        // 缺 llm_advisory 拒
        let mut no_advisory = report;
        no_advisory.as_object_mut().unwrap().remove("llm_advisory");
        assert!(validate_scan_report(&no_advisory)
            .unwrap_err()
            .contains("llm_advisory"));
    }

    #[test]
    fn scan_report_golden_vector_cross_repo() {
        let report = report_fixture();
        let digest = evorule_hash::prefixed(&evorule_hash::json_digest(&report));
        assert_eq!(
            digest, "blake3:0aa650a5bdf2c5998223c0f3c04500ef53a0b49faa5463ff69bb5812de42bdda",
            "golden vector drift: report fixture or digest contract changed on this side"
        );
    }

    #[test]
    fn scan_marker_classification() {
        // 无报告 = 灰标
        let key = canonical_server_key("my-server.exe", None as Option<&[String]>);
        let plain =
            serde_json::json!({"server_key": key, "command": "my-server.exe", "comment": "c"});
        assert_eq!(scan_marker(&plain)["scan_status"], "unscanned");
        // IOC 命中 = 红标（携带命中数；advisory 同时 flagged 也不遮蔽红）
        let red = serde_json::json!({"scan_report": {
            "ioc_hits": [{"rule_id": "TP-002"}],
            "llm_advisory": {"status": "flagged"}
        }});
        let marker = scan_marker(&red);
        assert_eq!(marker["scan_status"], "ioc_hit");
        assert_eq!(marker["ioc_hits"], 1);
        // advisory flagged = 黄标
        let yellow = serde_json::json!({"scan_report": {
            "ioc_hits": [], "llm_advisory": {"status": "flagged"}
        }});
        assert_eq!(scan_marker(&yellow)["scan_status"], "advisory");
        // 已扫描零命中 = 绿标
        let green = serde_json::json!({"scan_report": {
            "ioc_hits": [], "llm_advisory": {"status": "clear"}
        }});
        assert_eq!(scan_marker(&green)["scan_status"], "clean");
    }
}
