//! 认证与用户身份数据模型（43 号 正交 A）
//!
//! MVP 定案（43 号 §11，2026-08-22）：
//! - 单租户实例：每实例一个 `Tenant`，数据 SQL 层 tenant_id 隔离（多租户切换后置）；
//! - 四角色递进：查看者 ⊆ 规则工程师 ⊆ 审批者 ⊆ 管理员；
//! - 发布者 = 复用审批者 + 二次确认（不设独立发布者角色）；
//! - token：access 15min / refresh 30d，HS256 单密钥（生产级换 RS256，45 号批次 1）。
//!
//! **双层租户升级（2026-08-31 用户裁定，数据治理攻坚 B1）**：
//! - 一个部署实例 = 一个 platform（平台层，原 Tenant 语义）；platform 下若干 organization
//!   （org，数据隔离与协作单元）；
//! - **wire/存储字段名 `tenant_id` 不变，语义平移为 org id**（存量库零迁移；platform 层
//!   由 tenants 表承载，org 层由新表 orgs 承载）；
//! - 角色随 org 成员关系（user_org_memberships），同一用户可在不同 org 有不同角色；
//!   旧用户（无成员行）在其默认 org（users.tenant_id）回退 users.role，行为不变；
//! - 平台管理员（PlatformAdmin）：仅平台层动作（org 建/管/成员指派），org 内动作按
//!   rank 递进同样可执行（rank 5 ≥ admin 4）；
//! - org 内四角色递进不变。

use serde::{Deserialize, Serialize};

/// 组织租户（MVP 单租户实例：每实例一个）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tenant {
    pub tenant_id: String,
    pub name: String,
    /// 实例真实身份（39 号 §2，白标不掩盖来源，进溯源）
    pub instance_id: String,
    pub created_at: String,
}

/// 组织（org，B1 双层租户：platform 下的数据隔离与协作单元）
///
/// 命名说明：历史上叫"租户"，双层化后 org 层承接原租户的数据隔离语义；
/// `tenants` 表保留为 platform 注册表（每实例一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Org {
    pub org_id: String,
    pub name: String,
    /// 停用的 org 拒绝新登录/刷新（存量 token 到期自然失效）
    pub disabled: bool,
    pub created_at: String,
}

/// 用户-org 成员关系（B1：角色随成员关系，同一用户可跨 org 有不同角色）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserOrg {
    pub org_id: String,
    pub user_id: String,
    pub role: Role,
    pub created_at: String,
}

/// 组织内角色（四角色递进，43 号 §4 / 38 号 §4）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 查看者：检索/查看/拉取（限可见数据集）
    Viewer = 1,
    /// 规则工程师：创建 Draft、编辑、跑沙箱、提 Candidate
    RuleEngineer = 2,
    /// 审批者：Candidate→Active、发布（+二次确认）
    Approver = 3,
    /// 管理员：租户配置、服务注册、密钥管理、成员管理
    Admin = 4,
    /// 平台管理员（B1 双层租户）：平台层动作（org 建/管/成员指派）；
    /// org 内动作按 rank 递进（5 ≥ 4）同样可执行
    PlatformAdmin = 5,
}

impl Role {
    /// 从字符串解析（兼容反序列化之外的显式解析）
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "viewer" => Some(Role::Viewer),
            "rule_engineer" => Some(Role::RuleEngineer),
            "approver" => Some(Role::Approver),
            "admin" => Some(Role::Admin),
            "platform_admin" => Some(Role::PlatformAdmin),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::RuleEngineer => "rule_engineer",
            Role::Approver => "approver",
            Role::Admin => "admin",
            Role::PlatformAdmin => "platform_admin",
        }
    }
}

/// 动作集（43 号 §4 动作 → 角色映射，38 号 §5）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// 检索 / 查看
    View,
    /// 创建 Draft / 数据集
    Create,
    /// 编辑条目
    Edit,
    /// 跑沙箱（test）
    Test,
    /// 审批（Candidate→Active）
    Approve,
    /// 发布（public，独立发布审批，复用审批者+二次确认）
    Publish,
    /// 管理（租户配置/成员/密钥/服务注册）
    Admin,
    /// 平台管理（B1：org 建/停/成员指派，仅 PlatformAdmin）
    ManageOrgs,
}

impl Action {
    /// 该动作所需的最低角色等级（递进模型，43 号 §4）
    pub fn required_rank(&self) -> u8 {
        match self {
            Action::View => Role::Viewer as u8,
            Action::Create | Action::Edit | Action::Test => Role::RuleEngineer as u8,
            Action::Approve | Action::Publish => Role::Approver as u8,
            Action::Admin => Role::Admin as u8,
            Action::ManageOrgs => Role::PlatformAdmin as u8,
        }
    }
}

/// 角色是否允许该动作（递进判断）
pub fn can(role: Role, action: Action) -> bool {
    (role as u8) >= action.required_rank()
}

/// 是否 org 管理层（B1：admin 及以上含 platform_admin；替代旧 `role != Admin` 等值门控，
/// 使平台管理员在 org 内同样通过管理层门控）
pub fn is_org_admin(role: Role) -> bool {
    (role as u8) >= Role::Admin as u8
}

/// 用户（43 号 §2）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub user_id: String,
    pub tenant_id: String,
    pub username: String,
    /// PBKDF2-HMAC-SHA256 哈希（MVP；生产级 45 号换 Argon2id）
    pub password_hash: String,
    /// 随机盐（hex）
    pub salt: String,
    pub role: Role,
    pub disabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 认证审计记录（43 号 §6，only-append，与 34 号共用时间线语义）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthAudit {
    pub audit_id: String,
    /// 动作：register / login / refresh / logout / disable_user ...
    pub action: String,
    pub user_id: Option<String>,
    pub tenant_id: String,
    /// success | failure
    pub outcome: String,
    pub detail: Option<String>,
    pub created_at: String,
}

/// JWT 声明（43 号 §7，HS256，MVP）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenClaims {
    pub sub: String,
    pub tenant_id: String,
    pub role: String,
    #[serde(rename = "type")]
    pub token_type: String,
    pub iat: i64,
    pub exp: i64,
    /// 唯一 token id（43 号 §3.3）：登出/撤销按 jti 拉黑，防刷新旋转后旧 token 续用
    pub jti: String,
}

/// API Key（44 号 §14 定案：MVP 提供最小 scope 版，执行侧拉取快照包联动需要）
///
/// 仅存 `key_hash`（SHA-256），不存明文；`scope`：
/// - `pull`（MVP 最小版）：只读拉取（本租户/public 数据集快照包，执行侧联动）；
/// - 完整 scope/轮换/撤销告警 后置（44 号 §14-2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKey {
    pub key_id: String,
    pub tenant_id: String,
    pub name: String,
    /// pull | （后置扩展）
    pub scope: String,
    pub key_hash: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}
