-- Configuration store of the gateway core.
--
-- Configuration objects (organisations, workspaces, keys, providers, models, templates, ...)
-- are typed JSON documents in one table. The Rust types in `src/model` are the schema; the
-- database guards identity, uniqueness of names within a parent, and the order of changes.

-- One row. Every change to the configuration increments `version` inside its transaction.
-- Taking this row's lock first serialises all configuration changes, which is what lets
-- other instances detect changes by polling the version without ever missing one.
CREATE TABLE config_head (
    id       boolean PRIMARY KEY DEFAULT true CHECK (id),
    version  bigint  NOT NULL
);
INSERT INTO config_head (id, version) VALUES (true, 0);

CREATE TABLE config_objects (
    id          text PRIMARY KEY,
    kind        text        NOT NULL,
    -- The owning object: the organisation of a workspace, the workspace of a key, and so on.
    parent      text,
    -- Unique within (kind, parent) when set.
    name        text,
    doc         jsonb       NOT NULL,
    -- Incremented on every update; used for optimistic concurrency.
    version     integer     NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX config_objects_kind_parent ON config_objects (kind, parent, id);
CREATE UNIQUE INDEX config_objects_unique_name
    ON config_objects (kind, COALESCE(parent, ''), name) WHERE name IS NOT NULL;

-- One row per configuration version: which object changed and how.
CREATE TABLE config_changes (
    version    bigint PRIMARY KEY,
    kind       text        NOT NULL,
    object_id  text        NOT NULL,
    op         text        NOT NULL CHECK (op IN ('create', 'update', 'delete')),
    at         timestamptz NOT NULL DEFAULT now()
);

-- What administrators did. Written in the same transaction as the change it describes.
CREATE TABLE audit_log (
    id              bigserial PRIMARY KEY,
    at              timestamptz NOT NULL DEFAULT now(),
    actor           text        NOT NULL,
    actor_kind      text        NOT NULL,
    action          text        NOT NULL,
    kind            text        NOT NULL,
    object_id       text        NOT NULL,
    organisation_id text,
    workspace_id    text,
    -- SHA-256 of the object's document before and after the change.
    before_hash     text,
    after_hash      text,
    config_version  bigint,
    detail          jsonb       NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_log_at ON audit_log (at DESC, id DESC);
CREATE INDEX audit_log_object ON audit_log (kind, object_id);
CREATE INDEX audit_log_scope ON audit_log (organisation_id, workspace_id, at DESC);

-- Console login sessions. Only the SHA-256 of the token is stored.
CREATE TABLE sessions (
    token_hash  bytea PRIMARY KEY,
    user_id     text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);
CREATE INDEX sessions_expires ON sessions (expires_at);

-- Running core instances and the configuration version each has applied.
CREATE TABLE instances (
    id               text PRIMARY KEY,
    role             text        NOT NULL,
    hostname         text        NOT NULL,
    software_version text        NOT NULL,
    applied_version  bigint      NOT NULL,
    started_at       timestamptz NOT NULL,
    heartbeat_at     timestamptz NOT NULL
);
