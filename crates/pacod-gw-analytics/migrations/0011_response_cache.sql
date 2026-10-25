-- What the response cache did with a call (docs/plugin-concept.md, section 15): `hit`, `miss`,
-- `would_hit`, `refresh` or `bypass`, empty without a response cache; and, on a hit, what the
-- kept answer would have cost. Rows written before this migration read as empty: no cache.
--
-- The core sends these fields on every event, and an older analytics service drops the fields
-- it does not know: roll out the analytics service before the core (runbook section 6).
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS cache            LowCardinality(String) DEFAULT '' AFTER parent_id,
    ADD COLUMN IF NOT EXISTS cache_saved_cost Nullable(Float64) AFTER cache;
