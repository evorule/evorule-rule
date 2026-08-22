-- 0001_initial.sql —— evorule-rule PostgreSQL 初始 schema（45 号批次1 §2.3）
--
-- 对齐 store/mod.rs 的 SQLite 建表（31/33/34/35/36/37/43/44），完整复刻全部表；
-- 含 45 号新增 dataset_versions 快照表（C 类数据集版本↔条目内容归因）。
-- 生产级后续，默认构建不编译（见 Cargo.toml `postgres` feature / store/pg.rs）。
--
-- 注：当前为 schema 基线快照。执行侧动态迁移（sqlx migrate 版本化 + 回滚脚本）
-- 在接入 PgStore 查询层时启用；本文件以幂等 DDL 为主，便于首次建库。

-- 43 号：认证与用户身份（正交 A）
CREATE TABLE IF NOT EXISTS tenants (
    tenant_id   TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    instance_id TEXT NOT NULL,              -- 39 号：真实实例身份，进溯源
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    user_id       TEXT PRIMARY KEY,
    tenant_id     TEXT NOT NULL REFERENCES tenants(tenant_id),
    username      TEXT NOT NULL,
    password_hash TEXT NOT NULL,            -- PBKDF2-HMAC-SHA256（MVP；生产 Argon2id，见 45 §6）
    salt          TEXT NOT NULL,
    role          TEXT NOT NULL,            -- viewer/rule_engineer/approver/admin
    disabled      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (tenant_id, username)
);

CREATE TABLE IF NOT EXISTS auth_audits (
    audit_id   TEXT PRIMARY KEY,
    action     TEXT NOT NULL,               -- register/login/refresh/logout/disable_user (+key.access/rotate/revoke)
    user_id    TEXT,
    tenant_id  TEXT NOT NULL,
    outcome    TEXT NOT NULL,               -- success | failure
    detail     TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_auth_audit_time ON auth_audits(tenant_id, created_at);

-- 43 号 §3.3：JWT 撤销黑名单
CREATE TABLE IF NOT EXISTS revoked_tokens (
    jti        TEXT PRIMARY KEY,
    tenant_id  TEXT NOT NULL,
    user_id    TEXT,
    token_type TEXT NOT NULL,               -- access | refresh
    expires_at BIGINT NOT NULL,
    revoked_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_revoked_token_exp ON revoked_tokens(expires_at);

-- 44 号 §14：API Key（仅存哈希）
CREATE TABLE IF NOT EXISTS api_keys (
    key_id     TEXT PRIMARY KEY,
    tenant_id  TEXT NOT NULL REFERENCES tenants(tenant_id),
    name       TEXT NOT NULL,
    scope      TEXT NOT NULL,               -- pull（MVP）
    key_hash   TEXT NOT NULL,               -- SHA-256(token)
    created_at TEXT NOT NULL,
    revoked_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_api_keys_tenant ON api_keys(tenant_id, revoked_at);

-- 31 号：数据集
CREATE TABLE IF NOT EXISTS datasets (
    dataset_id   TEXT PRIMARY KEY,
    tenant_id    TEXT NOT NULL,
    name         TEXT NOT NULL,
    description  TEXT,
    domain       TEXT NOT NULL DEFAULT '[]',   -- JSON
    tags         TEXT NOT NULL DEFAULT '[]',   -- JSON
    visibility   TEXT NOT NULL DEFAULT 'Private',
    lifecycle    TEXT NOT NULL,                -- JSON（5 态 + state_history）
    versioning   TEXT NOT NULL,                -- JSON（版本链 + double-lane）
    law_ref      TEXT,                         -- JSON nullable
    version_selection TEXT,                    -- JSON nullable
    data_dependencies TEXT,                    -- JSON nullable
    meta         TEXT NOT NULL,                -- JSON（created/updated）
    content_hash TEXT
);
CREATE INDEX IF NOT EXISTS idx_datasets_tenant ON datasets(tenant_id, visibility);

-- 31 号：条目
CREATE TABLE IF NOT EXISTS entries (
    dataset_id  TEXT NOT NULL REFERENCES datasets(dataset_id),
    entry_id    TEXT NOT NULL,
    version     INTEGER NOT NULL,
    status      TEXT,
    provenance  TEXT NOT NULL,                -- JSON
    domain      TEXT NOT NULL,
    tags        TEXT NOT NULL DEFAULT '[]',
    data_source_binding TEXT NOT NULL DEFAULT '[]',
    consumed_inputs TEXT NOT NULL DEFAULT '[]',
    rule_body   TEXT NOT NULL,                -- evorule 原生 JSON（零转译）
    governance  TEXT,                         -- JSON nullable
    content_hash TEXT NOT NULL,
    PRIMARY KEY (dataset_id, entry_id, version)
);
CREATE INDEX IF NOT EXISTS idx_entries_domain ON entries(dataset_id, domain);
CREATE INDEX IF NOT EXISTS idx_entries_hash ON entries(dataset_id, content_hash);

-- 33 §6 / C1：内容寻址快照去重
CREATE TABLE IF NOT EXISTS entry_snapshots (
    dataset_id   TEXT NOT NULL REFERENCES datasets(dataset_id),
    content_hash TEXT NOT NULL,
    rule_body    TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (dataset_id, content_hash)
);
CREATE INDEX IF NOT EXISTS idx_snapshots_ds ON entry_snapshots(dataset_id);

-- 45 号批次1 / 33 §6：数据集版本内容归因快照（闭合 C 类残留）
CREATE TABLE IF NOT EXISTS dataset_versions (
    dataset_id TEXT NOT NULL REFERENCES datasets(dataset_id),
    version    TEXT NOT NULL,
    entry_hash TEXT NOT NULL,
    entry_id   TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (dataset_id, version, entry_hash)
);
CREATE INDEX IF NOT EXISTS idx_dsver_ds ON dataset_versions(dataset_id, version);

-- 34 号：条目级状态迁移审计（only-append）
CREATE TABLE IF NOT EXISTS entry_state_history (
    id         SERIAL PRIMARY KEY,
    dataset_id TEXT NOT NULL,
    entry_id   TEXT NOT NULL,
    version    INTEGER NOT NULL,
    from_state TEXT,
    to_state   TEXT NOT NULL,
    at         TEXT NOT NULL,
    by         TEXT NOT NULL,
    cause      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_esh_entry ON entry_state_history(dataset_id, entry_id, version);

-- 35 号 §5：无凭据服务模板
CREATE TABLE IF NOT EXISTS service_templates (
    template_id       TEXT PRIMARY KEY,
    tenant_id         TEXT NOT NULL REFERENCES tenants(tenant_id),
    service_name      TEXT NOT NULL,
    kind              TEXT NOT NULL,          -- pull | push
    io_contract       TEXT NOT NULL,          -- JSON
    endpoint_template TEXT NOT NULL,
    method            TEXT,
    headers_template  TEXT NOT NULL DEFAULT '{}',
    placeholder_notes TEXT NOT NULL DEFAULT '{}',
    created_at        TEXT NOT NULL,
    created_by        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_templates_tenant ON service_templates(tenant_id, service_name);

-- 44 号 §9：快照包导入流水（content_hash 幂等去重）
CREATE TABLE IF NOT EXISTS bundles_import_logs (
    log_id      TEXT PRIMARY KEY,
    dataset_id  TEXT NOT NULL,
    bundle_hash TEXT NOT NULL,
    status      TEXT NOT NULL,
    detail      TEXT,
    operator    TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_bundle_ds ON bundles_import_logs(dataset_id);

-- 37 号 §8：LLM 命名操作审计
CREATE TABLE IF NOT EXISTS llm_op_audit (
    request_id  TEXT PRIMARY KEY,
    operation   TEXT NOT NULL,
    model       TEXT,
    status      TEXT NOT NULL,               -- completed | failed
    duration_ms BIGINT NOT NULL,
    result_ref  TEXT,
    error       TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_op_time ON llm_op_audit(operation, created_at);

-- 45 号 §2.4：配额与计量（42 号模式：按日聚合）—— 预留，未接入查询层
CREATE TABLE IF NOT EXISTS usage_records (
    id         SERIAL PRIMARY KEY,
    tenant_id  TEXT NOT NULL,
    resource   TEXT NOT NULL,               -- e.g. entries / bytes / api_calls
    day        DATE NOT NULL,
    amount     BIGINT NOT NULL DEFAULT 0,
    UNIQUE (tenant_id, resource, day)
);