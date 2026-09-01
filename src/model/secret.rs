//! 密钥抽象层（45 号 §3 密钥管理 · K1，MVP 档）
//!
//! # 对齐 45 号 §3 / §6 定案（2026-08-22）
//! MVP 档位（§6 定案）：本地密钥 + 环境/代码内注入，**明确标注非生产**；双密钥轮换（active+previous）
//! 保持 **HS256 对称密钥**（§6：JWT MVP 维持 HS256，RS256 非对称 → Vault 为生产项，如实 deferred）。
//! 静态加密 / DEK-KEK 信封（§3.1 DEK 行）、Vault/KMS、RS256+jwks 均属生产级后续，**本档不实现**。
//!
//! # 结构
//! - `SecretScope`：密钥类（五类，45 号 §3.1 密钥分级）。
//! - `SecretKey`：单个密钥值 + 元信息（scope/代号/创建时间），**绝不泄露明文**（不 Display）。
//! - `KeyRing`：以 scope 为键的双代密钥容器（active + previous），提供轮换（rotate）与取用（access）。
//!
//! # 双代轮换语义（45 号 §3.3）
//! - 新数据用 active 加密/签名；旧数据（用 previous 加密/签名）仍可解/验（backward 兼容）；
//! - `rotate()`：生成新密钥 → 升为新 active；旧 active 降为 previous；旧 previous 丢弃；
//! - JWT：签发用 active，验签先试 active 再试 previous（§3.3 "旧公钥保留验证" 的对称版）。

use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::model::auth::AuthAudit;

/// 密钥类（45 号 §3.1 密钥分级）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretScope {
    /// JWT 签名密钥（access/refresh；MVP=HS256 对称）
    Jwt,
    /// 租户凭据（执行侧 service_registry 头 / 外部服务密钥，按租户隔离）
    TenantCredentials,
    /// API key 哈希盐（HKDF 派生盐，44 号 §10）
    ApiHashSalt,
    /// 数据加密密钥（静态加密，**MVP 后置**，此处仅占位）
    Dek,
    /// 数据库凭据（PostgreSQL 连接）
    DbCredentials,
}

impl SecretScope {
    pub fn key(&self) -> &'static str {
        match self {
            SecretScope::Jwt => "jwt",
            SecretScope::TenantCredentials => "tenant_credentials",
            SecretScope::ApiHashSalt => "api_hash_salt",
            SecretScope::Dek => "dek",
            SecretScope::DbCredentials => "db_credentials",
        }
    }
}

/// 单个密钥（含双代语义的元信息；明文不公开）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretKey {
    pub scope: SecretScope,
    /// 代号：0 = previous，1 = active（后续可扩代次，MVP 双代）
    pub generation: u8,
    /// 密钥值（字节，不落日志/审计）
    pub(crate) value: Vec<u8>,
    pub created_at: String,
}

/// 密钥环（KeyRing）：scope → (active, previous)，MVP 双代
#[derive(Debug, Clone)]
pub struct KeyRing {
    active: std::collections::HashMap<SecretScope, Vec<u8>>,
    previous: std::collections::HashMap<SecretScope, Vec<u8>>,
}

impl KeyRing {
    /// 空环
    pub fn empty() -> Self {
        Self {
            active: std::collections::HashMap::new(),
            previous: std::collections::HashMap::new(),
        }
    }

    /// 设 active 密钥（覆盖；不产生 previous）
    pub fn set_active(&mut self, scope: SecretScope, value: Vec<u8>) {
        self.active.insert(scope, value);
        // previous 保留（轮换时才会重建），此处仅设 active
    }

    /// 取 active 密钥字节（供调用方使用；不落日志）
    pub fn access_active(&self, scope: SecretScope) -> Option<&[u8]> {
        self.active.get(&scope).map(|v| v.as_slice())
    }

    /// 取 previous 密钥字节（供旧数据解/验）
    pub fn access_previous(&self, scope: SecretScope) -> Option<&[u8]> {
        self.previous.get(&scope).map(|v| v.as_slice())
    }

    /// 双代取用：优先 active，其次 previous（JWT 验签等 backward 兼容路径）
    pub fn access_any(&self, scope: SecretScope) -> Option<&[u8]> {
        self.access_active(scope)
            .or_else(|| self.access_previous(scope))
    }

    /// 轮换（双代，45 号 §3.3）：新 active = 生成；旧 active → previous；旧 previous 丢弃。
    ///
    /// - `generator`：生成新密钥字节（测试可注入固定，生产用安全随机）。
    /// - 返回审计记录（key.rotate，detail 含 scope/本轮前后 generation，**不含明文**）。
    pub fn rotate<F: FnOnce(SecretScope) -> Vec<u8>>(
        &mut self,
        scope: SecretScope,
        created_at: &str,
        generator: F,
    ) -> AuthAudit {
        let new_value = generator(scope);
        // 旧 active 降为 previous（即使之前有 previous 也丢弃——MVP 双代）
        if let Some(old_active) = self.active.get(&scope) {
            self.previous.insert(scope, old_active.clone());
        } else {
            self.previous.remove(&scope);
        }
        self.active.insert(scope, new_value);
        AuthAudit {
            audit_id: format!("kr_{}", uuidish()),
            action: "key.rotate".to_string(),
            user_id: None, // 系统级轮换；操作者由调用方审计层补充
            tenant_id: "system".to_string(),
            outcome: "success".to_string(),
            // 只记 scope/代次，不记明文（35 号 §6）
            detail: Some(format!(
                "scope={} previous={} active",
                scope.key(),
                if self.previous.contains_key(&scope) {
                    1
                } else {
                    0
                }
            )),
            created_at: created_at.to_string(),
        }
    }

    /// 是否含某 scope 的 active 密钥
    pub fn has_active(&self, scope: SecretScope) -> bool {
        self.active.contains_key(&scope)
    }

    /// scope 数（诊断）
    pub fn scope_count(&self) -> usize {
        self.active.len()
    }
}

/// 生成随机密钥字节（默认生成器，安全随机：rand::RngCore）
pub fn random_key_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

/// hex 编码（诊断/指纹，不涉明文唯一暴露）
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 简单 id（无 uuid crate 依赖，复用模式）
fn uuidish() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

/// 用密钥派生固定长盐（K3 API 盐导出：HKDF 风格单次提取，供 api_keys 哈希使用）。
/// `label` 保证不同用途的派生互相独立（JWT / api_hash_salt 等）。
pub fn derive_salt_from_secret(secret: &[u8], label: &str, out_len: usize) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"evorule-rule/secret-derive-v1");
    hasher.update(label.as_bytes());
    hasher.update(salt_separator());
    hasher.update(secret);
    let h = hasher.finalize();
    // SHA-256 32B；out_len ≤ 32 时截断即可（满足 API 盐应用）
    h[..out_len.min(h.len())].to_vec()
}

/// HKDF 风格：标签与会话盐间分隔符（约定值）
fn salt_separator() -> &'static [u8] {
    b"\x00|"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_scope_key_names() {
        assert_eq!(SecretScope::Jwt.key(), "jwt");
        assert_eq!(SecretScope::ApiHashSalt.key(), "api_hash_salt");
    }

    #[test]
    fn test_keyring_rotate_double_generation() {
        let mut ring = KeyRing::empty();
        let now = "2026-08-22T00:00:00Z";
        ring.rotate(SecretScope::Jwt, now, |_| vec![1, 2, 3]);
        assert_eq!(ring.access_active(SecretScope::Jwt).unwrap(), &[1, 2, 3]);
        assert!(ring.access_previous(SecretScope::Jwt).is_none());

        // 第二次轮换：active=[4,5,6]，previous=[1,2,3]
        let audit = ring.rotate(SecretScope::Jwt, now, |_| vec![4, 5, 6]);
        assert_eq!(ring.access_active(SecretScope::Jwt).unwrap(), &[4, 5, 6]);
        assert_eq!(ring.access_previous(SecretScope::Jwt).unwrap(), &[1, 2, 3]);
        // previous 保留（backward：旧签名仍可 verify）
        assert_eq!(ring.access_any(SecretScope::Jwt).unwrap(), &[4, 5, 6]);

        // 审计：key.rotate，detail 不含明文
        assert_eq!(audit.action, "key.rotate");
        assert!(audit.detail.as_deref().unwrap_or("").contains("scope=jwt"));
        assert!(!audit.detail.as_deref().unwrap_or("").contains("1,2,3"));
    }

    #[test]
    fn test_keyring_previous_dropped_after_2nd_rotation() {
        // 第三轮：previous=[1,2,3] 应被丢弃（MVP 双代，只留一轮 previous）
        let mut ring = KeyRing::empty();
        ring.rotate(SecretScope::Jwt, "t", |_| vec![1]);
        ring.rotate(SecretScope::Jwt, "t", |_| vec![2]);
        ring.rotate(SecretScope::Jwt, "t", |_| vec![3]);
        assert_eq!(ring.access_active(SecretScope::Jwt).unwrap(), &[3]);
        assert_eq!(ring.access_previous(SecretScope::Jwt).unwrap(), &[2]);
    }

    #[test]
    fn test_derive_salt_isolated_by_label() {
        let secret = b"top-secret-key";
        let a = derive_salt_from_secret(secret, "jwt", 16);
        let b = derive_salt_from_secret(secret, "api_hash_salt", 16);
        assert_eq!(a.len(), 16);
        assert_ne!(a, b, "不同 label 应派生不同盐");
        // 确定性
        assert_eq!(a, derive_salt_from_secret(secret, "jwt", 16));
    }
}
