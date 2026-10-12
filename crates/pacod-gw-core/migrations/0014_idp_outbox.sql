-- pacod-id (identity concept 13.1): the organisations and workspaces the gateway registers
-- there, as scopes for role bindings and invitations. A change is written here in the same
-- transaction as the organisation or workspace, and delivered in order (one deliverer at a
-- time, under an advisory lock). A row pacod-id refuses for good is parked, not retried.
CREATE TABLE idp_outbox (
    id              bigserial   PRIMARY KEY,
    -- put_organisation, delete_organisation, put_scope, delete_scope
    op              text        NOT NULL,
    -- organisation:<id> or gw.workspace:<id>
    target          text        NOT NULL,
    payload         jsonb       NOT NULL DEFAULT '{}'::jsonb,
    created_at      timestamptz NOT NULL DEFAULT now(),
    attempts        integer     NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    last_error      text        NOT NULL DEFAULT '',
    -- Set when pacod-id refused the row for good; such rows are skipped and reported.
    parked_at       timestamptz
);
CREATE INDEX idp_outbox_due ON idp_outbox (id) WHERE parked_at IS NULL;
