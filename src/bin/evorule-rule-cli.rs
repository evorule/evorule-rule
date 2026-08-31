//! evorule-rule-cli —— 治理服务 REST 命令行客户端（B3 段B 14 号）。
//!
//! 纯 REST 接入，不对 db 直连；导入走 /v1/bundles/import 同一校验链（单一 SSOT 门禁）。
//! 用法：
//!   evorule-rule-cli datasets list
//!   evorule-rule-cli datasets export ds-tax-2024 --out bundle.json
//!   evorule-rule-cli datasets import bundle.json
//!   evorule-rule-cli entries bulk-import ./my-dataset --dataset ds-new --tests tests.json
//!   evorule-rule-cli entries bulk-export ds-tax-2024 ./out-dir
//!
//! 认证：env `EVORULE_RULE_TOKEN`（Bearer）或 --token；服务地址 env `EVORULE_RULE_URL`。

use clap::Parser;

fn main() {
    let args = evorule_rule::cli::CliArgs::parse();
    if let Err(e) = evorule_rule::cli::run(args) {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}
