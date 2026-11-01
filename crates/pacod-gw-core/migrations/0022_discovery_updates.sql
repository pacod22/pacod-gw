-- Model discovery also proposes updates (concept section 6.4): a repository model whose
-- provider now reports other facts (limits, capabilities), or that lacks a capability the
-- preset suggested for it sets. `details` holds what differs; nothing changes until an admin
-- applies the proposal. An older instance lists such proposals but neither makes nor applies
-- them, and refuses to accept them as new models; its runs mark them resolved, and the next run
-- of a newer instance opens them again.

ALTER TABLE discovery_proposals DROP CONSTRAINT discovery_proposals_kind_check;
ALTER TABLE discovery_proposals ADD CONSTRAINT discovery_proposals_kind_check
    CHECK (kind IN ('new', 'gone', 'update'));
