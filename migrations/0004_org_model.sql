-- 0004_org_model.sql —— 双层租户：platform/org 两级结构
--
-- 演进还原（schema 治理批）：SQLite 侧引入于双层租户落地
-- 批次。平台层（platform）之下按 org 划分；datasets.tenant_id 等字段
-- 语义平移为 org id，字段名不变零迁移（orgs 不承载外键改写）。

CREATE TABLE IF NOT EXISTS orgs (
    org_id     TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    disabled   INTEGER NOT NULL DEFAULT 0,   -- 停用的 org 拒绝新登录/刷新
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_org_memberships (
    user_id    TEXT NOT NULL,
    org_id     TEXT NOT NULL,
    role       TEXT NOT NULL,                -- viewer/rule_engineer/approver/admin/platform_admin
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, org_id),
    FOREIGN KEY (user_id) REFERENCES users(user_id)
);
CREATE INDEX IF NOT EXISTS idx_membership_org ON user_org_memberships(org_id);
