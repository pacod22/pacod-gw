-- Several core instances, possibly on several machines (runbook section 9).
--
-- boot_id      the process that owns the row; new at every start. A heartbeat only takes
--              over a row of another process when that process has stopped heart-beating,
--              so two live processes with the same instance id are noticed.
-- home_id      random id kept in the instance's spool directory: the same directory is the
--              same instance, so a restart after a crash claims its row at once.
-- live         the instance's live figures (cluster/report.rs), rewritten with the heartbeat.
--              The central console reads them instead of calling into the instance.
-- conflict_at  the last time another live process presented this instance id.
--
-- All columns are nullable: instances of the previous version keep working while a rolling
-- upgrade is under way.

ALTER TABLE instances
    ADD COLUMN boot_id     text,
    ADD COLUMN home_id     text,
    ADD COLUMN live        jsonb,
    ADD COLUMN live_at     timestamptz,
    ADD COLUMN conflict_at timestamptz;
