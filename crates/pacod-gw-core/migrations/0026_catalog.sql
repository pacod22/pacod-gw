-- The model-data sync reads catalogues (pacod-aggregator, OpenRouter, LiteLLM, the models file).
-- What each source last said about each model is kept here, outside the configuration, for
-- three reasons: a property applies only when the source's own value changed since its last
-- reading, so that an administrator's edit is not undone every day; a value from the models
-- file applies only when the file changed, so that a restart never undoes what the daily sync
-- applied since; and a draft is priced from what the sources said, at once, without fetching.
-- Rows are kept for entries no model matches as well, for the drafts that will.
--
-- Additive: an older instance neither reads nor writes this table.

CREATE TABLE IF NOT EXISTS catalog_seen (
    -- `pacod_aggregator`, `openrouter`, `litellm`, `file` or `upload`; no CHECK, so that a later
    -- release may add one.
    source   text NOT NULL,
    vendor   text NOT NULL,
    -- As the catalogue writes it, normalised (lower case).
    api_id   text NOT NULL,
    -- The catalogue's entry (`pacod.catalog/v1`), as it was read.
    entry    jsonb NOT NULL,
    -- The gateway's model it matched, when one did.
    model_id text,
    seen_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source, vendor, api_id)
);

-- Model discovery also takes proposals from the sync: models pacod-aggregator's catalogue or
-- the models file lists that the repository lacks (`source = 'catalog'` and `'file'`). As with
-- the price tracker's, the provider's own list stays the authority and a source resolves only
-- its own proposals.
--
-- Additive: an instance of the release before resolves only its own sources' rows.
ALTER TABLE discovery_proposals DROP CONSTRAINT IF EXISTS discovery_proposals_source_check;
ALTER TABLE discovery_proposals ADD CONSTRAINT discovery_proposals_source_check
    CHECK (source IN ('provider', 'broker', 'scraper', 'catalog', 'file'));
