-- Rollups (concept section 14.4): totals per hour, and per day and end user, rebuilt from the
-- deduplicated events by the analytics service's rollup job. Reports read them for the hours
-- and days that were built and are not marked late, and the events for the rest, so that an
-- answer is the same with or without them.
--
-- A rebuild writes a bucket's rows anew under a higher generation, then deletes the bucket's
-- rows of older generations. ReplacingMergeTree keeps the newest generation of a row under
-- FINAL in the meantime, so a reader never counts a bucket twice.

CREATE TABLE IF NOT EXISTS usage_hourly
(
    bucket              DateTime('UTC'),
    organisation_id     LowCardinality(String),
    workspace_id        LowCardinality(String),
    key_id              String,
    provider            LowCardinality(String),
    model               LowCardinality(String),
    template            LowCardinality(String),
    status              LowCardinality(String),
    requests            UInt64,
    errors              UInt64,
    input_tokens        UInt64,
    output_tokens       UInt64,
    cache_read_tokens   UInt64,
    cache_write_tokens  UInt64,
    reasoning_tokens    UInt64,
    -- In the reporting currency, as the reports count it.
    cost                Float64,
    generation          UInt64
)
ENGINE = ReplacingMergeTree(generation)
PARTITION BY toYYYYMM(bucket)
ORDER BY (bucket, organisation_id, workspace_id, key_id, provider, model, template, status);

CREATE TABLE IF NOT EXISTS usage_daily_end_users
(
    bucket              DateTime('UTC'),
    organisation_id     LowCardinality(String),
    workspace_id        LowCardinality(String),
    end_user            String,
    requests            UInt64,
    errors              UInt64,
    input_tokens        UInt64,
    output_tokens       UInt64,
    cache_read_tokens   UInt64,
    cache_write_tokens  UInt64,
    reasoning_tokens    UInt64,
    cost                Float64,
    first_at            DateTime64(9, 'UTC'),
    last_at             DateTime64(9, 'UTC'),
    generation          UInt64
)
ENGINE = ReplacingMergeTree(generation)
PARTITION BY toYYYYMM(bucket)
ORDER BY (bucket, organisation_id, workspace_id, end_user);

-- The buckets that were built: 'hour' for usage_hourly, 'day' for usage_daily_end_users.
CREATE TABLE IF NOT EXISTS rollup_built
(
    granularity  LowCardinality(String),
    bucket       DateTime('UTC'),
    generation   UInt64
)
ENGINE = ReplacingMergeTree(generation)
ORDER BY (granularity, bucket);

-- Hours that received events after they had closed, for example from an instance that shipped
-- its spool after an outage. Ingest marks them; the job rebuilds them (and their day) and
-- clears the marks it has seen. Until then, readers take their events instead.
CREATE TABLE IF NOT EXISTS rollup_late
(
    bucket     DateTime('UTC'),
    marked_at  DateTime64(3, 'UTC')
)
ENGINE = MergeTree
ORDER BY bucket;
