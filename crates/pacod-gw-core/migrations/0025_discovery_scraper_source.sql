-- Model discovery also takes proposals from the price tracker: models a provider's pricing or
-- model page lists that the repository does not have (`source = 'scraper'`). The provider's own
-- list stays the authority: a model both list is one proposal, the provider's, and the tracker
-- never takes over a proposal of another source. Each source resolves only its own proposals.
--
-- Additive: an instance of the release before resolves only `provider` rows and leaves these
-- alone; one from before 0023 resolves every open proposal of a provider, and the next check of
-- a newer instance opens the tracker's again.
ALTER TABLE discovery_proposals DROP CONSTRAINT IF EXISTS discovery_proposals_source_check;
ALTER TABLE discovery_proposals ADD CONSTRAINT discovery_proposals_source_check
    CHECK (source IN ('provider', 'broker', 'scraper'));
