-- Consumption counted against limits (concept section 12.4).
--
-- Each core instance keeps its counters in memory and adds its increments here about once a
-- second, then reads the combined totals back. `used` is in whole units: tokens, requests, or
-- millionths of the reporting currency for spend limits.
CREATE TABLE limit_counters (
    limit_id      text        NOT NULL,
    -- The organisation, workspace, key or end user the counter belongs to.
    subject       text        NOT NULL,
    -- Start of the window as unix seconds; 0 for lifetime limits.
    window_start  bigint      NOT NULL,
    used          bigint      NOT NULL DEFAULT 0,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (limit_id, subject, window_start)
);
CREATE INDEX limit_counters_updated ON limit_counters (updated_at);
