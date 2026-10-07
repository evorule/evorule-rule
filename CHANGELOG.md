# Changelog

本文件记录 evorule-rule 的对外可感知变更。格式参考 Keep a Changelog，版本号遵循仓库 Cargo.toml 语义化版本。

## [Unreleased]

### Added — 服务目录种子新增 template_render（template-services 插件）

- 官方预置原生服务 +1：`template_render`（非敏感）——上下文 + 模板 → JSON / Markdown / 纯文本的确定性渲染（`{{}}` 家族语法，if/for 最小集）。嵌入副本 `src/model/official_native_services.template-services.embedded.json` 由 sync-native-services.ps1 从执行侧 SSOT 同步，快照期望表随动更新。

### Changed — PostgreSQL migrations 单文件拆分为增量序列

- `migrations/0001_initial.sql` 回退为初始基线态，后续 schema 演进按 git 考古还原为 `0002_service_catalog` → `0008_writeback_events` 增量序列（含 org 双层租户 / 知识资产化三表 / dataset_version_snapshots / 机器闸审计列等此前 pg 迁移缺项）。
- 迁移版本表 = sqlx 内置 `_sqlx_migrations`（版本化 + 逐版本增量跳过）；真实 PostgreSQL 16 实测：全新库 8 个版本依序应用全绿，已迁移库重连校验+全跳过。
- 注意：0001 内容相对旧快照有变更，按旧版 0001 建过库的存量 pg 实例需按 sqlx checksum 校验口径处置（修复前 pg 存储为 feature 门控未部署态，无存量实例受影响）。
- pg 存储层同批清偿：`postgres` feature 编译修复（StateChange 初始化器随机器闸三列对齐）。

### Changed — 认证密码哈希升级 PBKDF2 → Argon2id

- 新密码哈希一律 **Argon2id**（OWASP 2023 参数 m=19MiB/t=2/p=1），存储形态 `a2$<hex>`（世代前缀）。
- **存量透明迁移**：升级前 PBKDF2-HMAC-SHA256 哈希（无前缀 hex 形态）在登录校验通过后就地重哈希 Argon2id 落库（同盐换算法），**无强制重置**；迁移在首个成功登录时自动完成，无需管理动作。
- `DEFAULT_PBKDF2_ITERATIONS`（600_000）常量**退役**：仅保留用于存量哈希的 legacy 校验路径，不再参与任何新哈希；后续版本迭代数配置项随存量迁移完成度评估移除。
- 校验全程保持 subtle 恒时比较；`verify_password` 对损坏存储值（长度不符/非法 hex）安全拒绝。
- 新增 `RuleStore::update_user_password_hash`（登录期透明迁移专用存储通路）。
