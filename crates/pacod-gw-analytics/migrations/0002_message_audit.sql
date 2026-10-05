-- The full record of each audited call: `AuditRecord` (crates/pacod-gw-events/src/audit.rs).
-- The scalar fields are columns. The four message parts and the three JSON values are stored as
-- JSON text, compressed with ZSTD. Rows are ordered by workspace, session and time, so the
-- repetition between the turns of one conversation compresses well (concept section 14.6).
--
-- Retention: `expires_at` comes from the record and ClickHouse deletes the row once it has
-- passed. Reads also filter on `expires_at`, so an expired record is never served while it
-- waits for the merge that removes it.
--
-- Duplicates are handled as for usage_events: the batch id is the insert deduplication token,
-- and ReplacingMergeTree collapses a record that arrives again in a different batch.
CREATE TABLE IF NOT EXISTS message_audit
(
    audit_id           String,
    event_id           UUID,
    timestamp          DateTime64(9, 'UTC'),
    expires_at         DateTime64(9, 'UTC'),

    organisation_id    LowCardinality(String),
    workspace_id       LowCardinality(String),
    key_id             String,
    end_user           String,
    session_id         String,
    trace_id           String,
    template           LowCardinality(String),
    model              LowCardinality(String),
    mode               LowCardinality(String),

    -- JSON text of one `AuditPart` (client_*) or of a list of them, one per attempt (provider_*)
    client_request     String CODEC(ZSTD(3)),
    provider_requests  String CODEC(ZSTD(3)),
    provider_responses String CODEC(ZSTD(3)),
    client_response    String CODEC(ZSTD(3)),

    -- JSON text of the three free-form values
    resolution         String CODEC(ZSTD(3)),
    timeline           String CODEC(ZSTD(3)),
    outcome            String CODEC(ZSTD(3)),

    truncated          Bool,

    INDEX idx_audit_id audit_id TYPE bloom_filter(0.01) GRANULARITY 1
)
ENGINE = ReplacingMergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (workspace_id, session_id, timestamp, audit_id)
TTL expires_at DELETE
SETTINGS non_replicated_deduplication_window = 10000;
