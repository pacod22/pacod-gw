-- What a hit of the response cache saved, in the reporting currency (docs/plugin-concept.md,
-- section 15), converted by the core as it converts costs: the analytics service holds no
-- exchange rates. Rows written before this migration read as unconverted, and reports then
-- count the saved cost as recorded, as they do costs.
--
-- The core sends the field on every hit, and an older analytics service drops the fields it
-- does not know: roll out the analytics service before the core (runbook section 6).
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS cache_saved_cost_reporting Nullable(Float64) AFTER cache_saved_cost;
