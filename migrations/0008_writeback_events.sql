-- 0008_writeback_events.sql —— 回写通道收件队列（T1 追认队列形态）
--
-- 演进还原（schema 治理批）：SQLite 侧引入于回写收件通道
-- 批次（POST /v1/writeback/rule_failure 收件端点）。收件即入队，补丁
-- 动作等数据说话，首版只收不触发；事件原文单源 RuleFailureEvent。
-- 此表此前被并入 0001 快照，拆分批按演进史归位为独立增量。

CREATE TABLE IF NOT EXISTS writeback_events (
    id           BIGSERIAL PRIMARY KEY,
    tenant_id    TEXT NOT NULL,
    dataset_id   TEXT NOT NULL,
    entry_id     TEXT NOT NULL,
    version_used TEXT NOT NULL,
    failure_type TEXT,
    event        TEXT NOT NULL,              -- RuleFailureEvent 原文 JSON
    received_at  TEXT NOT NULL               -- ISO-8601 UTC
);
CREATE INDEX IF NOT EXISTS idx_writeback_tenant ON writeback_events(tenant_id, received_at);
