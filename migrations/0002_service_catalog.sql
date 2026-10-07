-- 0002_service_catalog.sql —— 服务目录（服务名/契约治理侧 SSOT）
--
-- 演进还原（schema 治理批）：SQLite 侧引入于治理侧功能收编
-- （service_catalog 建表，scope=platform 官方 + tenant 租户自定义）；
-- pg 迁移基线（0001）建库时点早于该批次，此处补齐。整型布尔口径
-- 沿用 0001（sensitive INTEGER，与 users.disabled 同风格）。

CREATE TABLE IF NOT EXISTS service_catalog (
    service_name TEXT PRIMARY KEY,
    version      TEXT NOT NULL DEFAULT '1.0.0',
    description  TEXT,
    io_contract  TEXT,                          -- JSON nullable
    sensitive    INTEGER NOT NULL DEFAULT 0,
    binding_hint TEXT NOT NULL DEFAULT 'native',
    managed_by   TEXT NOT NULL,                 -- official | org:<tenant_id>
    scope        TEXT NOT NULL,                 -- platform | tenant:<tenant_id>
    created_at   TEXT NOT NULL,
    updated_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_catalog_scope ON service_catalog(scope);
