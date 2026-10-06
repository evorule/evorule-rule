# Changelog

本文件记录 evorule-rule 的对外可感知变更。格式参考 Keep a Changelog，版本号遵循仓库 Cargo.toml 语义化版本。

## [Unreleased]

### Changed — 认证密码哈希升级 PBKDF2 → Argon2id

- 新密码哈希一律 **Argon2id**（OWASP 2023 参数 m=19MiB/t=2/p=1），存储形态 `a2$<hex>`（世代前缀）。
- **存量透明迁移**：升级前 PBKDF2-HMAC-SHA256 哈希（无前缀 hex 形态）在登录校验通过后就地重哈希 Argon2id 落库（同盐换算法），**无强制重置**；迁移在首个成功登录时自动完成，无需管理动作。
- `DEFAULT_PBKDF2_ITERATIONS`（600_000）常量**退役**：仅保留用于存量哈希的 legacy 校验路径，不再参与任何新哈希；后续版本迭代数配置项随存量迁移完成度评估移除。
- 校验全程保持 subtle 恒时比较；`verify_password` 对损坏存储值（长度不符/非法 hex）安全拒绝。
- 新增 `RuleStore::update_user_password_hash`（登录期透明迁移专用存储通路）。
