-- Raw usage events, one row per proxied request (see crates/pacod-gw-events).
CREATE TABLE IF NOT EXISTS usage_events
(
    id                UUID,
    timestamp         DateTime64(3, 'UTC'),
    tenant_id         LowCardinality(String),
    api_key_id        String,
    provider          LowCardinality(String),
    model             LowCardinality(String),
    route             String,
    status_code       UInt16,
    latency_ms        UInt64,
    prompt_tokens     UInt32,
    completion_tokens UInt32,
    cost_usd          Float64,
    cached            Bool
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (tenant_id, timestamp);
