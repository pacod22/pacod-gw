-- Benchmark reports (pacod-benchmark; the document is crates/pacod-gw-bench-report): what a
-- benchmark published or a platform admin uploaded, for the console's Benchmarks page. They are
-- not configuration: a report changes no configuration version, and no instance reloads for it.
-- The document is kept as sent, byte for byte; the columns are what the list shows and sorts on.
-- Additive: an older instance ignores the table.
CREATE TABLE IF NOT EXISTS benchmark_reports (
    -- bmr_<uuid>, minted by the gateway; time-ordered.
    id                text PRIMARY KEY,
    name              text NOT NULL,
    schema            text NOT NULL,
    schema_minor      integer NOT NULL DEFAULT 0,
    -- The run's id at the benchmark that made it.
    report_id         text NOT NULL,
    -- When the benchmark made the report, and when the gateway took it.
    created_at        timestamptz NOT NULL,
    imported_at       timestamptz NOT NULL DEFAULT now(),
    -- push: a benchmark published it; upload: a platform admin uploaded the file.
    source            text NOT NULL CHECK (source IN ('push', 'upload')),
    -- Who gave it to the gateway (the caller), and who the report says ran it.
    published_by      text NOT NULL,
    published_by_name text NOT NULL DEFAULT '',
    run_by            text NOT NULL DEFAULT '',
    target_label      text NOT NULL,
    target_kind       text NOT NULL,
    product           text NOT NULL,
    gateway_version   text NOT NULL DEFAULT '',
    layout            text NOT NULL DEFAULT '',
    measurement_class text NOT NULL,
    cells             integer NOT NULL,
    size_bytes        integer NOT NULL CHECK (size_bytes <= 1048576),
    -- SHA-256 of the document as sent: the same report given twice is kept once.
    digest            text NOT NULL UNIQUE,
    document          bytea NOT NULL
);
CREATE INDEX IF NOT EXISTS benchmark_reports_by_imported ON benchmark_reports (imported_at DESC);
