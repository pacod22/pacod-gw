-- Four more attribution columns, sent by the caller in the `gateway` object or as `x-gw-*`
-- headers (concept sections 5.6 and 9.2): the agent that made the call, the platform user the
-- agent acts for (independent of end_user), the task as a short label, and the complexity the
-- caller declares (low, medium or high). Rows written before this migration read as empty.
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS agent_id   LowCardinality(String) DEFAULT '' AFTER tags,
    ADD COLUMN IF NOT EXISTS user_id    String DEFAULT '' AFTER agent_id,
    ADD COLUMN IF NOT EXISTS task       LowCardinality(String) DEFAULT '' AFTER user_id,
    ADD COLUMN IF NOT EXISTS complexity LowCardinality(String) DEFAULT '' AFTER task;

ALTER TABLE message_audit
    ADD COLUMN IF NOT EXISTS agent_id   LowCardinality(String) DEFAULT '' AFTER trace_id,
    ADD COLUMN IF NOT EXISTS user_id    String DEFAULT '' AFTER agent_id,
    ADD COLUMN IF NOT EXISTS task       LowCardinality(String) DEFAULT '' AFTER user_id,
    ADD COLUMN IF NOT EXISTS complexity LowCardinality(String) DEFAULT '' AFTER task;
