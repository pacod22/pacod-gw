-- Files and message batches created at a provider through the gateway (concept section 8.3).
-- Provider credentials are usually shared between workspaces, and the provider tells objects
-- apart by credential only; the gateway records which workspace created each object, filters
-- lists to it, and refuses references to another workspace's objects.
CREATE TABLE provider_objects (
    -- The provider's id: file_..., msgbatch_...
    id                text PRIMARY KEY,
    provider_id       text NOT NULL,
    -- 'file' or 'batch'.
    kind              text NOT NULL,
    organisation_id   text NOT NULL,
    workspace_id      text NOT NULL,
    key_id            text NOT NULL,
    -- Whose credential created it: 'shared', 'organisation' or 'workspace'.
    credential_scope  text NOT NULL,
    -- Files: 'active' or 'deleted'. Batches: the provider's processing status, then 'settled'
    -- once the gateway has accounted its results, and 'deleted'.
    status            text NOT NULL,
    -- The provider's object as last seen.
    object            jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- Batches: what the gateway decided per item (model, template, price), by custom id.
    detail            jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- Batches: the accounted cost of the items, in the reporting currency.
    cost              double precision,
    currency          text NOT NULL DEFAULT '',
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    -- Batches: when the provider was last asked about it, and when it was settled.
    polled_at         timestamptz,
    settled_at        timestamptz
);

CREATE INDEX provider_objects_by_workspace ON provider_objects (workspace_id, kind, created_at DESC, id DESC);
CREATE INDEX provider_objects_open_batches ON provider_objects (polled_at)
    WHERE kind = 'batch' AND status NOT IN ('settled', 'deleted');
