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
//! - 生命周期照常走数据集五态（条目落 Draft → Candidate → Active 组织内生效
//!   → Published 对外可拉取）；执行侧拉取端点 = `GET /v1/datasets/{id}/bundle`
//!   （X-Api-Key scope=pull，同租户 Active 即可拉，跨租户须 public+Published）。
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

/// 批准条目固定 schema_ref（builtin 严格壳：六必填字段+形态，见
/// `src/store/schemas/mcp/server.json`）
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

/// 批准条目 payload 全量校验（写核统一消费点：schema 形状闸之上叠加跨字段
/// 语义闸——归因完整性 + server_key 重算比对）。
pub fn validate_mcp_server_payload(payload: &Value) -> Result<(), String> {
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
    // 归因完整性：无归因的放行不允许
    for key in ["comment", "approved_by", "approved_at", "approval_ref"] {
        match obj.get(key).and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => {}
            _ => return Err(format!("缺 {key}（或非非空字符串；批准必须带完整归因）")),
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
}
