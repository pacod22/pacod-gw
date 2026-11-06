-- The models a source of model data has been applied to. catalog_seen says what a source last said
-- about a vendor's model id, and a property applies only when that changed. A model added since
-- was not there to hear it: without this table it would never take the properties the source
-- states. One row per model, not per source: a model that has heard one source keeps the rule, so
-- that reading an unchanged models file at a restart cannot undo what the daily sync applied since.
--
-- Additive: an older instance neither reads nor writes this table. A newer one takes a model it has
-- no row for as new, which costs no more than reading once what the sources state.

CREATE TABLE IF NOT EXISTS catalog_read (
    model_id text PRIMARY KEY,
    -- The source that read it first.
    source   text NOT NULL,
    read_at  timestamptz NOT NULL DEFAULT now()
);
