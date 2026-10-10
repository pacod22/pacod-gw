-- Attachments of audit records kept in full (concept section 14.6): stored once per workspace
-- and content, and referred to from the records by hash (`message_audit.attachments`).
--
-- Expiry: every record that refers to an attachment stores it again with the record's
-- expiry. ReplacingMergeTree keeps the row with the latest expiry, so an attachment lives as
-- long as the last record that refers to it. Erasing records removes the attachments of their
-- workspace that no remaining record refers to.
CREATE TABLE IF NOT EXISTS audit_attachments
(
    workspace_id  LowCardinality(String),
    -- SHA-256 of the content, in hex
    hash          String,
    content       String CODEC(ZSTD(3)),
    bytes         UInt64,
    expires_at    DateTime64(9, 'UTC')
)
ENGINE = ReplacingMergeTree(expires_at)
ORDER BY (workspace_id, hash)
TTL expires_at DELETE
SETTINGS non_replicated_deduplication_window = 10000;

-- The hashes of the attachments a record refers to.
ALTER TABLE message_audit ADD COLUMN IF NOT EXISTS attachments Array(String) AFTER truncated;
