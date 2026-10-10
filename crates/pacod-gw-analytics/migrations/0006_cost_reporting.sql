-- The cost converted into the installation's reporting currency when the call settled
-- (concept section 13.1), and that currency. Reports add these up, so that costs in several
-- currencies are not mixed. Rows written before this migration have no converted cost; the
-- reports take their cost as it was recorded.
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS cost_reporting Nullable(Float64) AFTER currency,
    ADD COLUMN IF NOT EXISTS reporting_currency LowCardinality(String) DEFAULT '' AFTER cost_reporting;
