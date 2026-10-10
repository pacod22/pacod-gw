-- The model the provider says answered, in its own words (concept section 8.3). When the
-- provider served another model than the one asked for, `model_served` names the repository
-- model it maps to and the call is priced at that model. Rows written before this migration
-- read as empty.
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS model_reported LowCardinality(String) DEFAULT '' AFTER model_served;
