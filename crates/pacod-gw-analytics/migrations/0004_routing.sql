-- How routing chose the model (concept section 7.5): the scheme and its version, where it was
-- assigned (template, workspace, organisation, built_in), the rule that matched, and the
-- models a call moved on from before one served it. Rows written before this migration read
-- as empty: no scheme, no failover.
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS routing_scheme         LowCardinality(String) DEFAULT '' AFTER path,
    ADD COLUMN IF NOT EXISTS routing_scheme_version UInt32 DEFAULT 0 AFTER routing_scheme,
    ADD COLUMN IF NOT EXISTS routing_source         LowCardinality(String) DEFAULT '' AFTER routing_scheme_version,
    ADD COLUMN IF NOT EXISTS routing_rule           LowCardinality(String) DEFAULT '' AFTER routing_source,
    ADD COLUMN IF NOT EXISTS failovers              UInt8 DEFAULT 0 AFTER routing_rule,
    ADD COLUMN IF NOT EXISTS models_tried           Array(LowCardinality(String)) DEFAULT [] AFTER failovers;
