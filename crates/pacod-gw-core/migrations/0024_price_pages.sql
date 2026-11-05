-- The price tracker reads pages that administrators confirmed (`config_objects` of kind
-- `price_page`). What the daily check finds out about a page lives here rather than in the
-- page's document: a check every day would otherwise bump the configuration version, write an
-- audit entry and rebuild every instance's snapshot, while nothing an administrator decided
-- has changed.
--
-- Additive: an older instance neither reads nor writes this table.

CREATE TABLE IF NOT EXISTS price_page_state (
    -- Deleting a page deletes what was known about it.
    page_id              text PRIMARY KEY REFERENCES config_objects (id) ON DELETE CASCADE,
    provider_id          text NOT NULL,
    last_checked_at      timestamptz,
    -- When the page's content last differed from the reading before.
    last_changed_at      timestamptz,
    -- When the extraction model last read the page.
    last_read_at         timestamptz,
    -- Over the lines that carry models, prices and limits; a change here makes the page read.
    content_hash         text NOT NULL DEFAULT '',
    -- Over the whole text, for diagnosis.
    text_hash            text NOT NULL DEFAULT '',
    http_status          integer,
    -- Where the page ended up after redirects.
    final_url            text NOT NULL DEFAULT '',
    -- `read`, `unchanged`, `not_read` or `failed`; no CHECK, so that a later release may add one.
    outcome              text NOT NULL DEFAULT '',
    message              text NOT NULL DEFAULT '',
    consecutive_failures integer NOT NULL DEFAULT 0,
    -- Set by the scan: `gone` (the page answers 404 or 410) or `moved` (it redirects elsewhere).
    flag                 text,
    moved_to             text,
    -- An administrator asked for this page to be checked, and read even when unchanged.
    check_requested      boolean NOT NULL DEFAULT false,
    force_read           boolean NOT NULL DEFAULT false,
    -- What the last reading found, and the reading before: properties are applied only where
    -- the page itself changed, so that an administrator's edit is not undone every day.
    extraction           jsonb NOT NULL DEFAULT '{}'::jsonb,
    previous_extraction  jsonb NOT NULL DEFAULT '{}'::jsonb,
    updated_at           timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS price_page_state_provider ON price_page_state (provider_id);

-- When the scraper last asked for work, and what it said about itself and its browser: a poll
-- that is not granted a lease writes nothing else, and the console must be able to tell that
-- the scraper is not running.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS last_polled_at timestamptz;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS poll_status jsonb;
