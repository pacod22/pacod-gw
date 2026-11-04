-- Model discovery also takes proposals from pacod-broker (runbook section 20): models the broker
-- lists at an active provider of the repository's, and repository models that are broker
-- resources but not yet linked. `source` says who found a proposal; a model both the provider
-- and the broker list is one proposal, the provider's, with the broker's resource id in its
-- details. Each source resolves only its own proposals that a later run no longer finds.
--
-- Additive: an older instance ignores the column, keeps the unique key it knows, and its runs
-- resolve every open proposal of a provider; the next run of a newer instance opens the
-- broker's again.
ALTER TABLE discovery_proposals
    ADD COLUMN IF NOT EXISTS source text NOT NULL DEFAULT 'provider' CHECK (source IN ('provider', 'broker'));
