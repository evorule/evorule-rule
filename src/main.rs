// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 EvoRule Project
//
// evorule-rule-serve —— 独立治理服务二进制（Phase 2 治理接线）。
//
// 将 evorule-rule 从纯库 crate 提升为可独立运行的治理服务，
// 供 console-cloud 的 GovernanceBackend 通过 REST 接入
// （数据集 → 5 态生命周期 → 审批发布 → 版本历史/快照包）。
//
// 用法（例）：
//   evorule-rule-serve --db ./data/rule.db --port 18081 \
//       --admin-user admin --admin-password <pw> \
//       --allowed-origins "http://localhost:5174,http://127.0.0.1:5174"
//
// 说明：
//   - 默认 SQLite 活跃引擎；`--features postgres` + `DATABASE_URL` 才尝试 PG（见 bootstrap_backend）
//   - `--secret` 不提供则复用持久化密钥文件（与 --db 同目录 jwt_secret.key），
//     首次启动自动随机生成并持久化——重启 token 保持有效（UV-092）；密钥不再打印到日志
//   - 首次启动自动创建默认租户；同时提供 `--admin-user/--admin-password` 时自动引导管理员（幂等）

use std::net::SocketAddr;

use axum::http::{header, HeaderName, Method};
use clap::Parser;
use evorule_rule::{auth, router, AppState, AuthError, AuthService, Role, RuleStore};
use tower_http::cors::CorsLayer;

/// evorule-rule 独立治理服务
#[derive(Parser, Debug)]
#[command(
    name = "evorule-rule-serve",
    version,
    about = "EvoRule JSON 规则资产治理服务"
)]
struct Cli {
    /// SQLite 数据库路径（父目录自动创建）
    #[arg(long, default_value = "./data/rule.db")]
    db: String,

    /// 监听地址
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// 监听端口
    #[arg(long, default_value_t = 18081)]
    port: u16,

    /// JWT 签名密钥（不提供则复用持久化密钥文件 jwt_secret.key，首次自动随机生成并持久化；显式传入会覆盖持久化值）
    #[arg(long, env = "EVORULE_RULE_SECRET")]
    secret: Option<String>,

    /// 实例身份（进溯源/审计）
    #[arg(long, default_value = "evorule-rule-instance")]
    instance_id: String,

    /// evo-agent serve 地址（LLM 命名操作代理目标，37 号）
    #[arg(long, default_value = "http://127.0.0.1:18090")]
    llm_base_url: String,

    /// CORS 允许的 Origin 列表（逗号分隔；空 = 仅同源）
    #[arg(long, env = "EVORULE_RULE_ALLOWED_ORIGINS", value_delimiter = ',')]
    allowed_origins: Vec<String>,

    /// 默认租户 ID（不存在则创建）
    #[arg(long, default_value = "default")]
    tenant: String,

    /// 默认租户名称
    #[arg(long, default_value = "默认组织")]
    tenant_name: String,

    /// 引导管理员用户名（与 --admin-password 同时提供才创建）
    #[arg(long)]
    admin_user: Option<String>,

    /// 引导管理员密码（与 --admin-user 同时提供才创建）
    #[arg(long)]
    admin_password: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move { run(cli).await })
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // 1. 数据库（父目录自动创建）
    let db_path = std::path::Path::new(&cli.db);
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let store = RuleStore::open(&cli.db)?;

    // 2. 默认租户（幂等）
    let now = evorule_rule::api::unix_now();
    store.ensure_default_tenant(
        &cli.tenant,
        &cli.tenant_name,
        &cli.instance_id,
        &auth::iso_from_unix(now),
    )?;
    tracing::info!(tenant = %cli.tenant, "默认租户就绪");

    // 2.1 B1 双层租户：platform 下默认 org（幂等；存量数据 tenant_id 即该 org id）
    store.ensure_default_org(&cli.tenant, &cli.tenant_name, &auth::iso_from_unix(now))?;
    tracing::info!(org = %cli.tenant, "默认组织就绪");

    // 2.5 C2：服务目录预置官方 7 个原生服务（幂等 seed；02 方案服务契约三层闭环 层0）
    let seeded = store.seed_official_services_if_empty(&auth::iso_from_unix(now))?;
    if seeded > 0 {
        tracing::info!(seeded, "官方服务目录已预置（7 个原生服务）");
    } else {
        tracing::info!("服务目录已存在，跳过官方预置");
    }

    // 3. 密钥（UV-092：优先级 = --secret 显式 > 持久化密钥文件 > 随机生成并持久化；
    //    重启自动复用同一密钥，token 跨重启保持有效；密钥永不打印到日志——
    //    安全边界与 rule.db 一致：持有文件系统读权限即持有库内口令哈希，同级信任面）
    let secret_file = secret_file_path(&cli.db);
    let secret = match &cli.secret {
        Some(s) if !s.is_empty() => {
            persist_secret(&secret_file, s)?;
            tracing::info!(
                path = %secret_file.display(),
                "已使用显式 --secret 并持久化（重启自动复用，token 保持有效）"
            );
            s.clone()
        }
        _ => match load_secret(&secret_file)? {
            Some(s) => {
                tracing::info!(
                    path = %secret_file.display(),
                    "已加载持久化签名密钥（重启 token 保持有效）"
                );
                s
            }
            None => {
                let s = random_secret();
                persist_secret(&secret_file, &s)?;
                tracing::info!(
                    path = %secret_file.display(),
                    "已生成随机签名密钥并持久化（重启自动复用，token 保持有效）"
                );
                s
            }
        },
    };

    // 4. 引导管理员（幂等：仅当租户内无该用户名时创建；B1：平台管理员，可管理 org）
    if let (Some(user), Some(pass)) = (&cli.admin_user, &cli.admin_password) {
        let auth_svc = AuthService::new(&secret);
        match auth_svc.register(&store, &cli.tenant, user, pass, Role::PlatformAdmin, now) {
            Ok(_) => tracing::info!(user = %user, "引导管理员创建成功（platform_admin）"),
            Err(AuthError::UsernameTaken) => {
                tracing::info!(user = %user, "引导管理员已存在，跳过创建");
            }
            Err(e) => return Err(format!("引导管理员创建失败: {e}").into()),
        }
    }

    // 5. 应用状态 + 后端靴（PG 冒烟门控；默认 SQLite）
    let state = AppState::new(store, &secret, &cli.instance_id, &cli.llm_base_url);
    let state = state.bootstrap_backend().await;
    tracing::info!(backend = %state.backend.as_str(), "存储后端已激活");

    // 6. CORS（浏览器跨源接入 console-cloud:5174 / evo-agent-ui:5173 等）
    let app = router(state);
    let app = if cli.allowed_origins.is_empty() {
        app
    } else {
        let header_vals: Vec<axum::http::HeaderValue> = cli
            .allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        if header_vals.is_empty() {
            app
        } else {
            let cors = CorsLayer::new()
                .allow_origin(header_vals)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::PATCH,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::AUTHORIZATION,
                    header::ACCEPT,
                    HeaderName::from_static("x-api-key"),
                    HeaderName::from_static("idempotency-key"),
                ])
                .allow_credentials(true);
            app.layer(cors)
        }
    };

    // 7. 监听
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %addr, db = %cli.db, "evorule-rule 治理服务已启动");
    axum::serve(listener, app).await?;
    Ok(())
}

/// 随机生成 HS256 签名密钥（32 字节 base64url）
fn random_secret() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 签名密钥持久化路径：与 SQLite 库同目录（data/jwt_secret.key，UV-092）
fn secret_file_path(db: &str) -> std::path::PathBuf {
    std::path::Path::new(db)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("jwt_secret.key")
}

/// 读取持久化密钥（不存在/空白文件返回 None，视同缺失重新生成）
fn load_secret(path: &std::path::Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let s = s.trim().to_string();
            if s.is_empty() {
                Ok(None)
            } else {
                Ok(Some(s))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 持久化密钥（幂等覆盖；保证父目录存在）
fn persist_secret(path: &std::path::Path, secret: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UV-092：密钥文件 读回一致 / 覆盖生效 / 缺失或空白视同缺失 / 路径与 --db 同目录
    #[test]
    fn test_secret_file_roundtrip_override_and_missing() {
        let dir = std::env::temp_dir().join(format!(
            "uv092-secret-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jwt_secret.key");

        // 1) 不存在 → None
        assert!(load_secret(&path).unwrap().is_none());

        // 2) 持久化 → 读回一致
        let s1 = random_secret();
        persist_secret(&path, &s1).unwrap();
        assert_eq!(load_secret(&path).unwrap().as_deref(), Some(s1.as_str()));

        // 3) 显式覆盖 → 新值生效（--secret 传入即覆盖持久化值的语义）
        let s2 = random_secret();
        persist_secret(&path, &s2).unwrap();
        assert_eq!(load_secret(&path).unwrap().as_deref(), Some(s2.as_str()));

        // 4) 空白文件 → None（防半写/损坏文件静默当密钥用）
        std::fs::write(&path, "  \n").unwrap();
        assert!(load_secret(&path).unwrap().is_none());

        // 5) 路径派生：与 --db 同目录；无父目录时落到 ./
        assert_eq!(
            secret_file_path("./data/rule.db"),
            std::path::Path::new("./data/jwt_secret.key")
        );
        assert_eq!(
            secret_file_path("rule.db"),
            std::path::Path::new("./jwt_secret.key")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
