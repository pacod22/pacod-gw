-- One row per usage event. The columns mirror the flattened JSON of `UsageEvent`
-- (crates/pacod-gw-events/src/usage.rs) field by field and in the same order. The insert
-- rejects unknown fields, so a field added to the struct needs a migration that adds its column.
--
-- Duplicates. Delivery is at least once (concept section 14.3).
--   * A batch delivered again under the same batch id is dropped on insert: the batch id is the
--     insert_deduplication_token, kept for the last non_replicated_deduplication_window inserts.
--   * An event that arrives again in a different batch has the same sorting key, so
--     ReplacingMergeTree collapses it. Reports read with FINAL and never see the copy.
-- The partition key is derived from `timestamp`, which is part of the sorting key, so two copies
-- of an event always land in the same partition.
CREATE TABLE IF NOT EXISTS usage_events
(
    -- identity
    id                     UUID,
    timestamp              DateTime64(9, 'UTC'),
    instance_id            LowCardinality(String),

    -- scope
    organisation_id        LowCardinality(String),
    workspace_id           LowCardinality(String),
    key_id                 String,

    -- attribution
    end_user               String,
    session_id             String,
    trace_id               String,
    tags                   Map(String, String),

    -- routing
    mode                   LowCardinality(String),
    interface              LowCardinality(String),
    provider               LowCardinality(String),
    model_requested        LowCardinality(String),
    model_served           LowCardinality(String),
    template               LowCardinality(String),
    template_version       UInt32,
    path                   String,

    -- outcome
    status                 LowCardinality(String),
    http_status            UInt16,
    error_type             LowCardinality(String),
    error_message          String,
    finish_reason          LowCardinality(String),
    attempts               UInt8,
    stream                 Bool,
    upstream_request_id    String,

    -- timing, milliseconds (concept section 14.2)
    gateway_delay_ms       Float32,
    receive_ms             Float32,
    pre_ms                 Float32,
    connect_ms             Float32,
    retry_ms               Float32,
    upstream_first_byte_ms Float32,
    upstream_stream_ms     Float32,
    post_ms                Float32,
    relay_max_ms           Float32,
    settle_ms              Float32,
    total_ms               Float32,

    -- usage
    input_tokens           UInt64,
    cache_read_tokens      UInt64,
    cache_write_tokens     UInt64,
    cache_write_5m_tokens  UInt64,
    cache_write_1h_tokens  UInt64,
    output_tokens          UInt64,
    reasoning_tokens       UInt64,
    characters             UInt64,
    server_tool_calls      UInt32,
    usage_source           LowCardinality(String),

    -- cost
    cost                   Nullable(Float64),
    currency               LowCardinality(String),
    price_version          UInt32,
    price_variant          LowCardinality(String),
    price_missing          Bool,

    -- other
    dropped_params         Array(String),
    audit_id               String,

    -- Point lookups that carry no time range: one request, one trace, one session.
    INDEX idx_id id TYPE bloom_filter(0.01) GRANULARITY 1,
    INDEX idx_trace_id trace_id TYPE bloom_filter(0.01) GRANULARITY 1,
    INDEX idx_session_id session_id TYPE bloom_filter(0.01) GRANULARITY 1
)
ENGINE = ReplacingMergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (organisation_id, workspace_id, timestamp, id)
SETTINGS non_replicated_deduplication_window = 10000;
