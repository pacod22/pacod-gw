-- Audit mode for batches (concept section 14.6): the requests of a batch's items, kept from the
-- call that brought them (a batch's creation, or its input file's upload) until the batch is
-- settled, so that each item's record holds its request with its result. Rows exist only for
-- calls audit mode applies to. Additive: an older instance ignores the table.
CREATE TABLE IF NOT EXISTS audit_batch_items (
    -- The request id of the call that brought the requests.
    items_key          text NOT NULL,
    custom_id          text NOT NULL,
    workspace_id       text NOT NULL,
    end_user           text NOT NULL DEFAULT '',
    -- The item as the caller sent it, and as it went to the provider: as the record keeps
    -- them (attachments referenced where the workspace says so, cut at the size limit), with
    -- their sizes before that.
    received           bytea NOT NULL,
    received_bytes     bigint NOT NULL,
    received_truncated boolean NOT NULL DEFAULT false,
    sent               bytea NOT NULL,
    sent_bytes         bigint NOT NULL,
    sent_truncated     boolean NOT NULL DEFAULT false,
    resolution         jsonb NOT NULL,
    -- The end user's records were erased: the item is not recorded when its batch settles.
    erased             boolean NOT NULL DEFAULT false,
    created_at         timestamptz NOT NULL DEFAULT now(),
    expires_at         timestamptz NOT NULL,
    PRIMARY KEY (items_key, custom_id)
);
CREATE INDEX IF NOT EXISTS audit_batch_items_by_end_user ON audit_batch_items (workspace_id, end_user);
CREATE INDEX IF NOT EXISTS audit_batch_items_by_expiry ON audit_batch_items (expires_at);
