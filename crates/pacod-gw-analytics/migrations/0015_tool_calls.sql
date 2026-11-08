-- One row per call of a tool or an agent through the gateway (catalogue MCP-14): an MCP tool
-- call on /mcp, or a request to an agent on /a2a. The columns mirror the JSON of
-- `ToolCallRecord` (crates/pacod-gw-events/src/tool.rs) field by field and in the same order.
-- The insert rejects unknown fields, so a field added to the struct needs a migration that adds
-- its column.
--
-- A table of its own, not rows of usage_events: a tool call uses no tokens and has no model, and
-- counting it there would change every report of tokens, cost and requests.
--
-- The arguments of a call are not stored; `arguments_hmac` is a keyed hash of them.
--
-- Duplicates are handled as for usage_events: the batch id is the insert deduplication token,
-- and ReplacingMergeTree collapses a record that arrives again in a different batch. Retention
-- follows the usage events' (jobs/retention.rs).
--
-- The core ships these records to /ingest/tool-calls, which an older analytics service does not
-- have: roll out the analytics service before the core (runbook section 6).
CREATE TABLE IF NOT EXISTS tool_calls
(
    -- identity
    id                 UUID,
    timestamp          DateTime64(9, 'UTC'),
    instance_id        LowCardinality(String),

    -- scope
    organisation_id    LowCardinality(String),
    workspace_id       LowCardinality(String),
    key_id             String,

    -- attribution
    end_user           String,
    session_id         String,
    trace_id           String,
    tags               Map(String, String),
    agent_id           LowCardinality(String),
    user_id            String,
    task               LowCardinality(String),

    -- what was called
    protocol           LowCardinality(String),
    protocol_version   LowCardinality(String),
    target             LowCardinality(String),
    target_id          String,
    operation          LowCardinality(String),
    exposed_name       LowCardinality(String),
    pin_hash           String,
    arguments_hmac     String,

    -- outcome
    verdict            LowCardinality(String),
    status             LowCardinality(String),
    http_status        UInt16,
    rpc_error_code     Int32,
    error_type         LowCardinality(String),
    error_message      String,
    result_error       Bool,

    -- size and time
    request_bytes      UInt64,
    response_bytes     UInt64,
    truncated          Bool,
    upstream_ms        Float32,
    total_ms           Float32,

    INDEX idx_id id TYPE bloom_filter(0.01) GRANULARITY 1,
    INDEX idx_trace_id trace_id TYPE bloom_filter(0.01) GRANULARITY 1
)
ENGINE = ReplacingMergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (organisation_id, workspace_id, timestamp, id)
SETTINGS non_replicated_deduplication_window = 10000;
