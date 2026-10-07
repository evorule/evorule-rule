-- 0006_dataset_event_schemas.sql —— 数据集级 push 事件 schema 声明
--
-- 演进还原（schema 治理批）：SQLite 侧引入于数据集级 push
-- 事件 schema 声明批次（RuleDataset.event_schemas 落库/导出/导入全链路）。
-- JSON 数组列，存量默认 '[]'。

ALTER TABLE datasets ADD COLUMN event_schemas TEXT NOT NULL DEFAULT '[]';
