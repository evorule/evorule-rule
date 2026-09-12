//! 服务目录（02 方案 C2：服务名/契约治理侧 SSOT）
//!
//! 定位：治理侧「已知服务」参考目录 —— 服务名、版本、契约、敏感标记、绑定提示。
//! 作用：
//! - 依赖声明事前预检（`PUT /deps/datasets/{id}` 校验 service_name ∈ 目录）；
//! - 快照包导出时补齐服务契约（io_contract/sensitive/version 随包携带）；
//! - 与执行侧 `bound_services`（原生 + registry）衔接的三层绑定层 0。
//!
//! 边界：目录存「服务描述」，不存端点/凭据（凭据永不入库，走执行侧密钥管理）。

use serde::{Deserialize, Serialize};

use crate::model::dependency::IoContract;

/// 服务绑定提示（执行侧如何提供该服务；D9 服务独立化演进方向）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BindingHint {
    /// 原生内嵌（demo-services 等插件）
    #[default]
    Native,
    /// service_registry.json 显式绑定
    Registry,
    /// 独立服务进程（D9 演进：独立开发/部署/演进）
    Standalone,
}

/// 服务目录条目
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceCatalogEntry {
    pub service_name: String,
    /// 服务业务版本（C4）
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 输入输出契约（C6）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_contract: Option<IoContract>,
    /// 是否涉及凭据/敏感数据（C6）
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub binding_hint: BindingHint,
    /// official | org:<tenant_id>
    pub managed_by: String,
    /// platform | tenant:<tenant_id>
    pub scope: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

fn default_version() -> String {
    "1.0.0".into()
}

/// 嵌入副本登记表（泛化）：新增插件 = 在此追加一项
/// `(插件 id, include_str! 嵌入副本)`，聚合函数与守卫测试零改动
/// （副本由 evorule-server `scripts/sync-native-services.ps1` 同步）。
/// 声明序 = 聚合序，与执行侧挂载链声明序一致。
const EMBEDDED_SERVICE_FILES: &[(&str, &str)] = &[
    (
        "demo-services",
        include_str!("official_native_services.demo-services.embedded.json"),
    ),
    (
        "physics-services",
        include_str!("official_native_services.physics-services.embedded.json"),
    ),
    (
        "indicator-services",
        include_str!("official_native_services.indicator-services.embedded.json"),
    ),
];

/// 官方预置原生服务种子（声明文件化；泛化至多插件聚合）。
///
/// SSOT = evorule-server 仓各插件目录 `plugins/<id>/official_native_services.json`；
/// 本仓持有各插件嵌入副本（由 evorule-server `scripts/sync-native-services.ps1`
/// 从源仓同步），守卫测试锁定副本合法性与顺序。聚合序 = [`EMBEDDED_SERVICE_FILES`]
/// 声明序（与执行侧挂载链声明序一致）；跨插件服务名全局唯一，冲突即 fail-fast
/// （嵌入副本属仓内完整性问题，如实报错、不静默跳过）。
/// 返回 `(service_name, sensitive, description)`。
pub fn official_native_services() -> Vec<(String, bool, String)> {
    let mut all: Vec<(String, bool, String)> = Vec::new();
    for (plugin_id, raw) in EMBEDDED_SERVICE_FILES {
        let file: serde_json::Value = serde_json::from_str(raw).unwrap_or_else(|e| {
            panic!(
                "official_native_services.{plugin_id}.embedded.json 非法 JSON: {e} — \
                     请在 evorule-server 仓运行 scripts/sync-native-services.ps1 重新同步"
            )
        });
        let services = file.get("services").and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("official_native_services.{plugin_id}.embedded.json 缺 services 数组 — 请重新同步嵌入副本"));
        for s in services {
            let name = s
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("official_native_services.{plugin_id}.embedded.json 条目缺 name — 请重新同步嵌入副本"))
                .to_string();
            let sensitive = s
                .get("sensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| panic!("official_native_services.{plugin_id}.embedded.json 条目缺 sensitive — 请重新同步嵌入副本"));
            let description = s
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("official_native_services.{plugin_id}.embedded.json 条目缺 description — 请重新同步嵌入副本"))
                .to_string();
            if all.iter().any(|(n, _, _)| *n == name) {
                panic!(
                    "服务名 {name} 跨插件重复（plugin_id={plugin_id}）— \
                     全局服务名必须唯一，请修正源仓声明文件后重新同步"
                );
            }
            all.push((name, sensitive, description));
        }
    }
    all
}

/// 由官方种子生成目录条目（version=1.0.0，binding_hint=Native，宽松契约摘要）
pub fn official_entry(
    name: &str,
    sensitive: bool,
    description: &str,
    now: &str,
) -> ServiceCatalogEntry {
    ServiceCatalogEntry {
        service_name: name.to_string(),
        version: default_version(),
        description: Some(description.to_string()),
        io_contract: Some(IoContract {
            r#in: Some(serde_json::json!({"$comment": format!("{name} 输入契约见执行侧实现")})),
            out: Some(serde_json::json!({"$comment": format!("{name} 输出契约见执行侧实现")})),
        }),
        sensitive,
        binding_hint: BindingHint::Native,
        managed_by: "official".to_string(),
        scope: "platform".to_string(),
        created_at: now.to_string(),
        updated_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 嵌入副本守卫（泛化至多插件聚合）：各副本可解析且 schema
    /// 合法（字段齐全、非空、name 全局唯一、聚合序 = 登记表声明序）。
    /// 与源仓 SSOT 的内容一致性由同步脚本
    /// （evorule-server `scripts/sync-native-services.ps1`：复制 + 双侧守卫）保证。
    #[test]
    fn test_embedded_native_services_copy_valid() {
        let seed = official_native_services();
        assert!(!seed.is_empty(), "聚合种子不应为空");
        let mut names: Vec<String> = Vec::new();
        for (name, _, description) in &seed {
            assert!(!name.is_empty(), "存在空 name 条目");
            assert!(!description.is_empty(), "{name} 缺 description");
            names.push(name.clone());
        }
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "服务名重复 — 嵌入副本与 SSOT 漂移,请重新同步"
        );
        // 顺序锁定:聚合种子必须含各插件声明表首服务,且按登记表声明序排列
        // (demo-services 先于 physics-services,与执行侧挂载链同源)。
        let expect_heads = ["inverse_kinematics_solver", "physics_simulate"];
        let mut pos = 0usize;
        for head in expect_heads {
            let found = names[pos..]
                .iter()
                .position(|n| n == head)
                .unwrap_or_else(|| panic!("聚合种子缺插件首服务 {head} — 嵌入副本漂移,请重新同步"));
            pos += found;
            assert_eq!(
                names[pos], head,
                "聚合序漂移: {head} 应按登记表声明序出现,请重新同步"
            );
            pos += 1;
        }
    }

    /// 快照守卫：聚合种子与仓内字面量期望表**逐条全等**
    /// （name + sensitive + description + 顺序）。
    ///
    /// 背景：本文件曾只断言 name 唯一性与 2 个 head 顺序，
    /// `sensitive`/`description`/非 head 条目的增删改处于**零守卫**状态——
    /// 而 `sensitive` 是治理权威字段（驱动「敏感服务禁直调、须走审批链」的 403 门禁），
    /// SSOT 改动后忘记跑同步脚本 → 治理侧目录**静默低报敏感性**，所有测试仍全绿。
    ///
    /// 本测试把期望表固化为代码：任何副本改动（含正常同步）都会让本测试红，
    /// 强制开发者**显式**更新期望表并随改动一同评审——守卫从「宣称存在」变为真实存在。
    /// 同步流程：SSOT 改动 → 跑 sync-native-services.ps1 → 本测试红 → 按新副本更新
    /// [`EXPECTED_SERVICES`] → 测试绿。SSOT 与副本漂移另由脚本 `--verify` 模式 + CI job 把关。
    #[test]
    fn test_embedded_native_services_snapshot() {
        /// 快照期望表（聚合序 = EMBEDDED_SERVICE_FILES 声明序：
        /// demo-services → physics-services → indicator-services）。
        /// 条目 = (name, sensitive, description)，与 SSOT 声明文件逐字对应。
        const EXPECTED_SERVICES: &[(&str, bool, &str)] = &[
            // ---- demo-services ----
            ("inverse_kinematics_solver", false, "机器人逆运动学求解(Phase 1 原生)"),
            ("robot_move_joints", false, "机器人关节移动(确定性,Phase 1 原生)"),
            ("llm_advisor", true, "LLM 建议服务(sensitive:涉及外部 LLM API)"),
            ("shadow_ik_solver", false, "影子 IK 求解(对照验证)"),
            ("sampling_service", false, "采样服务"),
            ("rule_sandbox", false, "规则沙箱验证服务"),
            ("config_persist", false, "规则热加载持久化服务"),
            // ---- physics-services ----
            ("physics_simulate", false, "确定性物理仿真推进(刚体+辛积分器,vendored rpsm-core 内核)"),
            ("physics_energy", false, "物理系统总机械能计算(确定性)"),
            ("physics_grav_band", false, "有界重力带(分层势场)仿真推进与逃逸判定(确定性)"),
            // ---- indicator-services ----
            ("indicator_sma", false, "简单移动平均 SMA(窗口 N,pandas rolling(N).mean() 语义,warmup 期 null)"),
            ("indicator_ema", false, "指数移动平均 EMA(span N,pandas ewm(span=N, adjust=False) 语义,递推逐位对齐)"),
            ("indicator_macd", false, "MACD 快慢线与柱(默认 12/26/9,三组 ewm(span, adjust=False) 组合)"),
            ("indicator_rsi", false, "RSI(默认 14,Wilder 平滑 ewm(alpha=1/N, adjust=False),分类语义与参考实现逐分支对齐)"),
        ];

        let seed = official_native_services();
        assert_eq!(
            seed.len(),
            EXPECTED_SERVICES.len(),
            "条目数漂移 — 嵌入副本与快照期望表不一致：\
             若为正常同步请更新 EXPECTED_SERVICES 并评审；否则运行 \
             evorule-server scripts/sync-native-services.ps1 重新同步"
        );
        for (i, ((name, sensitive, description), (exp_name, exp_sensitive, exp_desc))) in
            seed.iter().zip(EXPECTED_SERVICES.iter()).enumerate()
        {
            assert_eq!(
                name, exp_name,
                "第 {i} 条服务名漂移 — 请重新同步或显式更新快照期望表"
            );
            assert_eq!(
                sensitive, exp_sensitive,
                "服务 {name} 的 sensitive 标记漂移 — sensitive 是治理权威字段\
                 （驱动敏感服务 403 直调门禁），任何变更必须显式评审快照期望表"
            );
            assert_eq!(
                description, exp_desc,
                "服务 {name} 的 description 漂移 — 请重新同步或显式更新快照期望表"
            );
        }
        // 权威标记显式断言（C6）：治理侧 403 门禁依赖此值，漂移即红。
        assert!(
            seed.iter().any(|(n, s, _)| n == "llm_advisor" && *s),
            "llm_advisor 必须 sensitive=true（C6 治理权威）"
        );
    }
}
