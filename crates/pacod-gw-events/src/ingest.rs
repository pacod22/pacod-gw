//! Framing of the batches the core ships to the analytics service.
//!
//! A batch is a POST with a newline-delimited JSON body: one `UsageEvent` (or `AuditRecord`)
//! per line. The batch id is stable across retries of the same batch, so the analytics service
//! can hand it to ClickHouse as the deduplication token.

/// `POST` path for usage events.
pub const USAGE_PATH: &str = "/ingest/usage";
/// `POST` path for audit records.
pub const AUDIT_PATH: &str = "/ingest/audit";

/// Header carrying the batch id.
pub const BATCH_ID_HEADER: &str = "x-gw-batch-id";
/// Header carrying the shared secret between core and analytics.
pub const INTERNAL_TOKEN_HEADER: &str = "x-gw-internal-token";

/// Content type of a batch body.
pub const NDJSON: &str = "application/x-ndjson";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestReply {
    pub batch_id: String,
    pub rows: u64,
}
