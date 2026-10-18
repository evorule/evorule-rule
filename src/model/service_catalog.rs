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
    (
        "template-services",
        include_str!("official_native_services.template-services.embedded.json"),
    ),
    (
        "inference-services",
        include_str!("official_native_services.inference-services.embedded.json"),
    ),
    (
        "geometry-services",
        include_str!("official_native_services.geometry-services.embedded.json"),
    ),
    (
        "numeric-services",
        include_str!("official_native_services.numeric-services.embedded.json"),
    ),
    (
        "mechanics-services",
        include_str!("official_native_services.mechanics-services.embedded.json"),
    ),
    (
        "gnss-services",
        include_str!("official_native_services.gnss-services.embedded.json"),
    ),
    (
        "grav-services",
        include_str!("official_native_services.grav-services.embedded.json"),
    ),
    (
        "checksum-services",
        include_str!("official_native_services.checksum-services.embedded.json"),
    ),
    (
        "cny-services",
        include_str!("official_native_services.cny-services.embedded.json"),
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
        /// demo-services → physics-services → indicator-services → template-services
        /// → inference-services → geometry-services → numeric-services → mechanics-services）。
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
            // ---- template-services ----
            ("template_render", false, "模板渲染(上下文+模板→JSON/Markdown/纯文本,{{}} 家族语法,if/for 最小集,确定性纯函数)"),
            // ---- inference-services ----
            ("inference_dimension_from_unit", false, "SI 单位字符串解析为七分量量纲向量([L,M,T,I,Θ,N,J],确定性)"),
            ("inference_dimension_compare_units", false, "两 SI 单位量纲等价性比较(七分量向量逐位相等判定,确定性)"),
            ("inference_dimension_verify_formula", false, "物理公式量纲一致性验证(已知变量量纲推导四则与幂表达式,确定性)"),
            ("inference_rw_extract", false, "规则读写集静态提取(规范化路径+I/O出边+动态标注,纯函数确定性)"),
            ("inference_cycle_detect", false, "规则集内环路检测(Kahn拓扑+环路径提取,静态/保守分层,确定性)"),
            ("inference_conflict_detect", false, "规则集内冲突检测(六类型学:常量矛盾/顺序遮蔽/写竞争/互补域/循环写/IO串台,只报告不拦截)"),
            ("inference_forward_simulate", false, "前向链仿真(合成__exec__文档迭代到不动点,enforce前置门对齐,io诚实断流,确定性)"),
            ("inference_backward_plan", false, "后向链规划(目标路径反推生产者规则链,复用读写集提取,静态可达性,确定性)"),
            ("inference_effect_predict", false, "规则效果预测(影响分级+动态/IO诚实警告,基于读写集提取薄层,确定性)"),
            ("inference_convergence_check", false, "收敛检测(自修改/环复用/步数上界,静态可判定子集,undetermined显式,确定性)"),
            ("inference_counterfactual_analyze", false, "反事实差异分析(双前向仿真+终态可观测面diff,控制面单列,确定性解释,确定性)"),
            ("inference_equivalence_probe", false, "等价探针(结构比较+有限探针行为证据,probe_equivalent为证据语义非数学证明,确定性)"),
            ("inference_differential_diff", false, "规则集差分(读写集结构化diff,写异必不同/读异可能不同的保守方向,确定性)"),
            ("inference_counterfactual_sweep", false, "反事实参数扫描(参数路径逐值双仿真,每点差异面+汇总,复用反事实语义,确定性)"),
            // ---- geometry-services ----
            ("geometry_collision_check", false, "几何碰撞检测(零件两两相交判定,bbox/圆/多边形,触即算,确定性)"),
            ("geometry_clearance_check", false, "几何间隙检查(零件两两最小距离vs阈值,违规清单+全局最小,宽容方向容差,确定性)"),
            ("geometry_occlusion_check", false, "几何遮挡序检查(重叠零件绘制序vs声明z层,画家算法语义,确定性)"),
            // ---- numeric-services ----
            ("numeric_rational_eval", false, "有理数表达式精确求值(整数与四则/整数幂,支持 vars 变量绑定,既约分数输出,确定性)"),
            ("numeric_decimal_eval", false, "定点十进制表达式求值(支持 vars 变量绑定,全程精确有理运算,输出端按 scale+rounding 单次舍入,确定性)"),
            ("numeric_decimal_convert", false, "分数与十进制互转(终止性如实标注,无限小数须显式舍入,确定性)"),
            ("numeric_number_theory", false, "数论原子(gcd/lcm/factorial/nCr/nPr,大整数精确,确定性)"),
            ("numeric_fin_interest", false, "单利/复利精确计算(中间量全程有理数精确,末端显式舍入,确定性)"),
            ("numeric_fin_amortize", false, "等额本息/等额本金摊销表(逐期显式舍入,尾差并入末期,确定性)"),
            ("numeric_unit_convert", false, "单位换算(换算率由调用方显式提供,服务不内置任何率,有理数中转末端舍入,确定性)"),
            ("numeric_dim_eval", false, "量纲感知表达式求值(单位字面量解析+量纲检查+精确求值,复用量纲原语,确定性)"),
            // ---- mechanics-services ----
            ("mechanics_newton_second", false, "牛顿第二定律 F=m·a 三量互求与一致性校验(精确有理数,确定性)"),
            ("mechanics_kinematics_uniform", false, "匀加速运动学互求(v0/a/t/s/v 任三独立量解其余,双根组合如实拒解,确定性)"),
            ("mechanics_momentum_impulse", false, "动量与冲量互求(p=m·v 与 I=F·t,精确有理数,确定性)"),
            ("mechanics_energy_mech", false, "机械能计算(Ek=m·v²/2 与 Ep=m·g·h,g 显式入参零内置常数,确定性)"),
            ("mechanics_ohm_law", false, "欧姆定律 U=I·R 三量互求与一致性校验(精确有理数,确定性)"),
            ("mechanics_resistance_network", false, "电阻网络等效(串联Σ/并联倒数和,有理数精确,短路如实报告,确定性)"),
            ("mechanics_pid_step", false, "离散 PID 单步(位置式,状态显式出入无隐藏状态,双跑位元一致,确定性)"),
            ("gnss_factory_prebias", false, "GNSS 工厂频率预偏置换算(标称/设偏/分数偏置三量互求+每日漂移纳秒换算,精确有理数,确定性)"),
            ("gnss_range_error", false, "GNSS 时间误差与距离误差换算(c=SI 精确定义内置带版本戳,精确有理数,确定性)"),
            ("grav_schwarzschild_radius", false, "史瓦西半径 rs=2GM/c(G 显式入参,c 为 SI 精确定义内置,精确有理数,确定性)"),
            ("grav_perihelion_precession", false, "近日点进动 Delta_phi=3888000GM/(c^2*a*(1-e^2)) 弧秒式精确(水星 43 角秒/世纪经典验证;弧度输出用 pi 的 50 位有理近似带版本戳,确定性)"),
            ("grav_light_deflection", false, "光线偏折 alpha=4GM/(c^2*b)(太阳掠射 1.75 角秒经典验证;角秒/弧度中 pi 用 50 位有理近似带版本戳,弧度本体精确,确定性)"),
            ("checksum_luhn", false, "Luhn 模 10 校验(银行卡号/IMEI 等;validate 校验/append 补位,纯整数,确定性)"),
            ("checksum_isbn", false, "ISBN 校验(isbn10 模 11 含 X 位/isbn13 交替权 1-3;validate 校验/check_digit 补位,纯整数,确定性)"),
            ("checksum_iban", false, "IBAN mod-97 校验(ISO 13616;validate 校验/check_digits 补算两位校验码;结构+mod97,国家长度表不内置如实声明,确定性)"),
            ("cny_amount_upper", false, "人民币大写金额转换(票据规范:零合并/整字/角分规则;负数与超 16 位整数如实拒解;规范两可处以省略零为确定性选择并注明,确定性)"),
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
