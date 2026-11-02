// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 EvoRule Project
// This file is part of EvoRule, licensed under GNU Affero General Public License v3 or later.
//! MCP 任务能力包（无人值守防护层，治理数据集约定 `org.evorule.mcp.packs`）
//!
//! 无人值守防护层 T₀：standing delegation 的落地形态。运营方在人在线时于治理面
//! 创建能力包（Draft → Candidate → Active 人工审批链，与拉起批准清单同一套
//! 生命周期），无人值守任务经 task_ref 拉取——**能力包本身在治理面，不在
//! 工作区**（与拉起批准清单同一信任根基：agent 写不了自己的授权）。
//!
//! ## 能力包契约（payload 即包体；schema_ref 固定 [`MCP_PACK_SCHEMA_REF`]）
//!
//! ```json
//! {
//!   "task_ref": "<schedule/任务标识>",
//!   "servers": ["blake3:<64hex>", "..."],
//!   "ttl_secs": 86400,
//!   "budget_secs": 3600,
//!   "on_gap": "block"
//! }
//! ```
//!
//! - `task_ref` = schedule 任务标识（entry_id 约定 = task_ref；同 ref 重发
//!   = 版本递增重新授权）。
//! - `servers` = 拉起批准清单 server_key 的**子集**（执行侧门组合运算
//!   pack∩list 双闸；此处不校验子集性——批准清单是活数据集，包创建时点
//!   的子集性由运营方核验，执行侧交集运算天然兜底）。
//! - `ttl_secs` = 授权时间界（执行侧按包激活时刻起算，超时同未授权处置）。
//! - `budget_secs` = 可选资源界声明（联动 16 号 wall_clock_budget 既有强制
//!   机制；本数据集只承载声明与核验，不新增执行面）。
//! - `on_gap` = 能力缺口处置，**唯一合法值 "block"**（无人值守防护层缺省
//!   收紧不变式：不提供 improvise 形态）。
//!
//! ## 与拉起批准清单的分面
//!
//! 批准清单（`org.evorule.mcp.servers`）管「谁可以被允许拉起」（许可）；
//! 能力包管「本次无人值守任务允许用哪些」（standing delegation 授权）。
//! 执行侧门组合 = pack∩list 双闸（详见无人值守防护层设计正本）。

use serde_json::Value;

/// MCP 任务能力包数据集约定 id
pub const MCP_PACKS_DATASET_ID: &str = "org.evorule.mcp.packs";

/// 能力包固定 schema_ref（builtin 严格壳，见 `src/store/schemas/mcp/pack.json`）
pub const MCP_PACK_SCHEMA_REF: &str = "builtin:mcp/pack";

/// on_gap 唯一合法值（无人值守防护层缺省收紧不变式：不提供 improvise 形态）
pub const ON_GAP_BLOCK: &str = "block";

/// 能力包包体校验（写核统一消费点；schema 形状闸之上叠加语义闸）。
///
/// - `task_ref` 非空（trim 后）；
/// - `servers` 非空数组、每项为 `blake3:<64hex>` 形态（规范键）、无重复；
/// - `ttl_secs` 正整数；
/// - `on_gap` 若给出必须为 `"block"`（缺省即 block——schema 层 required，
///   旧形态兼容由本函数的 required 语义兜底）；
/// - `budget_secs` 若给出必须为正整数。
pub fn validate_mcp_pack_payload(payload: &Value) -> Result<(), String> {
    let obj = payload
        .as_object()
        .ok_or_else(|| "payload 须为对象".to_string())?;
    let task_ref = obj
        .get("task_ref")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "缺 task_ref（或非字符串）".to_string())?;
    if task_ref.trim().is_empty() {
        return Err("task_ref 不能为空".to_string());
    }
    let servers = obj
        .get("servers")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "缺 servers（或非数组）".to_string())?;
    if servers.is_empty() {
        return Err(
            "servers 不能为空数组（空包=零授权，无存在意义；请删除包而非空授权）".to_string(),
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    for v in servers {
        let key = v
            .as_str()
            .ok_or_else(|| "servers 含非字符串项".to_string())?;
        let hex = key
            .strip_prefix("blake3:")
            .ok_or_else(|| format!("servers 项 `{key}` 缺 blake3: 前缀（须为批准清单规范键）"))?;
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("servers 项 `{key}` 非规范键形态（blake3:<64hex>）"));
        }
        if !seen.insert(key.to_string()) {
            return Err(format!("servers 含重复项：{key}"));
        }
    }
    let ttl = obj
        .get("ttl_secs")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "缺 ttl_secs（或非正整数）".to_string())?;
    if ttl == 0 {
        return Err("ttl_secs 必须为正整数".to_string());
    }
    match obj.get("on_gap") {
        Some(Value::String(s)) if s == ON_GAP_BLOCK => {}
        Some(other) => {
            return Err(format!(
                "on_gap 仅支持 \"{}\"，收到 {}（无人值守防护层缺省收紧不变式：不提供 improvise 形态）",
                ON_GAP_BLOCK, other
            ));
        }
        None => {
            return Err("缺 on_gap（唯一合法值 \"block\"）".to_string());
        }
    }
    if let Some(b) = obj.get("budget_secs") {
        match b.as_u64() {
            Some(secs) if secs > 0 => {}
            _ => return Err("budget_secs 须为正整数（或整体省略）".to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn payload_json(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    fn valid_payload() -> Value {
        payload_json(
            r#"{
            "task_ref": "nightly-maintenance",
            "servers": ["blake3:d5c3446a9fd2ae9dc68e6ffbaeb619e0e675f0572bffd04de9a9afce2d28ed87"],
            "ttl_secs": 86400,
            "on_gap": "block"
        }"#,
        )
    }

    #[test]
    fn valid_pack_passes() {
        assert!(validate_mcp_pack_payload(&valid_payload()).is_ok());
        // budget 可选
        let mut p = valid_payload();
        p["budget_secs"] = serde_json::json!(3600);
        assert!(validate_mcp_pack_payload(&p).is_ok());
    }

    #[test]
    fn pack_rejects_shape_violations() {
        // 空 servers
        let mut p = valid_payload();
        p["servers"] = serde_json::json!([]);
        assert!(validate_mcp_pack_payload(&p)
            .unwrap_err()
            .contains("不能为空数组"));
        // 非规范键
        let mut p = valid_payload();
        p["servers"] = serde_json::json!(["npx"]);
        assert!(validate_mcp_pack_payload(&p)
            .unwrap_err()
            .contains("blake3:"));
        // 重复项
        let mut p = valid_payload();
        p["servers"] = serde_json::json!([
            "blake3:d5c3446a9fd2ae9dc68e6ffbaeb619e0e675f0572bffd04de9a9afce2d28ed87",
            "blake3:d5c3446a9fd2ae9dc68e6ffbaeb619e0e675f0572bffd04de9a9afce2d28ed87"
        ]);
        assert!(validate_mcp_pack_payload(&p).unwrap_err().contains("重复"));
        // on_gap 非法值（improvise 形态不存在）
        let mut p = valid_payload();
        p["on_gap"] = serde_json::json!("improvise");
        assert!(validate_mcp_pack_payload(&p)
            .unwrap_err()
            .contains("improvise"));
        // ttl 零
        let mut p = valid_payload();
        p["ttl_secs"] = serde_json::json!(0);
        assert!(validate_mcp_pack_payload(&p)
            .unwrap_err()
            .contains("ttl_secs"));
        // 空任务标识
        let mut p = valid_payload();
        p["task_ref"] = serde_json::json!("  ");
        assert!(validate_mcp_pack_payload(&p)
            .unwrap_err()
            .contains("task_ref"));
    }
}
