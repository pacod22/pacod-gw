-- What plugins found and did (docs/plugin-concept.md, section 6.1): the intent, its confidence
-- and the engine that answered; the strongest verdict and a plugin's error; the plugins' time
-- before the provider call; and, on a call a plugin made for itself, the call it looked at.
-- Rows written before this migration read as empty: no plugin.
--
-- The core sends these fields on every event, and an older analytics service drops the fields
-- it does not know: roll out the analytics service before the core (runbook section 6).
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS intent            LowCardinality(String) DEFAULT '' AFTER audit_id,
    ADD COLUMN IF NOT EXISTS intent_confidence Float32 DEFAULT 0 AFTER intent,
    ADD COLUMN IF NOT EXISTS intent_engine     LowCardinality(String) DEFAULT '' AFTER intent_confidence,
    ADD COLUMN IF NOT EXISTS plugin_verdict    LowCardinality(String) DEFAULT '' AFTER intent_engine,
    ADD COLUMN IF NOT EXISTS plugin_error      LowCardinality(String) DEFAULT '' AFTER plugin_verdict,
    ADD COLUMN IF NOT EXISTS parent_id         String DEFAULT '' AFTER plugin_error,
    ADD COLUMN IF NOT EXISTS plugin_ms         Float32 DEFAULT 0 AFTER total_ms;
