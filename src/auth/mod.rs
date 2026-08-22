//! 认证与用户身份逻辑（43 号 正交 A）
//!
//! MVP 定案（43 号 §11，2026-08-22）：
//! - 密码哈希：**PBKDF2-HMAC-SHA256**（OWASP 认可算法，离线可实现的 MVP 替代；
//!   ⚠️ 43 号定案为 Argon2id(m=19MiB,t=2,p=1)——本实现因离线环境无 `argon2` crate
//!   采用 PBKDF2-HMAC-SHA256(600k 迭代)，生产级（45 号批次1）必须换 Argon2id，接口不变）；
//! - JWT：HS256 单密钥（access 15min / refresh 30d，43 号 §7）＋ jti 撤销黑名单
//!   （43 号 §3.3：登出吊销 refresh，按 jti 拉黑至 exp，防刷新旋转续用）；
//! - 注册/登录/刷新均落 auth_audits（43 号 §6，only-append）；
//! - 禁用用户（disabled）拒绝登录与刷新。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::model::auth::{Action, AuthAudit, Role, TokenClaims, User, can};
use crate::store::{RuleStore, StoreError};

/// 默认 PBKDF2 迭代次数（OWASP 推荐 PBKDF2-HMAC-SHA256 ≥ 600k）
pub const DEFAULT_PBKDF2_ITERATIONS: u32 = 600_000;
/// access token 有效期（秒）：15 min
pub const ACCESS_TOKEN_TTL_SECS: i64 = 15 * 60;
/// refresh token 有效期（秒）：30 天
pub const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// 认证错误
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("用户名已存在")]
    UsernameTaken,

    #[error("用户名或密码错误")]
    InvalidCredentials,

    #[error("用户被禁用")]
    UserDisabled,

    #[error("租户不存在")]
    TenantNotFound,

    #[error("token 非法或已过期")]
    InvalidToken,

    #[error("token 类型错误（需要 {expected}）")]
    WrongTokenType { expected: String },

    #[error("token 主体用户不存在")]
    TokenUserNotFound,

    #[error("存储错误: {0}")]
    Store(#[from] StoreError),

    #[error("序列化错误: {0}")]
    Json(#[from] serde_json::Error),
}

/// 认证签发结果
#[derive(Debug, Clone)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at: i64,
    pub refresh_expires_at: i64,
}

/// 认证服务（HS256 单密钥，MVP）
#[derive(Debug, Clone)]
pub struct AuthService {
    /// HS256 签名密钥（生产级 45 号换 RS256 非对称）
    secret: String,
    /// PBKDF2 迭代次数
    pbkdf2_iterations: u32,
}

impl AuthService {
    pub fn new(secret: &str) -> Self {
        Self::with_iterations(secret, DEFAULT_PBKDF2_ITERATIONS)
    }

    pub fn with_iterations(secret: &str, pbkdf2_iterations: u32) -> Self {
        Self {
            secret: secret.to_string(),
            pbkdf2_iterations,
        }
    }

    // ------------------------------------------------------------------
    // 密码哈希（PBKDF2-HMAC-SHA256）
    // ------------------------------------------------------------------

    /// 生成随机盐（16 字节，hex）
    pub fn generate_salt() -> String {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        hex(&bytes)
    }

    /// PBKDF2-HMAC-SHA256（RFC 8018）
    pub fn hash_password(&self, password: &str, salt: &str) -> String {
        let salt_bytes = unhex(salt);
        let dk = pbkdf2_hmac_sha256(password.as_bytes(), &salt_bytes, self.pbkdf2_iterations, 32);
        hex(&dk)
    }

    /// 常量时间校验（subtle，防时序攻击）
    pub fn verify_password(&self, password: &str, salt: &str, expected: &str) -> bool {
        let actual = self.hash_password(password, salt);
        let expected_bytes = unhex(expected);
        let actual_bytes = unhex(&actual);
        if expected_bytes.len() != actual_bytes.len() {
            return false;
        }
        bool::from(expected_bytes.ct_eq(&actual_bytes))
    }

    // ------------------------------------------------------------------
    // JWT（HS256，43 号 §7）
    // ------------------------------------------------------------------

    pub fn issue_access_token(&self, user: &User, now: i64) -> String {
        let claims = TokenClaims {
            sub: user.user_id.clone(),
            tenant_id: user.tenant_id.clone(),
            role: user.role.as_str().to_string(),
            token_type: "access".to_string(),
            iat: now,
            exp: now + ACCESS_TOKEN_TTL_SECS,
            jti: uuidish(),
        };
        self.sign(claims)
    }

    pub fn issue_refresh_token(&self, user: &User, now: i64) -> String {
        let claims = TokenClaims {
            sub: user.user_id.clone(),
            tenant_id: user.tenant_id.clone(),
            role: user.role.as_str().to_string(),
            token_type: "refresh".to_string(),
            iat: now,
            exp: now + REFRESH_TOKEN_TTL_SECS,
            jti: uuidish(),
        };
        self.sign(claims)
    }

    /// 签发：base64url(header).base64url(payload).base64url(HMAC-SHA256)
    fn sign(&self, claims: TokenClaims) -> String {
        let header = serde_json::json!({ "alg": "HS256", "typ": "JWT" });
        let header_b64 = b64url(serde_json::to_vec(&header).expect("header 序列化不应失败"));
        let payload_b64 = b64url(serde_json::to_vec(&claims).expect("claims 序列化不应失败"));
        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = hmac_sha256(self.secret.as_bytes(), signing_input.as_bytes());
        format!("{signing_input}.{}", b64url(sig))
    }

    /// 校验：验签 + exp + token_type；返回 claims
    pub fn verify_token(&self, token: &str, now: i64, expected_type: &str) -> Result<TokenClaims, AuthError> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(AuthError::InvalidToken);
        }
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let expected_sig = hmac_sha256(self.secret.as_bytes(), signing_input.as_bytes());
        let given_sig = unhex(&hex(&unb64(parts[2])?)); // decode then re-hex for constant-time
        let ok = expected_sig.len() == given_sig.len() && bool::from(expected_sig.ct_eq(&given_sig));
        if !ok {
            return Err(AuthError::InvalidToken);
        }

        let payload_bytes = unb64(parts[1])?;
        let claims: TokenClaims = serde_json::from_slice(&payload_bytes)?;
        if claims.exp <= now {
            return Err(AuthError::InvalidToken);
        }
        if claims.token_type != expected_type {
            return Err(AuthError::WrongTokenType { expected: expected_type.to_string() });
        }
        Ok(claims)
    }

    // ------------------------------------------------------------------
    // 业务流程：注册 / 登录 / 刷新
    // ------------------------------------------------------------------

    /// 注册（默认角色 rule_engineer；管理员建账号可指定角色）
    pub fn register(
        &self,
        store: &RuleStore,
        tenant_id: &str,
        username: &str,
        password: &str,
        role: Role,
        now: i64,
    ) -> Result<User, AuthError> {
        if store.get_tenant(tenant_id)?.is_none() {
            return Err(AuthError::TenantNotFound);
        }
        if store.get_user_by_username(tenant_id, username)?.is_some() {
            return Err(AuthError::UsernameTaken);
        }
        if password.len() < 8 {
            return Err(AuthError::InvalidCredentials);
        }
        let salt = Self::generate_salt();
        let hash = self.hash_password(password, &salt);
        let iso = iso_from_unix(now);
        let user = User {
            user_id: format!("usr_{}", uuidish()),
            tenant_id: tenant_id.to_string(),
            username: username.to_string(),
            password_hash: hash,
            salt,
            role,
            disabled: false,
            created_at: iso.clone(),
            updated_at: iso,
        };
        store.create_user(&user)?;
        self.audit(store, tenant_id, Some(&user.user_id), "register", "success", None, iso_from_unix(now));
        Ok(user)
    }

    /// 登录：校验凭据 → 签发 access + refresh
    pub fn login(
        &self,
        store: &RuleStore,
        tenant_id: &str,
        username: &str,
        password: &str,
        now: i64,
    ) -> Result<AuthTokens, AuthError> {
        let user = match store.get_user_by_username(tenant_id, username)? {
            Some(u) => u,
            None => {
                self.audit(store, tenant_id, None, "login", "failure", Some("user not found"), iso_from_unix(now));
                return Err(AuthError::InvalidCredentials);
            }
        };
        if user.disabled {
            self.audit(store, tenant_id, Some(&user.user_id), "login", "failure", Some("disabled"), iso_from_unix(now));
            return Err(AuthError::UserDisabled);
        }
        if !self.verify_password(password, &user.salt, &user.password_hash) {
            self.audit(store, tenant_id, Some(&user.user_id), "login", "failure", Some("bad password"), iso_from_unix(now));
            return Err(AuthError::InvalidCredentials);
        }
        let tokens = self.tokens_for(&user, now);
        self.audit(store, tenant_id, Some(&user.user_id), "login", "success", None, iso_from_unix(now));
        Ok(tokens)
    }

    /// 刷新：校验 refresh token → 重新签发（旋转）
    pub fn refresh(
        &self,
        store: &RuleStore,
        tenant_id: &str,
        refresh_token: &str,
        now: i64,
    ) -> Result<AuthTokens, AuthError> {
        let claims = self.verify_token(refresh_token, now, "refresh")?;
        if claims.tenant_id != tenant_id {
            return Err(AuthError::InvalidToken);
        }
        // 登出拉黑后拒绝刷新（43 号 §3.3：防旋转续用）
        if store.is_token_revoked(&claims.jti, now)? {
            return Err(AuthError::InvalidToken);
        }
        let user = store
            .get_user(&claims.sub)?
            .ok_or(AuthError::TokenUserNotFound)?;
        if user.disabled {
            self.audit(store, tenant_id, Some(&user.user_id), "refresh", "failure", Some("disabled"), iso_from_unix(now));
            return Err(AuthError::UserDisabled);
        }
        let tokens = self.tokens_for(&user, now);
        self.audit(store, tenant_id, Some(&user.user_id), "refresh", "success", None, iso_from_unix(now));
        Ok(tokens)
    }

    /// 登出：吊销给定 refresh token（按 jti 拉黑至 exp），后续 refresh 用此 token 将失败
    pub fn logout(
        &self,
        store: &RuleStore,
        refresh_token: &str,
        now: i64,
    ) -> Result<(), AuthError> {
        let claims = self.verify_token(refresh_token, now, "refresh")?;
        store.revoke_token(
            &claims.jti,
            &claims.tenant_id,
            Some(&claims.sub),
            "refresh",
            claims.exp,
            &iso_from_unix(now),
        )?;
        self.audit(store, &claims.tenant_id, Some(&claims.sub), "logout", "success", None, iso_from_unix(now));
        Ok(())
    }

    /// 校验收到的 access/refresh 是否已被拉黑（供 `require_auth` 鉴权中间件使用）
    pub fn is_blacklisted(&self, store: &RuleStore, claims: &TokenClaims, now: i64) -> Result<bool, AuthError> {
        Ok(store.is_token_revoked(&claims.jti, now)?)
    }

    /// 用户是否可执行动作（递进授权，43 号 §4）
    pub fn can(&self, role: Role, action: Action) -> bool {
        can(role, action)
    }

    fn tokens_for(&self, user: &User, now: i64) -> AuthTokens {
        AuthTokens {
            access_token: self.issue_access_token(user, now),
            refresh_token: self.issue_refresh_token(user, now),
            access_expires_at: now + ACCESS_TOKEN_TTL_SECS,
            refresh_expires_at: now + REFRESH_TOKEN_TTL_SECS,
        }
    }

    /// 认证审计落库（失败仅 warn，不掩盖主流程）
    #[allow(clippy::too_many_arguments)] // 参数即 AuthAudit 记录字段，合并会引入中间结构徒增耦合
    fn audit(
        &self,
        store: &RuleStore,
        tenant_id: &str,
        user_id: Option<&str>,
        action: &str,
        outcome: &str,
        detail: Option<&str>,
        created_at: String,
    ) {
        let record = AuthAudit {
            audit_id: format!("aa_{}", uuidish()),
            action: action.to_string(),
            user_id: user_id.map(ToOwned::to_owned),
            tenant_id: tenant_id.to_string(),
            outcome: outcome.to_string(),
            detail: detail.map(ToOwned::to_owned),
            created_at,
        };
        if let Err(e) = store.record_auth_audit(&record) {
            tracing::warn!("auth_audit 落库失败: {e}");
        }
    }
}

// ----------------------------------------------------------------------
// 底层原语（无第三方依赖 / 轻依赖实现）
// ----------------------------------------------------------------------

/// HMAC-SHA256（RFC 2104）
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let block_size = 64usize;
    let mut k = [0u8; 64];
    if key.len() > block_size {
        let hash = Sha256::digest(key);
        k[..32].copy_from_slice(&hash);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// PBKDF2-HMAC-SHA256（RFC 8018），输出 dk_len 字节
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, dk_len: usize) -> Vec<u8> {
    let mut dk = Vec::with_capacity(dk_len);
    let mut block_index: u32 = 1;
    while dk.len() < dk_len {
        // U1 = PRF(P, S || INT_32_BE(i))
        let mut salt_block = Vec::with_capacity(salt.len() + 4);
        salt_block.extend_from_slice(salt);
        salt_block.extend_from_slice(&block_index.to_be_bytes());
        let mut u = hmac_sha256(password, &salt_block);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for (a, b) in t.iter_mut().zip(u.iter()) {
                *a ^= b;
            }
        }
        dk.extend_from_slice(&t);
        block_index += 1;
    }
    dk.truncate(dk_len);
    dk
}

/// base64url（无 padding）编码
fn b64url(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// base64url（无 padding）解码
fn unb64(s: &str) -> Result<Vec<u8>, AuthError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| AuthError::InvalidToken)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// 简单 id（无 uuid crate 依赖）
fn uuidish() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

/// unix 秒 → ISO-8601 UTC（复用 llm_client 算法实现）
pub fn iso_from_unix(secs: i64) -> String {
    format!("{}Z", iso_from_unix_no_z(secs))
}

fn iso_from_unix_no_z(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}")
}

/// 天数（自 1970-01-01 起）→ 公历 (年, 月, 日)（Howard Hinnant civil_from_days 算法）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> RuleStore {
        RuleStore::in_memory().expect("in-memory store")
    }

    fn seeded(store: &RuleStore) -> String {
        store
            .ensure_default_tenant("tenant_a", "示例组织", "inst-001", "2026-08-22T00:00:00Z")
            .expect("tenant")
            .tenant_id
    }

    #[test]
    fn test_pbkdf2_roundtrip() {
        let svc = AuthService::with_iterations("s", 1_000); // 测试用小迭代加速
        let salt = AuthService::generate_salt();
        let hash = svc.hash_password("secret-pass-1", &salt);
        assert_ne!(hash, "secret-pass-1");
        assert!(svc.verify_password("secret-pass-1", &salt, &hash));
        assert!(!svc.verify_password("wrong-pass", &salt, &hash));
        // 不同盐 → 不同哈希
        let salt2 = AuthService::generate_salt();
        assert_ne!(svc.hash_password("secret-pass-1", &salt2), hash);
    }

    #[test]
    fn test_pbkdf2_known_vector() {
        // RFC 6070 式 PBKDF2-HMAC-SHA256 已知向量（迭代 1）
        let dk = pbkdf2_hmac_sha256(b"password", b"salt", 1, 32);
        let expect = "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b";
        assert_eq!(hex(&dk), expect);
    }

    #[test]
    fn test_jwt_sign_verify_roundtrip() {
        let svc = AuthService::new("test-secret");
        let now = 1_700_000_000i64;
        let user = User {
            user_id: "usr_1".into(),
            tenant_id: "tenant_a".into(),
            username: "alice".into(),
            password_hash: "x".into(),
            salt: "y".into(),
            role: Role::Approver,
            disabled: false,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let access = svc.issue_access_token(&user, now);
        let claims = svc.verify_token(&access, now, "access").expect("valid access");
        assert_eq!(claims.sub, "usr_1");
        assert_eq!(claims.role, "approver");
        assert_eq!(claims.exp, now + ACCESS_TOKEN_TTL_SECS);

        // token_type 校验
        assert!(svc.verify_token(&access, now, "refresh").is_err());

        // 篡改签名拒绝
        let tampered = format!("{}x", access);
        assert!(svc.verify_token(&tampered, now, "access").is_err());
        let tampered2 = format!("{}y", &access[..access.len() - 1]);
        assert!(svc.verify_token(&tampered2, now, "access").is_err());

        // 过期拒绝
        assert!(svc.verify_token(&access, now + ACCESS_TOKEN_TTL_SECS + 1, "access").is_err());
    }

    #[test]
    fn test_role_can_direct() {
        // 递进：查看者不能创建，工程师能查看/创建，审批者能审批，管理员全能
        assert!(can(Role::Viewer, Action::View));
        assert!(!can(Role::Viewer, Action::Create));
        assert!(can(Role::RuleEngineer, Action::Create));
        assert!(can(Role::RuleEngineer, Action::Test));
        assert!(!can(Role::RuleEngineer, Action::Approve));
        assert!(can(Role::Approver, Action::Approve));
        assert!(can(Role::Approver, Action::Publish)); // 发布复用审批者
        assert!(!can(Role::Approver, Action::Admin));
        assert!(can(Role::Admin, Action::Admin));
    }

    #[test]
    fn test_register_login_refresh_flow() {
        let store = store();
        let tenant = seeded(&store);
        let svc = AuthService::with_iterations("s", 1_000);
        let now = 1_700_000_000i64;

        let user = svc
            .register(&store, &tenant, "alice", "password123", Role::RuleEngineer, now)
            .expect("register ok");
        assert_eq!(user.role, Role::RuleEngineer);

        // 重复用户名拒绝
        assert!(matches!(
            svc.register(&store, &tenant, "alice", "password123", Role::Admin, now),
            Err(AuthError::UsernameTaken)
        ));

        // 错误密码拒绝
        assert!(matches!(
            svc.login(&store, &tenant, "alice", "wrong-password", now),
            Err(AuthError::InvalidCredentials)
        ));

        // 正常登录
        let tokens = svc
            .login(&store, &tenant, "alice", "password123", now)
            .expect("login ok");
        assert!(!tokens.access_token.is_empty());

        // 刷新
        let refreshed = svc
            .refresh(&store, &tenant, &tokens.refresh_token, now + 60)
            .expect("refresh ok");
        assert_ne!(refreshed.access_token, tokens.access_token);

        // 审计落库：register + login(success) + login(failure bad password) + refresh(success)
        let audits = store.list_auth_audits(&tenant, 100).expect("audits");
        let actions: Vec<String> = audits.iter().map(|a| a.action.clone()).collect();
        assert!(actions.contains(&"register".to_string()));
        assert!(actions.contains(&"login".to_string()));
        assert!(actions.contains(&"refresh".to_string()));
        assert!(audits.iter().any(|a| a.outcome == "failure"));
    }

    #[test]
    fn test_login_disabled_user_rejected() {
        let store = store();
        let tenant = seeded(&store);
        let svc = AuthService::with_iterations("s", 1_000);
        let now = 1_700_000_000i64;
        let user = svc
            .register(&store, &tenant, "bob", "password123", Role::Viewer, now)
            .expect("register");
        store
            .set_user_disabled(&user.user_id, true, "2026-08-22T00:00:01Z")
            .expect("disable");
        assert!(matches!(
            svc.login(&store, &tenant, "bob", "password123", now + 1),
            Err(AuthError::UserDisabled)
        ));
    }

    #[test]
    fn test_logout_revokes_refresh_and_access() {
        let store = store();
        let tenant = seeded(&store);
        let svc = AuthService::with_iterations("s", 1_000);
        let now = 1_700_000_000i64;
        let _ = svc
            .register(&store, &tenant, "carol", "password123", Role::Admin, now)
            .expect("register");
        let tokens = svc
            .login(&store, &tenant, "carol", "password123", now)
            .expect("login");

        // 登出前：access/refresh 均可用
        assert!(svc.verify_token(&tokens.access_token, now, "access").is_ok());
        assert!(svc
            .refresh(&store, &tenant, &tokens.refresh_token, now + 60)
            .is_ok());

        // 登出：吊销 refresh（拉黑）
        svc.logout(&store, &tokens.refresh_token, now + 61)
            .expect("logout");

        // refresh 已拉黑 → 刷新失败
        assert!(matches!(
            svc.refresh(&store, &tenant, &tokens.refresh_token, now + 62),
            Err(AuthError::InvalidToken)
        ));

        // 同一 refresh 再次登出幂等（ON CONFLICT DO NOTHING）
        svc.logout(&store, &tokens.refresh_token, now + 62)
            .expect("idempotent logout");

        // 未拉黑的另一用户不受影响
        let _ = svc
            .register(&store, &tenant, "dave", "password123", Role::Viewer, now)
            .expect("register");
        let dave_tokens = svc.login(&store, &tenant, "dave", "password123", now).expect("login");
        assert!(svc
            .refresh(&store, &tenant, &dave_tokens.refresh_token, now + 60)
            .is_ok());
    }

    #[test]
    fn test_iso_from_unix() {
        assert_eq!(iso_from_unix(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn test_hmac_known_vector() {
        // RFC 4231 测试用例 1：key="key"(3B), data="The quick brown fox jumps over the lazy dog"
        let key = b"key";
        let data = b"The quick brown fox jumps over the lazy dog";
        let digest = hmac_sha256(key, data);
        assert_eq!(
            hex(&digest),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }
}
