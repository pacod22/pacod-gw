-- How far back the rollup job has looked for hours that were never built. It walks older
-- events once, a week at a time, from the newest down to the oldest; afterwards it looks at
-- the last two days only, so that it never scans all events on every run.
CREATE TABLE IF NOT EXISTS rollup_progress
(
    name     LowCardinality(String),
    value    DateTime('UTC'),
    updated  DateTime64(3, 'UTC')
)
ENGINE = ReplacingMergeTree(updated)
ORDER BY name;
