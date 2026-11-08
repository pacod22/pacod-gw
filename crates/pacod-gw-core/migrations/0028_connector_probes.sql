-- What probing a tool server saw (catalogue MCP-10): when it was probed last and how that went,
-- and each tool it listed with the part of its definition an approval covers. Kept outside the
-- configuration: a probe that writes configuration would raise the configuration version on
-- every instance every few hours, for nothing a caller is served. Only an approval writes
-- configuration (`tool_pin`), and a re-probe does where a pinned definition changed.
--
-- `owner_id` is the tool server's id (later also a remote agent's). The rows go with it.
--
-- Additive: an older instance neither reads nor writes these tables.

CREATE TABLE IF NOT EXISTS connector_probe_runs (
    owner_id   text PRIMARY KEY,
    probed_at  timestamptz NOT NULL,
    -- The revision the server speaks; empty when the probe failed.
    version    text NOT NULL DEFAULT '',
    ok         boolean NOT NULL,
    -- Why it failed; empty when it did not.
    problem    text NOT NULL DEFAULT '',
    tools      integer NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS connector_probes (
    owner_id   text NOT NULL,
    -- The tool's own name on its server.
    item       text NOT NULL,
    definition jsonb NOT NULL,
    hash       text NOT NULL,
    first_seen timestamptz NOT NULL DEFAULT now(),
    -- A tool the latest successful probe did not list has an older `last_seen` than its run's
    -- `probed_at`: the server no longer offers it.
    last_seen  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_id, item)
);
