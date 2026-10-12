-- pacod-id took over the gateway's people (identity concept 17.1): once `pacod-gw-core identity
-- finalize` deleted the gateway's own users, admin tokens, role bindings and invitations, the
-- core no longer starts with PGW_AUTH=local.
CREATE TABLE identity_state (
    only_row     boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    finalized_at timestamptz NOT NULL DEFAULT now(),
    -- pacod-id's record of the import the documents went to.
    import       jsonb       NOT NULL DEFAULT '{}'
);
