# Changelog

evorule-rule 的所有显著变更都记录在此文件。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/),版本遵循 [SemVer 2.0](https://semver.org/lang/zh-CN/)。

> 仓库位置:`gitee.com/evorule/evorule-rule` · 协议:AGPL-3.0-or-later · 设计稿:`D:\evo-agent\_design_docs\` 30-47 号(决策点 ①-⑨)

---

## [0.2.0] - 2026-08-22

**首次对外发布** — 数据治理系统 MVP + REST API + 认证 + LLM 客户端 + PG schema + 密钥管理 全部落地

> 注:历史上内部存在 commit `1b0feec`(commit message 自标 v0.1.0),该次为对内开发节点,从未对外发布或打 tag。本次为 evorule-rule 首次 `git push` + `git tag` + 公开 release。

### 🆕 新增

**LLM 消费方(决策点 ⑦,设计稿 37 号)**

- LlmClient 真实 HTTP 冒烟集成测试(方案 B,3 次 HTTP 调用,30+ 断言)
- 操作级 LLM 审计落库(决策点 ⑦ §8)
- 审计查询与统计展示接口(37 号 §8 对外层)

**认证与用户身份(决策点 ⑧ 起步,设计稿 43 号)**

- 认证与用户身份层 MVP(43 号 正交 A):Bearer token 签发/验证/吊销、登出撤销 + jti 黑名单、发布前凭据扫描、LLM 产出离开 Draft 拦截

**REST API 面(设计稿 44 号)**

- 完整 REST 路由(44 号 正交 B):`/v1/datasets` / `/v1/entries` / `/v1/dependencies` / `/v1/services` / `/v1/search` / `/v1/bundles` 等
- 条目顶层 CRUD + submit-candidate + approve + history + deps
- 依赖 deps 读写 + 服务模板注册/列表/详情/bind
- 检索数据集/条目 + 版本 diff
- 快照包导出/裁剪/导入/预检/状态
- 列表/检索分页(`{items, next_cursor}` + `limit`/`offset`)
- `Idempotency-Key` 幂等中间件(同 key 同负载返回缓存/异负载 409/并发在途 409)

**内容哈希(决策点 C1/C2)**

- `entry_snapshots` 内容寻址快照 + 去重统计 + 版本历史回查
- 条目内容级 diff(`entry_content_diff` + `GET /entries/{id}/diff`)

**PostgreSQL 双后端(45 号批次1)**

- `AppState` 新增 `BackendKind::{Sqlite, Postgres}` + `pg_smoke` 标志
- `bootstrap_backend` 启动靴:`DATABASE_URL` 选型,`PgStore::smoke_check` 门控建池+迁移+最小 CRUD 往返;失败如实回落 SQLite,绝不伪造
- `GET /v1/admin/backend` 管理员自检端点,实时报告活跃后端 + PG 探针结果
- `PgStore` 完整 12 张核心表 CRUD:`datasets` / `entries` / `service_templates` / `api_keys` / `llm_op_audit` / `auth_audits` / `revoked_tokens` / `users` / `dataset_versions` / `entry_state_history` / `bundles_import_logs` + `usage_records` 预留
- 真实 PG 集成测试覆盖 12 表往返全绿
- 修复 PG 方言类型映射 gap:`INTEGER=INT4`(32 位) vs SQLite `INTEGER`(8 字节),`version` 列按 `i32` 读写
- 修复 `tenant` 外键完整性、共享库测试幂等、`disabled INTEGER` 列 `i32` bind

**密钥管理(45 号批次1,设计稿 45 号)**

- K1:`model/secret.rs` `SecretScope` 五类(APIKey / JwtSigning / Tenant / User / Service) + `SecretKey` + `KeyRing` 双代容器 + `rotate()` 双代轮换(HS256 保持兼容)
- K3:`derive_salt_from_secret` HKDF 盐基元
- K4:`AuthService` 双代验签(active 优先 / previous 兜底;签发用 active;轮换后旧 token 仍兼容)
- K6:`keyring.rs` 便捷封装(`KeyRing` → `AuthService`);`key.rotate()` 审计落库
- `persist_key_audit` 持久化轮换事件

**数据模型层(决策点 ①,设计稿 31 号,基础)**

- `RuleDataset` / `RuleEntry` / `Provenance` / `RuleDependency` / `Lifecycle` / `Governance` / `Version` 七大数据模型

**版本解析(决策点 ③,设计稿 33 号)**

- `VersionResolver` 双选择:`auto_by_effective_date`(自动取当前有效日期) / `pinned`(锁定版本)

**生命周期(决策点 ④,设计稿 34 号)**

- 5 态状态机(`Draft` → `Candidate` → `Approved` → `Published` → `Deprecated`)
- `transition_dataset_status` + `publish_dataset` 强类型转移函数
- `is_publicly_pullable` 双条件(状态 = Published AND 已发布审批)

**快照包(决策点 ⑥,设计稿 36 号)**

- 导出 / 导入校验 / 裁剪
- SHA-256 内容寻址防篡改
- `trimmer` 依赖级裁剪(只导出 required deps,避免循环引用)

### 🔄 变更

- `--features postgres` 门控:sqlx 仅在启用 PG 后端时编译,默认 build 不负重
- `Cargo.toml` description 仍为 v0.1.0 时期描述(数据模型层);v0.3.0 整体 release 时同步更新

### 🐛 修复

- PG `INTEGER=INT4` 对齐 SQLite `INTEGER` 8 字节(commit `6326988`)
- `update_dataset` `Option` 字段序列化为 `null` 字符串导致读回失败(commit `cba3b83`)
- `scan_credentials` 三处 `manual_char_comparison` 告警(数组字面量代替闭包比较,commit `8f7921a`)
- 登出 token 撤销 + jti 黑名单,杜绝"登出后 token 仍可用"安全洞(commit `11a93c0`)

### 🧪 测试

- 108 测试全绿(默认 `cargo test --workspace` + `cargo test --features postgres --workspace`)
- 双 `cargo clippy --workspace --all-targets -- -D warnings` 干净
- 真实 PostgreSQL 集成测试覆盖 12 表 CRUD 往返
- LlmClient 真实 HTTP 冒烟(网络可达时)

### 📦 依赖

新增(`Cargo.toml`):

- `subtle = "2"` — 常数时间比较(K6 密钥比对)
- `rand = "0.8"` — 随机 salt/secret 生成
- `base64 = "0.22"` — JWT/Key 编码
- `ureq = "2"` — 同步 HTTP 客户端(LLM 冒烟,无异步依赖)
- `axum = "0.8"` — REST 框架(44 号)
- `tokio = "1"` features=["full"] — async runtime
- `tower = "0.5"` features=["util"] — 中间件栈
- `http-body-util = "0.1"` — axum body 工具
- `sqlx = "0.8"` features=["runtime-tokio", "postgres", "migrate", "json"] — PG 驱动(门控)

### ⚠️ BREAKING / Deferred 项(如实标注)

- K2 RSA/RS256 + Vault/KMS 集成:**未做**(v0.2.x 后续)
- K7 信封加密 / 静态加密 at-rest:**未做**(v0.2.x 后续)
- 决策点 ⑧ 多租户 RBAC 完整实现(SQL 层 tenant_id 过滤 + 跨租户拒绝 + 4 角色递进):**未做**(v0.2.x 后续,仅 token 层认证 + tenant_id 字段已落)
- 决策点 ⑨ 白标 + 部署规格 + Docker 化:**未做**(v0.3+)
- usage_records 配额预留(45 号设计):store 层无写入路径,本轮不落地(如实标注,见 commit `44a7e44`)
- 全量 API 切 PG:SQLite 仍是活跃引擎;`store trait` 异步化是后续批次

### 🔗 设计稿对应

- 决策点 ① 数据模型 → 31 号
- 决策点 ② 版本单位 → 32 号
- 决策点 ③ 版本语义 → 33 号
- 决策点 ④ 生命周期 → 34 号
- 决策点 ⑤ 数据源绑定 → 35 号(本轮仅 schema,执行侧契约校验在 evorule-server 协同)
- 决策点 ⑥ 快照包 → 36 号
- 决策点 ⑦ LLM 客户端 → 37 号(等 evo-agent 服务化后接 07f E1 serve)
- 决策点 ⑧ 多租户 → 38 号(本轮 token 层起步)
- 决策点 ⑨ 白标 → 39 号(v0.3+)

---

## 历史说明(本仓库无 0.1.0 正式 release)

- 内部 commit `1b0feec`(2026-08-21 17:35)message 写"v0.1.0",但从未 tag / push / 公开。**不视为正式 v0.1.0 release**,仅作开发节点。
- 本次 v0.2.0 是 evorule-rule 首次 `git tag` + `git push` + 公开 release。

[0.2.0]: https://gitee.com/evorule/evorule-rule/releases/tag/v0.2.0
