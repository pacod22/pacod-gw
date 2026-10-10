-- Model discovery (concept section 6.4): what a scheduled run found at each provider. A model
-- the provider lists and the repository lacks is a proposal for a draft; a repository model
-- the provider no longer lists is flagged. Nothing is created or activated by a run. Runs
-- themselves are kept in job_runs under the job name 'discovery'.

CREATE TABLE discovery_proposals (
    id           text PRIMARY KEY,
    provider_id  text        NOT NULL,
    upstream_id  text        NOT NULL,
    -- 'new': listed by the provider, not in the repository. 'gone': in the repository, no
    -- longer listed.
    kind         text        NOT NULL CHECK (kind IN ('new', 'gone')),
    -- For 'gone': the repository model.
    model_id     text,
    -- What the provider says about the model: display name, context window, output limit.
    details      jsonb       NOT NULL DEFAULT '{}'::jsonb,
    -- 'resolved' when a later run no longer finds the difference.
    status       text        NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'accepted', 'dismissed', 'resolved')),
    first_seen   timestamptz NOT NULL DEFAULT now(),
    last_seen    timestamptz NOT NULL DEFAULT now(),
    decided_by   text,
    decided_at   timestamptz,
    UNIQUE (provider_id, upstream_id, kind)
);

CREATE INDEX discovery_proposals_open ON discovery_proposals (status, provider_id);
