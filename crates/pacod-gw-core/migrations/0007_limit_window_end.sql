-- When each counter's window ends, so that every instance can keep a copy of every counter of
-- a running window (limits/sync.rs). Without it, a gateway that had not counted a subject yet
-- admitted that subject's first call before it had read the stored total: with N gateways a
-- limit of one call a day allowed N.
--
-- Unix seconds; the largest bigint for lifetime limits. Nullable: rows written before this
-- migration, or by instances of the previous version during a rolling upgrade, have none and
-- are read on first use as before.

ALTER TABLE limit_counters ADD COLUMN window_end bigint;
CREATE INDEX limit_counters_window_end ON limit_counters (window_end);
