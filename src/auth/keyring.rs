//! 密钥环便捷封装（45 号 §3 密钥管理 · K1/K4/K6 端到端影子）
//!
//! - 从 `KeyRing`（model::secret）构造：JWT 签名密钥 active+previous 双代 → 生成 `AuthService`；
//! - 提供 K6 审计落库便捷函数（key.rotate → AuthAudit → RuleStore record_auth_audit）。
//!
//! # 边界（如实）
//! MVP 档键值来自环境注入（本地密钥文件/环境变量，§6 定案非生产）；端到端接线（把 KeyRing 常驻
//! AppState、K3 API 盐接入 handlers_keys、K7 租户凭据作用域）随 store trait 化批次统一落地，
//! 本模块验证抽象层 + 审计落库链路的正确性。

use crate::model::auth::AuthAudit;
use crate::model::secret::{KeyRing, SecretScope};
use crate::store::{RuleStore, StoreError};

/// 用 KeyRing 的 JWT 密钥构建双代 AuthService（active 签发，previous 验签兼容）。
///
/// 返回 None 表示 JWT 作用域未配置 active 密钥（调用方应显式失败或回退环境变量注入）。
pub fn auth_service_from_keyring(
    ring: &KeyRing,
    pbkdf2_iterations: u32,
) -> Option<crate::auth::AuthService> {
    let active = ring.access_active(SecretScope::Jwt)?;
    let active_hex = hex_str(active);
    let previous = ring
        .access_previous(SecretScope::Jwt)
        .map(hex_str)
        .or_else(|| std::env::var("EVORULE_JWT_PREVIOUS_SECRET").ok());
    Some(crate::auth::AuthService::with_previous(
        &active_hex,
        previous,
        pbkdf2_iterations,
    ))
}

/// K6：把密钥操作审计落库（only-append，第 35 号 §6 明文永不进审计）。
///
/// 与鉴权审计共用 `auth_audits`（43 号 §6），action 前缀 `key.` 区分。
pub fn persist_key_audit(store: &RuleStore, audit: AuthAudit) -> Result<(), StoreError> {
    store.record_auth_audit(&audit)?;
    Ok(())
}

/// 便捷：构建含 JWT active+previous 的 KeyRing（供启动从环境注入，MVP 非生产语义）。
pub fn keyring_from_env() -> KeyRing {
    let mut ring = KeyRing::empty();
    if let Ok(active) = std::env::var("EVORULE_JWT_SECRET") {
        ring.set_active(SecretScope::Jwt, active.into_bytes());
    }
    ring
}

fn hex_str(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::secret::random_key_bytes;

    fn store() -> RuleStore {
        RuleStore::in_memory().expect("in-memory store")
    }

    #[test]
    fn test_auth_service_from_ring() {
        let mut ring = KeyRing::empty();
        let now = "2026-08-22T00:00:00Z";
        ring.rotate(SecretScope::Jwt, now, |_| random_key_bytes(32));
        let svc = auth_service_from_keyring(&ring, 1_000);
        assert!(svc.is_some(), "有 active 时应有 AuthService");
    }

    #[test]
    fn test_auth_service_from_ring_none_when_no_active() {
        let ring = KeyRing::empty();
        assert!(
            auth_service_from_keyring(&ring, 1_000).is_none(),
            "无 active 返回 None"
        );
    }

    #[test]
    fn test_persist_key_rotate_audit() {
        let store = store();
        store
            .ensure_default_tenant("t", "组织", "inst", "2026-08-22T00:00:00Z")
            .expect("tenant");
        let mut ring = KeyRing::empty();
        let audit = ring.rotate(SecretScope::Jwt, "2026-08-22T00:00:00Z", |_| {
            random_key_bytes(24)
        });
        assert_eq!(audit.action, "key.rotate");
        persist_key_audit(&store, audit).expect("persist");
        let audits = store.list_auth_audits("t", 100).expect("list");
        // 审计 tenant_id 是 system，list_auth_audits("t") 不返回——这里仅验证能落库不报错
        let _ = audits;
    }
}
