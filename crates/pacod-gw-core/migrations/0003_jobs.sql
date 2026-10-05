-- Batch jobs that the analytics service runs under a lease from the core (concept section
-- 13.3): the price tracker first. The lease makes sure one instance runs a job at a time.

CREATE TABLE jobs (
    name             text PRIMARY KEY,
    -- Who holds the lease, and until when. Empty and NULL when nobody does.
    holder           text NOT NULL DEFAULT '',
    lease_until      timestamptz,
    -- When the job last started, for the schedule.
    last_started_at  timestamptz,
    -- Set by "run now"; cleared when a run starts.
    run_requested    boolean NOT NULL DEFAULT false,
    -- The run in progress, if any.
    current_run      text
);

CREATE TABLE job_runs (
    id           text PRIMARY KEY,
    job          text NOT NULL,
    -- 'schedule', 'manual' or 'import'.
    trigger      text NOT NULL,
    started_at   timestamptz NOT NULL,
    finished_at  timestamptz,
    ok           boolean,
    message      text NOT NULL DEFAULT '',
    -- Per-model results and counts.
    result       jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX job_runs_by_job ON job_runs (job, started_at DESC);
