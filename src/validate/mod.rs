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

    /// law_set 条目结构校验（定律集条目准入结构面，纯函数零依赖）：
    ///
    /// - 九要素必填（机器闸准入从严口径）：formula / symbols / si_units / domain /
    ///   solvable / derivation / tier，外加 max_version_depth（有限性声明，整数 ≥1）
    ///   与 monotone_form（单调形态声明，值须为 true），再加条目 id（域 schema
    ///   required 面）；
    /// - 公式仅允许「等式恒真」语义：恰好一个等式分隔符（`==` 或 `=`），两侧非空
    ///   （裸 `=` = 分隔符缺侧，显式拒绝）；缺分隔符/多分隔符同样拒绝；
    /// - condition/then 判定语义属 rule_set 分野，由 law 载荷不含 transform 结构保证
    ///   （域 schema additionalProperties=false 同向收紧）。
    ///
    /// 载荷 schema 级校验走 D3 领域 schema resolver（law_set 域件）；本方法为机器闸
    /// M1 分支的准入结构面，从严于 schema required 面（九要素全数在场）。
    pub fn validate_law_structure(payload: &serde_json::Value) -> Result<(), ValidationError> {
        let obj = payload
            .as_object()
            .ok_or_else(|| ValidationError::Message("law 条目载荷须为 JSON 对象".into()))?;

        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ValidationError::Message("law 条目缺 id（或非字符串）".into()))?;
        let id_ok = {
            let mut chars = id.chars();
            match chars.next() {
                Some(c) if c.is_ascii_lowercase() => {
                    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                }
                _ => false,
            }
        };
        if !id_ok {
            return Err(ValidationError::Message(format!(
                "law 条目 id 非法（须 ^[a-z][a-z0-9_]*$）: {id}"
            )));
        }

        let formula = obj
            .get("formula")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ValidationError::Message("law 条目缺 formula（或非字符串）".into()))?;
        Self::validate_law_formula(formula)?;

        let symbols = obj
            .get("symbols")
            .and_then(|v| v.as_object())
            .ok_or_else(|| ValidationError::Message("law 条目缺 symbols（或非对象）".into()))?;
        if symbols.len() < 2 {
            return Err(ValidationError::Message(format!(
                "law 条目 symbols 须至少 2 项（当前 {}）",
                symbols.len()
            )));
        }
        if symbols.values().any(|v| !v.is_string()) {
            return Err(ValidationError::Message(
                "law 条目 symbols 值须全为字符串".into(),
            ));
        }

        let si_units = obj
            .get("si_units")
            .and_then(|v| v.as_object())
            .ok_or_else(|| ValidationError::Message("law 条目缺 si_units（或非对象）".into()))?;
        if si_units
            .values()
            .any(|v| v.as_str().map(|s| s.trim().is_empty()).unwrap_or(true))
        {
            return Err(ValidationError::Message(
                "law 条目 si_units 值须全为非空字符串（SI 单位表达式）".into(),
            ));
        }

        let domain = obj
            .get("domain")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().is_empty())
            .unwrap_or(true);
        if domain {
            return Err(ValidationError::Message(
                "law 条目缺 domain（或非非空字符串）".into(),
            ));
        }

        for key in ["solvable", "derivation"] {
            match obj.get(key).and_then(|v| v.as_array()) {
                None => {
                    return Err(ValidationError::Message(format!(
                        "law 条目缺 {key}（或非数组）"
                    )))
                }
                Some(items) => {
                    if items.iter().any(|v| !v.is_string()) {
                        return Err(ValidationError::Message(format!(
                            "law 条目 {key} 项须全为字符串"
                        )));
                    }
                }
            }
        }

        match obj.get("tier").and_then(|v| v.as_str()) {
            Some(t) if t == "light" || t == "full" => {}
            _ => {
                return Err(ValidationError::Message(
                    "law 条目缺 tier（或非 light|full 枚举值）".into(),
                ))
            }
        }

        // 有限性声明（v1.0 原位加必填）：max_version_depth 须为整数且 ≥1
        match obj.get("max_version_depth") {
            Some(v) if v.as_u64().is_some_and(|n| n >= 1) => {}
            Some(_) => {
                return Err(ValidationError::Message(
                    "law 条目 max_version_depth 须为整数且 ≥1（有限性声明：版本链高度下界为 1）"
                        .into(),
                ))
            }
            None => {
                return Err(ValidationError::Message(
                    "law 条目缺 max_version_depth（或非整数；有限性声明必填且 ≥1）".into(),
                ))
            }
        }

        // 单调形态声明（v1.0 原位加必填）：值必须为 true（false 一律拒收）
        match obj.get("monotone_form") {
            Some(serde_json::Value::Bool(true)) => {}
            Some(_) => {
                return Err(ValidationError::Message(
                    "law 条目 monotone_form=false：非单调形态律式拒收（须以时间锚定改写重交）"
                        .into(),
                ))
            }
            None => {
                return Err(ValidationError::Message(
                    "law 条目缺 monotone_form（或非布尔；单调形态声明必填且须为 true）".into(),
                ))
            }
        }

        // 否定类构造静态扫描（与 evorule-server core/law_schema 同款纯函数，
        // golden 向量双仓锁定防漂移）：命中且无 time_anchor 非空声明 → 拒收；
        // 命中但 time_anchor 非空 → 放行；未命中 → 放行。
        let hits = scan_negation_constructs(formula);
        if !hits.is_empty() {
            let anchored = obj
                .get("time_anchor")
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if !anchored {
                return Err(ValidationError::Message(format!(
                    "law 条目 (id={id}): formula 含否定类构造（{}，位置 {}：`{}`）；如属时间锚定改写请声明 time_anchor，否则拒收",
                    hits[0].kind, hits[0].pos, hits[0].snippet
                )));
            }
        }
        Ok(())
    }

    /// law v1.1 intake 纪律面（发布链生产者预检，语料吸收原则 3/7 落地面）：
    /// 从 [`Self::validate_law_structure`]（九要素从严面）起步，额外要求
    /// form（形态四型，闸面按形态选判据）与 intake_channel（出身通道，审计事实
    /// 永不回写，与条目信任级正交——出身不因信任级晋升而洗白）。
    /// schema 级权威=law_set/v1.1 域件（data/domain_schemas/law_set-v1.1.json，
    /// SSOT=system-rules schemas/law_set/v1.1.json）；本面从严于 schema required，
    /// 供发布链转换器在产出 v1.1 文档前逐条自检。
    pub fn validate_law_intake_v11(payload: &serde_json::Value) -> Result<(), ValidationError> {
        Self::validate_law_structure(payload)?;
        let obj = payload
            .as_object()
            .expect("validate_law_structure 已保证载荷为对象");
        const LAW_FORMS: [&str; 4] = ["equation_identity", "constraint", "procedure", "definition"];
        const INTAKE_CHANNELS: [&str; 2] = ["human_curated", "llm_proposal"];
        let form = obj.get("form").and_then(|v| v.as_str()).ok_or_else(|| {
            ValidationError::Message(
                "law v1.1 条目缺 form（形态四型必填：equation_identity|constraint|procedure|definition）"
                    .into(),
            )
        })?;
        if !LAW_FORMS.contains(&form) {
            return Err(ValidationError::Message(format!(
                "law v1.1 条目 form 非法（须形态四型枚举值）: {form}"
            )));
        }
        let channel =
            obj.get("intake_channel")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ValidationError::Message(
                "law v1.1 条目缺 intake_channel（出身通道必填：human_curated|llm_proposal）".into(),
            )
                })?;
        if !INTAKE_CHANNELS.contains(&channel) {
            return Err(ValidationError::Message(format!(
                "law v1.1 条目 intake_channel 非法（须出身通道枚举值）: {channel}"
            )));
        }
        Ok(())
    }

    /// law 公式等式恒真结构检查：恰好一个等式分隔符（`==`/`=` 归一后），两侧非空。
    /// 裸 `=`（分隔符缺侧）、缺分隔符、多分隔符均显式拒绝（不静默放行）。
    fn validate_law_formula(formula: &str) -> Result<(), ValidationError> {
        let f = formula.trim();
        if f.chars().count() < 3 {
            return Err(ValidationError::Message(format!(
                "law 公式过短（minLength 3）: {formula}"
            )));
        }
        let normalized = f.replace("==", "=");
        let parts: Vec<&str> = normalized.split('=').collect();
        match parts.len() {
            2 => {
                let (lhs, rhs) = (parts[0].trim(), parts[1].trim());
                if lhs.is_empty() || rhs.is_empty() {
                    Err(ValidationError::Message(format!(
                        "law 公式裸 `=`（等式分隔符一侧为空，等式恒真语义须两侧非空）: {formula}"
                    )))
                } else {
                    Ok(())
                }
            }
            1 => Err(ValidationError::Message(format!(
                "law 公式缺等式分隔符（等式恒真语义须 LHS=RHS 形态）: {formula}"
            ))),
            _ => Err(ValidationError::Message(format!(
                "law 公式含多个等式分隔符（等式恒真语义须恰好一个）: {formula}"
            ))),
        }
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

// ============================================================================
// formula 否定类构造静态扫描（与 evorule-server core/law_schema 同款纯函数；
// SSOT=server 侧，本仓为消费副本，G19/G20 golden 向量双仓锁定防漂移）
// ============================================================================

/// 否定类构造扫描命中（独立纯函数返回值，便于单测与报因回显）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegationHit {
    /// 命中形态：`unary_minus`（一元负号）或 `logical_not`（逻辑否定记号）
    pub kind: &'static str,
    /// 命中位置（formula 内字符偏移，0 起）
    pub pos: usize,
    /// 命中片段（含少量上下文，供报因回显）
    pub snippet: String,
}

/// 一元负号的前缀字符（减号前最近非空白字符必须是这些之一或行首）；
/// `a - b` 二元减法（前缀是标识符）与 `-3.14` 负数字面量（后随数字）不命中。
const UNARY_MINUS_PRECEDERS: [char; 9] = ['=', '(', '+', '*', '/', '^', '<', '>', ','];

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// 命中片段截取（前后各取少量上下文，越界端加省略号）
fn negation_snippet(chars: &[char], pos: usize, width: usize) -> String {
    let start = pos.saturating_sub(width);
    let end = (pos + width).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

/// formula 否定类构造静态扫描（独立纯函数，便于单测）：
///
/// 两类命中形态——
/// ① 一元负号作用于标识符或括号组：减号前最近非空白字符是行首/运算符/分隔符
///    （`=(+*/^<>,`），且减号后（跳过空白）紧跟标识符字母或左括号；
///    `-3.14` 负数字面量与 `a - b` 二元减法不命中；
/// ② 逻辑否定记号：词边界 `not`、`¬`。
pub fn scan_negation_constructs(formula: &str) -> Vec<NegationHit> {
    let chars: Vec<char> = formula.chars().collect();
    let mut hits = Vec::new();
    for i in 0..chars.len() {
        if chars[i] == '-' {
            // 前瞻：减号后跳过空白须紧跟标识符字母或左括号
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            let next_ok = next.is_some_and(|c| c.is_ascii_alphabetic() || *c == '(');
            if !next_ok {
                continue;
            }
            // 回溯：减号前最近非空白字符须为行首/运算符/分隔符
            let unary = chars[..i]
                .iter()
                .rev()
                .find(|c| !c.is_whitespace())
                .map(|c| UNARY_MINUS_PRECEDERS.contains(c))
                .unwrap_or(true);
            if unary {
                hits.push(NegationHit {
                    kind: "unary_minus",
                    pos: i,
                    snippet: negation_snippet(&chars, i, 8),
                });
            }
        } else if chars[i] == '¬' {
            hits.push(NegationHit {
                kind: "logical_not",
                pos: i,
                snippet: negation_snippet(&chars, i, 8),
            });
        } else if chars[i] == 'n'
            && chars[i..].len() >= 3
            && chars[i + 1] == 'o'
            && chars[i + 2] == 't'
        {
            let before_ok = i == 0 || !is_word_char(chars[i - 1]);
            let after_idx = i + 3;
            let after_ok = after_idx >= chars.len() || !is_word_char(chars[after_idx]);
            if before_ok && after_ok {
                hits.push(NegationHit {
                    kind: "logical_not",
                    pos: i,
                    snippet: negation_snippet(&chars, i, 8),
                });
            }
        }
    }
    hits
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

    // ---------- law 条目结构校验（law_set 条目准入结构面） ----------

    fn law_payload() -> serde_json::Value {
        serde_json::json!({
            "id": "newton_second_law",
            "formula": "F = m * a",
            "symbols": {"F": "force", "m": "mass", "a": "acceleration"},
            "si_units": {"F": "kg*m*s^-2", "m": "kg", "a": "m*s^-2"},
            "domain": "mechanics",
            "solvable": ["F", "m", "a"],
            "derivation": ["second-order motion law in inertial frames"],
            "tier": "full",
            "max_version_depth": 1,
            "monotone_form": true
        })
    }

    // ---------- law v1.1 intake 纪律面（发布链生产者预检） ----------

    fn law_payload_v11() -> serde_json::Value {
        let mut p = law_payload();
        let obj = p.as_object_mut().unwrap();
        obj.insert(
            "form".into(),
            serde_json::Value::String("equation_identity".into()),
        );
        obj.insert(
            "intake_channel".into(),
            serde_json::Value::String("llm_proposal".into()),
        );
        p
    }

    #[test]
    fn test_law_intake_v11_compliant_passes() {
        Validator::validate_law_intake_v11(&law_payload_v11()).unwrap();
        let mut human = law_payload_v11();
        human["intake_channel"] = serde_json::Value::String("human_curated".into());
        Validator::validate_law_intake_v11(&human).unwrap();
        let mut constraint = law_payload_v11();
        constraint["form"] = serde_json::Value::String("constraint".into());
        Validator::validate_law_intake_v11(&constraint).unwrap();
    }

    #[test]
    fn test_law_intake_v11_v10_shape_rejected() {
        // v1.0 形态（无 form/intake_channel）在 v1.1 面必须拒收——纪律增量生效判据
        let err = Validator::validate_law_intake_v11(&law_payload())
            .expect_err("v1.0 形态缺 form/intake_channel 应拒收");
        assert!(err.to_string().contains("form"));
    }

    #[test]
    fn test_law_intake_v11_bad_enum_rejected() {
        let mut bad_form = law_payload_v11();
        bad_form["form"] = serde_json::Value::String("textbook_chapter".into());
        assert!(Validator::validate_law_intake_v11(&bad_form).is_err());

        let mut missing_channel = law_payload_v11();
        missing_channel
            .as_object_mut()
            .unwrap()
            .remove("intake_channel");
        let err = Validator::validate_law_intake_v11(&missing_channel)
            .expect_err("缺 intake_channel 应拒收");
        assert!(err.to_string().contains("intake_channel"));

        let mut bad_channel = law_payload_v11();
        bad_channel["intake_channel"] = serde_json::Value::String("scraped".into());
        assert!(Validator::validate_law_intake_v11(&bad_channel).is_err());
    }

    #[test]
    fn test_law_structure_golden_pass() {
        // 黄金正例：等式恒真公式 + 九要素齐备
        Validator::validate_law_structure(&law_payload()).unwrap();
        let mut einstein = law_payload();
        einstein["id"] = serde_json::json!("mass_energy_equivalence");
        einstein["formula"] = serde_json::json!("E = m * c^2");
        Validator::validate_law_structure(&einstein).unwrap();
        let mut gas = law_payload();
        gas["id"] = serde_json::json!("ideal_gas_law");
        gas["formula"] = serde_json::json!("P * V = n * R * T");
        Validator::validate_law_structure(&gas).unwrap();
        // `==` 分隔符同型放行（等式恒真语义等价形态）
        let mut eq_eq = law_payload();
        eq_eq["formula"] = serde_json::json!("F == m * a");
        Validator::validate_law_structure(&eq_eq).unwrap();
    }

    #[test]
    fn test_law_structure_negation_unary_minus_without_time_anchor_rejected() {
        // G19 镜像（与 evorule-server core/law_schema 同向量）：formula 含一元负号
        // 作用标识符且无 time_anchor → 拒收（报因含否定类构造措辞与命中片段回显）
        let mut p = law_payload();
        p["formula"] = serde_json::json!("v = -a * t");
        p["symbols"] = serde_json::json!({"v": "velocity", "a": "acceleration", "t": "time"});
        p["si_units"] = serde_json::json!({"v": "m*s^-1", "a": "m*s^-2", "t": "s"});
        let err =
            Validator::validate_law_structure(&p).expect_err("G19 一元负号无 time_anchor 应拒收");
        let msg = format!("{err}");
        assert!(
            msg.contains("否定类构造") && msg.contains("time_anchor") && msg.contains("-a"),
            "报因须含否定类构造措辞、时间锚定指引与命中片段: {msg}"
        );
    }

    #[test]
    fn test_law_structure_negation_unary_minus_with_time_anchor_passes() {
        // G20 镜像：同 formula + time_anchor 非空 → 通过；仅空白 → 仍按未声明拒收
        let mut p = law_payload();
        p["formula"] = serde_json::json!("v = -a * t");
        p["symbols"] = serde_json::json!({"v": "velocity", "a": "acceleration", "t": "time"});
        p["si_units"] = serde_json::json!({"v": "m*s^-1", "a": "m*s^-2", "t": "s"});
        p["time_anchor"] = serde_json::json!("anchored at t0 = 0 (time-anchored rewrite)");
        Validator::validate_law_structure(&p).unwrap();
        let mut blank = p.clone();
        blank["time_anchor"] = serde_json::json!("   ");
        assert!(
            Validator::validate_law_structure(&blank).is_err(),
            "time_anchor 仅空白应视为未声明而拒收"
        );
    }

    #[test]
    fn test_negation_scan_boundary_forms() {
        // 扫描器边界形态（与 server 侧同向量）：命中/不命中各型
        // 命中：行首/运算符/分隔符前缀 + 标识符或左括号后随
        for (formula, kind, pos) in [
            ("v = -a * t", "unary_minus", 4),
            ("x = (-k) * y", "unary_minus", 5),
            ("y = a^(-b) * c", "unary_minus", 7),
        ] {
            let hits = scan_negation_constructs(formula);
            assert_eq!(hits.len(), 1, "`{formula}` 应恰命中一处");
            assert_eq!(hits[0].kind, kind);
            assert_eq!(hits[0].pos, pos);
        }
        // 不命中：负数字面量 / 二元减法 / 词中 not
        for formula in [
            "y = 2 - a * t",
            "x = -3.14 * r",
            "v = a * t - b",
            "x = n * other",
        ] {
            assert!(
                scan_negation_constructs(formula).is_empty(),
                "`{formula}` 不应命中"
            );
        }
        // 逻辑否定记号：词边界 not / ¬ 命中
        let hits = scan_negation_constructs("y = not(a) * b");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "logical_not");
        assert_eq!(scan_negation_constructs("¬(a) = b").len(), 1);
    }

    #[test]
    fn test_law_structure_bare_equals_rejected() {
        // 裸 `=`（分隔符缺侧）显式拒绝；缺分隔符/多分隔符同拒
        for formula in ["= m * a", "F =", "F ==", "m * a", "a = b = c"] {
            let mut p = law_payload();
            p["formula"] = serde_json::json!(formula);
            let err = Validator::validate_law_structure(&p)
                .expect_err(&format!("formula `{formula}` 应被拒"));
            let msg = format!("{err}");
            assert!(
                msg.contains("公式"),
                "formula `{formula}` 应以公式语义拒绝: {msg}"
            );
        }
    }

    #[test]
    fn test_law_structure_nine_elements_required() {
        // 九要素缺一即拒（机器闸准入从严口径；七要素 + max_version_depth + monotone_form）
        for key in [
            "formula",
            "symbols",
            "si_units",
            "domain",
            "solvable",
            "derivation",
            "tier",
            "max_version_depth",
            "monotone_form",
        ] {
            let mut p = law_payload();
            p.as_object_mut().unwrap().remove(key);
            let err = Validator::validate_law_structure(&p).expect_err(&format!("缺 {key} 应被拒"));
            assert!(format!("{err}").contains("law 条目"), "{err}");
        }
        // id 缺失同拒
        let mut p = law_payload();
        p.as_object_mut().unwrap().remove("id");
        assert!(Validator::validate_law_structure(&p).is_err());
    }

    #[test]
    fn test_law_structure_field_shapes_rejected() {
        // id 模式 / symbols 项数 / si_units 非字符串 / tier 枚举 / 新必填字段
        // 非法形态（max_version_depth=0 / monotone_form=false）/ 非对象载荷
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (serde_json::json!("Bad_Id"), "id"),
            (serde_json::json!("9law"), "id"),
            (serde_json::json!({"F": "force"}), "symbols"), // 仅 1 项
            (serde_json::json!({"F": "force", "m": 2}), "symbols"),
            (
                serde_json::json!({"F": "", "m": "kg", "a": "m*s^-2"}),
                "si_units",
            ),
            (serde_json::json!("medium"), "tier"),
            (serde_json::json!(0), "max_version_depth"), // 有限性下界
            (serde_json::json!("high"), "max_version_depth"), // 非整数
            (serde_json::json!(false), "monotone_form"), // 非单调形态拒收
        ];
        for (value, key) in cases {
            let mut p = law_payload();
            p[key] = value;
            assert!(
                Validator::validate_law_structure(&p).is_err(),
                "字段 {key} 非法形态应被拒"
            );
        }
        assert!(Validator::validate_law_structure(&serde_json::json!("not-an-object")).is_err());
    }
}
