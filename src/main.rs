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
//     首次启动自动随机生成并持久化——重启 token 保持有效（）；密钥不再打印到日志
//   - 首次启动自动创建默认租户；同时提供 `--admin-user/--admin-password` 时自动引导管理员（幂等）

use std::net::SocketAddr;

use axum::http::{header, HeaderName, Method};
use clap::Parser;
use evorule_rule::{auth, serve_router, AppState, AuthError, AuthService, Role, RuleStore};
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

    /// evo-agent serve 地址（LLM 命名操作代理目标，历史批次；缺省随 evorule serve 现役端口 8081，RG-1 件A5a）
    #[arg(long, default_value = "http://127.0.0.1:8081")]
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

    /// Console static assets directory (optional). When present, the web console
    /// is served at /console (SPA fallback) and / redirects to /console/.
    /// Missing directory = warn + /console stays 404 (governance API unaffected).
    #[arg(long)]
    console_dir: Option<String>,

    /// 禁用按 IP 速率限制（令牌桶缺省 200 req/s / burst 200；体限+并发限不受此旗标影响）
    #[arg(long)]
    no_rate_limit: bool,
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

    // 2.6 治理原生线：MCP server 拉起批准清单数据集（幂等 seed；空清单 = 执行侧
    // enforce 全部 stdio 拒载，fail-closed——批准逐条走治理审批链产生，机器不代批）
    if store.seed_mcp_servers_dataset_if_empty(&cli.tenant, &auth::iso_from_unix(now))? {
        tracing::info!(
            dataset = evorule_rule::model::mcp_servers::MCP_SERVERS_DATASET_ID,
            "MCP 拉起批准清单数据集已预置"
        );
    } else {
        tracing::info!("MCP 拉起批准清单数据集已存在，跳过预置");
    }

    // 2.7 无人值守防护层：MCP 任务能力包数据集（幂等 seed；空包集=零授权）
    if store.seed_mcp_packs_dataset_if_empty(&cli.tenant, &auth::iso_from_unix(now))? {
        tracing::info!(
            dataset = evorule_rule::model::mcp_packs::MCP_PACKS_DATASET_ID,
            "MCP 任务能力包数据集已预置"
        );
    } else {
        tracing::info!("MCP 任务能力包数据集已存在，跳过预置");
    }

    // 3. 密钥（：优先级 = --secret 显式 > 持久化密钥文件 > 随机生成并持久化；
    //    重启自动复用同一密钥，token 跨重启保持有效；密钥永不打印到日志——
    //    安全边界与 rule.db 一致：持有文件系统读权限即持有库内口令哈希，同级信任面）
    let secret_file = secret_file_path(&cli.db);
    let previous_file = secret_file.with_extension("previous.key");
    // 密钥装载 + 轮换检测（K4 双代）：显式 --secret 与持久化旧钥不同 = 轮换
    // 事件——旧钥降为 previous 持久化，双代验签窗口开启（旧 token 至自然过期）；
    // 其余路径单代不变。
    let (secret, previous_secret) = match &cli.secret {
        Some(s) if !s.is_empty() => {
            let old = load_secret(&secret_file)?;
            persist_secret(&secret_file, s)?;
            let previous = match old {
                Some(o) if o != *s => {
                    persist_secret(&previous_file, &o)?;
                    tracing::info!(
                        path = %previous_file.display(),
                        "检测到显式密钥轮换：旧密钥降为 previous（双代验签窗口开启，旧 token 至自然过期）"
                    );
                    Some(o)
                }
                _ => None,
            };
            (s.clone(), previous)
        }
        _ => {
            let s = load_secret(&secret_file)?.unwrap_or_else(|| {
                let s = random_secret();
                tracing::info!(path = %secret_file.display(), "已生成随机签名密钥并持久化");
                s
            });
            let previous = load_secret(&previous_file).ok().flatten();
            (s, previous)
        }
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
    let state = AppState::with_previous_secret(
        store,
        &secret,
        previous_secret.as_deref(),
        &cli.instance_id,
        &cli.llm_base_url,
    );
    let state = state.bootstrap_backend().await;
    tracing::info!(backend = %state.backend.as_str(), "存储后端已激活");

    // 6. App assembly (governance API /v1 + same-origin alias /rule-serve +
    //    optional static console /console) + CORS (browser cross-origin access
    //    for console-cloud:5174 / evo-agent-ui:5173 etc.; empty allowed-origins
    //    = same-origin only, same-origin restriction invariant)
    let app = serve_router(state, cli.console_dir.as_deref().map(std::path::Path::new));
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
    // serve 面节流第三层（与 evorule-server 同模具）：按 IP 令牌桶速率限，
    // 置于最外层（cors 之外）；依赖 ConnectInfo<SocketAddr> 提取客户端地址，
    // 因此 serve 必须 into_make_service_with_connect_info（见下方 axum::serve）
    let app = match resolve_governor_config(
        if cli.no_rate_limit {
            0
        } else {
            RATE_LIMIT_PER_SEC
        },
        RATE_LIMIT_BURST,
    ) {
        Some(cfg) => {
            tracing::info!(
                "速率限制已启用：{} req/s（burst={RATE_LIMIT_BURST}）",
                if cli.no_rate_limit {
                    0
                } else {
                    RATE_LIMIT_PER_SEC
                }
            );
            app.layer(tower_governor::GovernorLayer::new(cfg))
        }
        None => {
            tracing::info!("速率限制已禁用（--no-rate-limit）");
            app
        }
    };

    // 7. 监听
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %addr, db = %cli.db, "evorule-rule 治理服务已启动");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// 按 IP 速率限制缺省档（与 evorule-server 同档）：持续 200 req/s，突发容量 200
const RATE_LIMIT_PER_SEC: u64 = 200;
/// 令牌桶容量（=突发上限，与 evorule-server 同档）
const RATE_LIMIT_BURST: u32 = 200;

/// 构造按 IP 令牌桶限速配置（与 evorule-server 同模具）：
/// - `per_sec == 0` → `None`（调用方完全跳过 GovernorLayer，真禁用不假禁用）；
/// - 其余 → tower_governor 0.8 语义换算：`per_second(n)` = 每 n 秒回补 1 个令牌，
///   故持续速率 per_sec req/s 换算为 period = 1000/per_sec 毫秒（≥1ms 下限防零）。
fn resolve_governor_config(
    per_sec: u64,
    burst: u32,
) -> Option<
    tower_governor::governor::GovernorConfig<
        tower_governor::key_extractor::PeerIpKeyExtractor,
        governor::middleware::NoOpMiddleware,
    >,
> {
    if per_sec == 0 {
        return None;
    }
    let period_ms = (1000 / per_sec).max(1);
    tower_governor::governor::GovernorConfigBuilder::default()
        .per_millisecond(period_ms)
        .burst_size(burst)
        .finish()
        .or_else(|| {
            tracing::warn!(
                "GovernorConfigBuilder::finish() 返回 None，fallback 到 GovernorConfig::default()"
            );
            Some(tower_governor::governor::GovernorConfig::default())
        })
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

/// 签名密钥持久化路径：与 SQLite 库同目录（data/jwt_secret.key，）
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

    /// ：密钥文件 读回一致 / 覆盖生效 / 缺失或空白视同缺失 / 路径与 --db 同目录
    #[test]
    fn test_secret_file_roundtrip_override_and_missing() {
        let dir = std::env::temp_dir().join(format!(
            "reg092-secret-{}-{}",
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

    /// 速率限制配置：per_sec=0 → 完全禁用（None，不假禁用）；缺省档 → Some
    #[test]
    fn test_governor_config_resolve() {
        assert!(resolve_governor_config(0, 200).is_none());
        assert!(resolve_governor_config(RATE_LIMIT_PER_SEC, RATE_LIMIT_BURST).is_some());
    }
}
