-- 0003_knowledge_assets.sql —— 知识数据集平行表（知识数据资产化，平行表方案定案）
--
-- 演进还原（schema 治理批）：SQLite 侧引入于知识数据资产化
-- 批次（knowledge 数据集治理链接入），同批含 datasets.dataset_kind 列。
-- 平行表定位：rule 查询热路径零扰动（entries 不动，knowledge 独立三表）。
-- 注：SQLite 侧另建 knowledge_fts（FTS5 trigram）+同步触发器做全文索引——
-- SQLite 特有机制，pg 侧检索面未接线，不移植（如实留档）。

-- knowledge 条目（与 entries 平行；payload=领域结构化 JSON，零转译）
CREATE TABLE IF NOT EXISTS knowledge_entries (
    dataset_id   TEXT NOT NULL,
    entry_id     TEXT NOT NULL,
    version      INTEGER NOT NULL,
    status       TEXT,
    provenance   TEXT NOT NULL,                -- JSON
    domain       TEXT NOT NULL,
    tags         TEXT NOT NULL DEFAULT '[]',   -- JSON array
    payload      TEXT NOT NULL,                -- 领域结构化 JSON（content_hash 的内容源）
    schema_ref   TEXT NOT NULL,                -- 领域 JSON Schema 引用 URI
    governance   TEXT,                         -- JSON nullable
    knowledge_meta TEXT,                       -- JSON nullable（kind/trust/license/contract 打包）
    content_hash TEXT NOT NULL,
    PRIMARY KEY (dataset_id, entry_id, version),
    FOREIGN KEY (dataset_id) REFERENCES datasets(dataset_id)
);
CREATE INDEX IF NOT EXISTS idx_kentry_ds ON knowledge_entries(dataset_id);

-- knowledge 条目状态迁移审计（only-append；独立于 entry_state_history）
CREATE TABLE IF NOT EXISTS knowledge_state_history (
    id         SERIAL PRIMARY KEY,
    dataset_id TEXT NOT NULL,
    entry_id   TEXT NOT NULL,
    version    INTEGER NOT NULL,
    from_state TEXT,
    to_state   TEXT NOT NULL,
    at         TEXT NOT NULL,
    by         TEXT NOT NULL,
    cause      TEXT NOT NULL,
    FOREIGN KEY (dataset_id, entry_id, version)
        REFERENCES knowledge_entries(dataset_id, entry_id, version)
);

-- knowledge 内容寻址快照去重（设计文档 §6 同语义）
CREATE TABLE IF NOT EXISTS knowledge_snapshots (
    dataset_id   TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    payload      TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (dataset_id, content_hash),
    FOREIGN KEY (dataset_id) REFERENCES datasets(dataset_id)
);

-- datasets 补 dataset_kind 列（存量默认 rule_set）
ALTER TABLE datasets ADD COLUMN dataset_kind TEXT NOT NULL DEFAULT 'rule_set';
