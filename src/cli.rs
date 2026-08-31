//! evorule-rule-cli（B3 段B 14 号）：治理服务 REST 命令行客户端。
//!
//! 纯 REST 接入（不对 db 直连），供服务公司/运维在终端批量操作数据资产：
//! - `datasets list` / `datasets export` / `datasets import`
//! - `entries bulk-import <dir>`（目录批 JSON → 构建 bundle → /v1/bundles/import，
//!   同一 5 步校验链 SSOT 门禁，不绕行）
//! - `entries bulk-export <dataset_id> <dir>`（bundle → 目录批 JSON）
//!
//! 认证：Bearer token（env `EVORULE_RULE_TOKEN` 或 --token）；
//! 服务地址：env `EVORULE_RULE_URL` 或 --url，默认 `http://127.0.0.1:18081`。
//!
//! bulk-import 的条目文件为 BundleEntry 形态（与 bulk-export 输出一致，往返可逆）：
//!
//! ```json
//! {
//!   "entry_id": "tax-001", "entry_kind": "rule",
//!   "rule_body": { "...": "evorule 原生 JSON" },
//!   "provenance": { "source": "《企业所得税法》" },
//!   "domain": "tax", "tags": [], "dependencies": []
//! }
//! ```
//!
//! 目录内可选 `dataset.json`（dataset_id/name/domain/visibility），缺省由
//! --dataset 或目录名推导；条目引用的服务自动聚合进 data_dependencies.services 声明
//! （执行侧绑定核对的前置，缺失声明由导入校验链显式拒绝）。

use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::path::Path;

use crate::bundle::{BundleEntry, BundleTests, DatasetBundle, BUNDLE_SCHEMA_VERSION};

/// evorule-rule-cli 参数
#[derive(Parser, Debug)]
#[command(
    name = "evorule-rule-cli",
    version,
    about = "EvoRule JSON 规则资产治理 CLI（REST 客户端）"
)]
pub struct CliArgs {
    /// 治理服务地址
    #[arg(long, env = "EVORULE_RULE_URL", default_value = "http://127.0.0.1:18081")]
    pub base_url: String,

    /// API token（Bearer；env EVORULE_RULE_TOKEN）
    #[arg(long, env = "EVORULE_RULE_TOKEN")]
    pub token: Option<String>,

    /// 以 JSON 打印原始响应（调试用）
    #[arg(long, default_value_t = false)]
    pub json: bool,

    #[command(subcommand)]
    pub cmd: Cmd,
}

/// CLI 子命令
#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// 数据集列表
    DatasetsList {
        /// 过滤条目后仅显示命中数据集数量提示（保留参数位，MVP 列表不过滤）
        #[arg(long)]
        limit: Option<u32>,
    },
    /// 导出数据集快照包（当前或指定版本；历史版本需 B4 快照留档）
    DatasetsExport {
        /// 数据集 ID
        id: String,
        /// 版本号（缺省 = 当前版本）
        #[arg(long)]
        version: Option<String>,
        /// 输出文件（缺省打印到 stdout）
        #[arg(long)]
        out: Option<String>,
    },
    /// 导入快照包文件（POST /v1/bundles/import，同一校验链 SSOT 门禁）
    DatasetsImport {
        /// bundle JSON 文件
        file: String,
    },
    /// 目录批 JSON → 构建 bundle → 导入（走 /v1/bundles/import 同一校验链）
    ///
    /// 导入链含闸门一（沙箱验证证据，verdict 必须为 pass）。缺省以"未验证"提交，
    /// 会被服务端如实拒绝；跑完测试工作台后用 --tests 携带证据 JSON（不绕行、不伪造）。
    EntriesBulkImport {
        /// 条目 JSON 目录（*.json；dataset.json 可选）
        dir: String,
        /// 目标数据集 ID（缺省取目录名）
        #[arg(long)]
        dataset: Option<String>,
        /// 闸门一证据 JSON 文件（BundleTests 形态，测试工作台产出）
        #[arg(long)]
        tests: Option<String>,
    },
    /// 数据集条目 → 目录批 JSON（与 bulk-import 同形态，往返可逆）
    EntriesBulkExport {
        /// 数据集 ID
        id: String,
        /// 输出目录（不存在则创建）
        dir: String,
        /// 版本号（缺省 = 当前版本）
        #[arg(long)]
        version: Option<String>,
    },
}

/// CLI 错误（如实透出服务端错误体，不静默）
#[derive(Debug)]
pub struct CliError(pub String);

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// CLI 入口（bin 薄壳调用；返回 Ok 时的 stdout 输出已随执行打印）
pub fn run(args: CliArgs) -> Result<(), CliError> {
    let base = args.base_url.trim_end_matches('/').to_string();
    let json_mode = args.json;
    match args.cmd {
        Cmd::DatasetsList { limit } => {
            let limit = limit.unwrap_or(100);
            let (_, body) = send(
                ureq::get(&format!("{base}/v1/datasets?limit={limit}")),
                args.token.as_deref(),
                None,
            )?;
            print_body(&body, json_mode);
            let items = body["items"].as_array().cloned().unwrap_or_default();
            println!("{:<24} {:<12} {:<10} {:<10} {}", "DATASET_ID", "KIND", "STATUS", "VERSION", "NAME");
            for ds in &items {
                println!(
                    "{:<24} {:<12} {:<10} {:<10} {}",
                    ds["dataset_id"].as_str().unwrap_or("-"),
                    ds["dataset_kind"].as_str().unwrap_or("-"),
                    ds["lifecycle"]["status"].as_str().unwrap_or("-"),
                    ds["versioning"]["current"].as_str().unwrap_or("-"),
                    ds["name"].as_str().unwrap_or("-"),
                );
            }
        }
        Cmd::DatasetsExport { id, version, out } => {
            let version = match version {
                Some(v) => v,
                None => current_version(&base, args.token.as_deref(), &id)?,
            };
            let (_, body) = send(
                ureq::get(&format!(
                    "{base}/v1/bundles/datasets/{id}/versions/{version}"
                )),
                args.token.as_deref(),
                None,
            )?;
            write_or_print(&body, out.as_deref())?;
        }
        Cmd::DatasetsImport { file } => {
            let bundle: Value = read_json(Path::new(&file))?;
            let (status, body) = send(
                ureq::post(&format!("{base}/v1/bundles/import")),
                args.token.as_deref(),
                Some(json!({ "bundle": bundle })),
            )?;
            require_created(status, &body)?;
            print_body(&body, json_mode);
        }
        Cmd::EntriesBulkImport { dir, dataset, tests } => {
            let mut bundle = build_bundle_from_dir(Path::new(&dir), dataset.as_deref())?;
            // 闸门一证据：缺省"未验证"（verdict=fail，服务端如实拒绝并给出指引）；
            // --tests 显式携带测试工作台产出的证据（T0 纪律：不默认 Pass，不绕行）
            if let Some(tf) = tests {
                let t: BundleTests = read_json(Path::new(&tf))
                    .and_then(|v| serde_json::from_value(v).map_err(|e| CliError(format!("--tests 文件解析失败: {e}"))))?;
                bundle.tests = t;
                bundle.audit.content_hash = bundle.compute_content_hash();
            }
            let (status, body) = send(
                ureq::post(&format!("{base}/v1/bundles/import")),
                args.token.as_deref(),
                Some(json!({ "bundle": serde_json::to_value(&bundle)
                    .map_err(|e| CliError(format!("bundle 序列化失败: {e}")))? })),
            )?;
            require_created(status, &body)?;
            print_body(&body, json_mode);
        }
        Cmd::EntriesBulkExport { id, dir, version } => {
            let version = match version {
                Some(v) => v,
                None => current_version(&base, args.token.as_deref(), &id)?,
            };
            let (_, body) = send(
                ureq::get(&format!(
                    "{base}/v1/bundles/datasets/{id}/versions/{version}"
                )),
                args.token.as_deref(),
                None,
            )?;
            let entries = body["entries"]
                .as_array()
                .cloned()
                .ok_or_else(|| CliError(format!("bundle 响应缺 entries 字段: {body}")))?;
            std::fs::create_dir_all(&dir)
                .map_err(|e| CliError(format!("创建目录 {dir} 失败: {e}")))?;
            for e in &entries {
                let entry_id = e["entry_id"]
                    .as_str()
                    .ok_or_else(|| CliError("条目缺 entry_id".into()))?;
                // entry_id 只含治理侧合法字符（字母/数字/连字符/下划线），直接作文件名
                let path = Path::new(&dir).join(format!("{entry_id}.json"));
                let text = serde_json::to_string_pretty(e)
                    .map_err(|e| CliError(format!("条目序列化失败: {e}")))?;
                std::fs::write(&path, text)
                    .map_err(|e| CliError(format!("写入 {} 失败: {e}", path.display())))?;
            }
            println!("已导出 {} 条条目 → {dir}", entries.len());
        }
    }
    Ok(())
}

// ------------------------------------------------------------------
// HTTP（ureq 阻塞客户端；服务端 4xx/5xx 错误体如实透出不静默）
// ------------------------------------------------------------------

fn send(
    req: ureq::Request,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<(u16, Value), CliError> {
    let req = match token {
        Some(t) => req.set("Authorization", &format!("Bearer {t}")),
        None => req,
    };
    let result = match body {
        Some(b) => {
            let text = serde_json::to_string(&b)
                .map_err(|e| CliError(format!("请求体序列化失败: {e}")))?;
            req.set("Content-Type", "application/json").send_string(&text)
        }
        None => req.call(),
    };
    let resp = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(CliError(format!("请求失败: {e}"))),
    };
    let status = resp.status();
    let text = resp
        .into_string()
        .map_err(|e| CliError(format!("读取响应失败: {e}")))?;
    let json: Value = serde_json::from_str(&text)
        .unwrap_or_else(|_| Value::String(text.clone()));
    Ok((status, json))
}

fn current_version(base: &str, token: Option<&str>, id: &str) -> Result<String, CliError> {
    let (_, body) = send(ureq::get(&format!("{base}/v1/datasets/{id}")), token, None)?;
    body["versioning"]["current"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| CliError(format!("数据集 {id} 响应缺 versioning.current: {body}")))
}

fn require_created(status: u16, body: &Value) -> Result<(), CliError> {
    if !(200..300).contains(&status) {
        let msg = body.to_string();
        // fail-fast + 自助指引：闸门一缺证据时给出可自愈路径（对齐系统自愈原则）
        if msg.contains("沙箱验证未通过") {
            return Err(CliError(format!(
                "导入被拒绝（HTTP {status}）: {body}\n指引: 导入校验链含闸门一（沙箱验证证据）——\
                 请先在测试工作台跑完验证，再用 --tests <证据JSON> 重新提交"
            )));
        }
        return Err(CliError(format!("导入被拒绝（HTTP {status}）: {body}")));
    }
    Ok(())
}

fn print_body(body: &Value, json_mode: bool) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string())
        );
    }
}

fn write_or_print(body: &Value, out: Option<&str>) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(body)
        .map_err(|e| CliError(format!("响应序列化失败: {e}")))?;
    match out {
        Some(path) => {
            std::fs::write(path, &text)
                .map_err(|e| CliError(format!("写入 {path} 失败: {e}")))?;
            println!("已写出 {path}");
        }
        None => println!("{text}"),
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError(format!("读取 {} 失败: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| CliError(format!("{} JSON 解析失败: {e}", path.display())))
}

// ------------------------------------------------------------------
// bulk-import：目录 → DatasetBundle（同一校验链门禁）
// ------------------------------------------------------------------

/// 从目录构建 DatasetBundle：`*.json` 为条目（BundleEntry 形态），`dataset.json` 可选。
/// 条目引用的服务自动聚合进声明；bundle 哈希在本端计算后随包上传（防篡改校验对齐）。
pub fn build_bundle_from_dir(dir: &Path, dataset_override: Option<&str>) -> Result<DatasetBundle, CliError> {
    let meta_path = dir.join("dataset.json");
    let meta = if meta_path.exists() {
        Some(read_json(&meta_path)?)
    } else {
        None
    };
    let dir_name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "bulk-import".into());
    let dataset_id = dataset_override
        .map(String::from)
        .or_else(|| meta.as_ref().and_then(|m| m["dataset_id"].as_str().map(String::from)))
        .unwrap_or_else(|| dir_name.clone());

    // 条目文件：排序保证确定性；跳过 dataset.json
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| CliError(format!("读取目录 {} 失败: {e}", dir.display())))?
        .filter_map(|r| r.ok())
        .map(|r| r.path())
        .filter(|p| {
            p.extension().map(|e| e == "json").unwrap_or(false)
                && p.file_name().map(|f| f != "dataset.json").unwrap_or(true)
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(CliError(format!("目录 {} 无条目 JSON 文件", dir.display())));
    }

    let mut entries: Vec<BundleEntry> = Vec::new();
    for f in &files {
        let v: Value = read_json(f)?;
        let entry: BundleEntry = serde_json::from_value(v.clone())
            .map_err(|e| CliError(format!("{} 不是合法条目（BundleEntry 形态）: {e}", f.display())))?;
        entries.push(entry);
    }

    // 服务声明聚合：条目 dependencies 引用的服务必须声明（与执行侧绑定核对同口径）
    let mut services: Vec<evorule_bundle::ServiceDecl> = Vec::new();
    for e in &entries {
        for d in &e.dependencies {
            if !services.iter().any(|s| s.service_name == d.service_name) {
                services.push(evorule_bundle::ServiceDecl {
                    service_name: d.service_name.clone(),
                    version: None,
                    io_contract: None,
                    sensitive: false,
                    description: None,
                    template: None,
                });
            }
        }
    }

    let mut bundle = DatasetBundle {
        bundle_schema_version: BUNDLE_SCHEMA_VERSION.into(),
        bundle_id: format!("bundle-{}-v1", dataset_id),
        dataset: evorule_bundle::BundleDatasetMeta {
            dataset_id: dataset_id.clone(),
            name: meta
                .as_ref()
                .and_then(|m| m["name"].as_str().map(String::from))
                .unwrap_or_else(|| dir_name),
            tenant_id: String::new(), // 导入侧以调用方租户为准（租户隔离，不信任包内声明）
            instance_id: "cli-bulk-import".into(),
            versioning: Default::default(),
            law_ref: None,
            // CLI 目录组装的 bundle 无法规生效基准，缺省 pinned 到当前版本（v1）——
            // 不误用 auto_by_effective_date（那要求 law_ref.effective_from，导入校验会如实拒绝）
            version_selection: Some(evorule_bundle::VersionSelection {
                mode: evorule_bundle::VersionSelectionMode::Pinned,
                pinned_version: Some("v1".into()),
                pinned_include_patch: None,
            }),
            view_of: None,
        },
        entries,
        data_dependencies: if services.is_empty() {
            None
        } else {
            Some(evorule_bundle::DataDependencies {
                inputs: vec![],
                services,
            })
        },
        // 闸门一：缺省"未验证"证据（verdict=fail）——服务端如实拒绝，CLI 提示走测试工作台；
        // 绝不默认 Pass（T0 纪律）
        tests: BundleTests::unverified(),
        audit: evorule_bundle::BundleAudit {
            exported_at: iso_now(),
            exported_by: "cli-bulk-import".into(),
            source_version: "v1".into(),
            content_hash: String::new(),
            hash_algo: "blake3".into(),
        },
    };
    bundle.audit.content_hash = bundle.compute_content_hash();
    Ok(bundle)
}

/// ISO-8601 UTC 时间戳（CLI 端本地生成；精度秒级足够溯源）
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 简化 UTC 格式化（避免引入 chrono；仅审计展示用）
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // days since 1970-01-01 → civil date（Howard Hinnant 算法）
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::TestVerdict;

    #[test]
    fn test_build_bundle_from_dir_roundtrip_shape() {
        let tmp = std::env::temp_dir().join(format!("cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let entry = json!({
            "entry_id": "e1",
            "entry_kind": "rule",
            "rule_body": {
                "transform": [{"type": "io_request", "params": {"service_name": "payroll_svc"}}]
            },
            "provenance": { "source": "《企业所得税法》" },
            "domain": "tax",
            "tags": [],
            "dependencies": [
                { "rule_ref": "transform[0]", "service_name": "payroll_svc" }
            ]
        });
        std::fs::write(tmp.join("e1.json"), serde_json::to_string(&entry).unwrap()).unwrap();
        std::fs::write(
            tmp.join("dataset.json"),
            json!({ "dataset_id": "ds-cli", "name": "CLI 数据集" }).to_string(),
        )
        .unwrap();

        let bundle = build_bundle_from_dir(&tmp, None).unwrap();
        assert_eq!(bundle.dataset.dataset_id, "ds-cli");
        assert_eq!(bundle.entries.len(), 1);
        assert_eq!(bundle.dataset.versioning.current, "v1");
        // 服务自动聚合
        let dd = bundle.data_dependencies.clone().unwrap();
        assert_eq!(dd.services[0].service_name, "payroll_svc");
        // T0 纪律：缺省证据为"未验证"（verdict=fail），绝不默认 Pass
        assert_eq!(bundle.tests.verdict, TestVerdict::Fail);
        // 哈希本端计算且可校验（防篡改口径对齐）
        bundle.verify_content_hash().unwrap();
        // 未验证证据 → 校验链（SSOT 门禁）如实拒绝（不绕行）
        let resolver = |_: &str| None;
        let err = crate::bundle::BundleImporter::validate(&bundle, &resolver).unwrap_err();
        assert!(matches!(err, crate::bundle::BundleError::TestsNotPassed { .. }), "{err}");
        // 模拟测试工作台产出证据后 → 校验链通过
        let mut evidenced = bundle;
        evidenced.tests = BundleTests { verdict: TestVerdict::Pass, ..Default::default() };
        evidenced.audit.content_hash = evidenced.compute_content_hash();
        crate::bundle::BundleImporter::validate(&evidenced, &resolver).unwrap();

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_build_bundle_rejects_bad_entry() {
        let tmp = std::env::temp_dir().join(format!("cli-bad-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // 缺 rule_body → BundleEntry 解析失败，显式报错不静默
        std::fs::write(
            tmp.join("bad.json"),
            json!({ "entry_id": "bad", "domain": "tax" }).to_string(),
        )
        .unwrap();
        let err = build_bundle_from_dir(&tmp, Some("ds-x")).unwrap_err();
        assert!(err.0.contains("不是合法条目"), "{err}");

        std::fs::remove_dir_all(&tmp).ok();
    }

    // ------------------------------------------------------------------
    // 端到端：CLI ↔ 真实 HTTP 服务（临时端口），覆盖计划测试门：
    // export→import 往返内容一致 / 闸门一缺证据如实拒绝 / API filter 集成
    // ------------------------------------------------------------------

    use crate::model::dataset::{DatasetKind, RuleDataset, Visibility};
    use crate::model::dependency::{DataDependencies, ServiceDecl, SourceBinding};
    use crate::model::entry::RuleEntry;
    use crate::model::dataset::Meta;
    use crate::model::provenance::Provenance;
    use crate::model::version::Versioning;
    use crate::store::RuleStore;

    fn e2e_dataset() -> RuleDataset {
        RuleDataset {
            dataset_id: "ds-cli-e2e".into(),
            name: "CLI 端到端".into(),
            description: None,
            dataset_kind: DatasetKind::RuleSet,
            domain: vec!["tax".into()],
            tags: vec![],
            tenant_id: "org-evorule".into(),
            visibility: Visibility::Private,
            lifecycle: Default::default(),
            versioning: Versioning::default(),
            law_ref: None,
            version_selection: None,
            data_dependencies: Some(DataDependencies {
                inputs: vec![],
                services: vec![ServiceDecl {
                    service_name: "payroll_svc".into(),
                    version: None,
                    io_contract: None,
                    sensitive: false,
                    description: None,
                    template: None,
                }],
            }),
            meta: Meta { created_at: "t".into(), created_by: "u".into(), updated_at: None, updated_by: None },
        }
    }

    fn e2e_entry() -> RuleEntry {
        RuleEntry {
            entry_id: "e1".into(),
            dataset_id: "ds-cli-e2e".into(),
            version: 1,
            status: None,
            provenance: Provenance {
                source: "《企业所得税法》".into(),
                clause: None,
                document_id: None,
                effective_from: None,
                effective_to: None,
                last_verified: None,
                verified_by: None,
            },
            domain: "tax".into(),
            tags: vec!["core".into()],
            data_source_binding: vec![SourceBinding {
                rule_ref: "rule_body.transform[0].params.service_name".into(),
                service_name: "payroll_svc".into(),
            }],
            consumed_inputs: vec![],
            rule_body: serde_json::json!({
                "transform": [{"type": "io_request", "params": {"service_name": "payroll_svc"}}]
            }),
            governance: None,
        }
    }

    fn cli_args(base: &str, token: &str, cmd: Cmd) -> CliArgs {
        CliArgs { base_url: base.into(), token: Some(token.into()), json: false, cmd }
    }

    /// run() 是阻塞 CLI 入口（ureq 同步请求）；测试内 server 与测试共享 current-thread
    /// runtime，必须放 spawn_blocking，否则测试线程被阻塞后 server 无法轮询 → 死锁。
    async fn run_blocking(base: &str, token: &str, cmd: Cmd) -> Result<(), CliError> {
        let args = cli_args(base, token, cmd);
        tokio::task::spawn_blocking(move || run(args)).await.unwrap()
    }

    #[tokio::test]
    async fn test_cli_end_to_end_roundtrip_via_rest() {
        use crate::model::auth::Role;
        use crate::{AppState, router};

        // 1. 内存库 + 种子数据 + admin（导入端点要求管理员）
        let store = RuleStore::in_memory().unwrap();
        store
            .ensure_default_tenant("org-evorule", "默认组织", "inst-001", "2026-08-22T00:00:00Z")
            .unwrap();
        store
            .ensure_default_org("org-evorule", "默认组织", "2026-08-22T00:00:00Z")
            .unwrap();
        store.create_dataset(&e2e_dataset()).unwrap();
        store.add_entry(&e2e_entry()).unwrap();
        let state = AppState::new(store, "test-secret", "inst-001", "http://127.0.0.1:9");
        state
            .auth
            .register(&state.store, "org-evorule", "cli-admin", "password123", Role::Admin, crate::api::unix_now())
            .unwrap();
        let app = router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{addr}");

        // 2. 登录拿 token
        let (status, body) = tokio::task::spawn_blocking({
            let base = base.clone();
            move || {
                crate::cli::send(
                    ureq::post(&format!("{base}/v1/auth/login")),
                    None,
                    Some(json!({"tenant_id": "org-evorule", "username": "cli-admin", "password": "password123"})),
                )
            }
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(status, 200, "{body}");
        let token = body["access_token"].as_str().unwrap().to_string();

        // 3. bulk-export：数据集条目 → 目录批 JSON
        let out_dir = std::env::temp_dir().join(format!("cli-e2e-out-{}", std::process::id()));
        run_blocking(&base, &token, Cmd::EntriesBulkExport {
            id: "ds-cli-e2e".into(),
            dir: out_dir.to_string_lossy().to_string(),
            version: None,
        })
        .await
        .unwrap();
        assert!(out_dir.join("e1.json").exists(), "bulk-export 应落 e1.json");

        // 4. bulk-import 不带证据 → 闸门一如实拒绝（含自助指引）
        let err = run_blocking(&base, &token, Cmd::EntriesBulkImport {
            dir: out_dir.to_string_lossy().to_string(),
            dataset: Some("ds-cli-import".into()),
            tests: None,
        })
        .await
        .unwrap_err();
        assert!(err.0.contains("沙箱验证未通过"), "{err}");

        // 5. bulk-import 携带证据（测试工作台产出形态）→ 导入成功
        let tmp = std::env::temp_dir().join(format!("cli-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tests_file = tmp.join("tests.json");
        std::fs::write(&tests_file, json!({"verdict": "pass"}).to_string()).unwrap();
        run_blocking(&base, &token, Cmd::EntriesBulkImport {
            dir: out_dir.to_string_lossy().to_string(),
            dataset: Some("ds-cli-import".into()),
            tests: Some(tests_file.to_string_lossy().to_string()),
        })
        .await
        .unwrap();

        // 6. API filter 集成：domain:tax 命中 1 条、非法段 400、往返内容一致
        let (status, body) = tokio::task::spawn_blocking({
            let base = base.clone();
            let token = token.clone();
            move || {
                crate::cli::send(
                    ureq::get(&format!("{base}/v1/datasets/ds-cli-import/entries?filter=domain:tax")),
                    Some(&token),
                    None,
                )
            }
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(status, 200, "{body}");
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        // 往返内容一致（bulk-export → bulk-import → rule_body 逐字节一致）
        assert_eq!(items[0]["rule_body"], e2e_entry().rule_body);
        assert_eq!(items[0]["entry_id"], "e1");

        let (status, body) = tokio::task::spawn_blocking({
            let base = base.clone();
            let token = token.clone();
            move || {
                crate::cli::send(
                    ureq::get(&format!("{base}/v1/datasets/ds-cli-import/entries?filter=noseg")),
                    Some(&token),
                    None,
                )
            }
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(status, 400, "非法段必须显式 400: {body}");

        std::fs::remove_dir_all(&out_dir).ok();
        std::fs::remove_dir_all(&tmp).ok();
    }
}
