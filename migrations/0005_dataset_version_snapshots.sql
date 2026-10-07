-- 0005_dataset_version_snapshots.sql —— 数据集版本内容归因快照
--
-- 演进还原（schema 治理批）：SQLite 侧引入于历史版本快照落库
-- 批次（export_bundle_at 解锁历史版本导出）。kind 区分 rule | knowledge，
-- content_json 存升版时刻的完整条目 JSON（与 dataset_versions.entry_hash
-- 同源 BLAKE3 口径）。

CREATE TABLE IF NOT EXISTS dataset_version_snapshots (
    dataset_id   TEXT NOT NULL,
    version      TEXT NOT NULL,             -- 数据集版本号（升版时刻的旧版本 = 留档对象）
    entry_id     TEXT NOT NULL,
    kind         TEXT NOT NULL,             -- rule | knowledge
    content_hash TEXT NOT NULL,             -- 与 dataset_versions.entry_hash 同源（BLAKE3）
    content_json TEXT NOT NULL,             -- 完整条目 JSON（RuleEntry / KnowledgeEntry）
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (dataset_id, version, entry_id),
    FOREIGN KEY (dataset_id) REFERENCES datasets(dataset_id)
);
CREATE INDEX IF NOT EXISTS idx_dvsnap_ver ON dataset_version_snapshots(dataset_id, version);
