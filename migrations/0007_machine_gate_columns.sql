-- 0007_machine_gate_columns.sql —— 机器闸行权通路审计列（gate 写路径同事务审计链）
--
-- 演进还原（schema 治理批）：SQLite 侧引入于机器闸行权写路径批次
-- （machine-gate promotion write path with same-transaction audit chain）。
-- 两审计表各补 gate/tier/post_review_required 三列；存量行 NULL =
-- human 语义（向后兼容；先例 = consumed_inputs/dataset_kind 动态迁移）。

ALTER TABLE entry_state_history ADD COLUMN gate TEXT;
ALTER TABLE entry_state_history ADD COLUMN tier TEXT;
ALTER TABLE entry_state_history ADD COLUMN post_review_required INTEGER;

ALTER TABLE knowledge_state_history ADD COLUMN gate TEXT;
ALTER TABLE knowledge_state_history ADD COLUMN tier TEXT;
ALTER TABLE knowledge_state_history ADD COLUMN post_review_required INTEGER;
