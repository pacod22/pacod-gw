-- Every version of every template (concept section 7.3). A template's document keeps its most
-- recent versions, which the request path needs; this table keeps all of them, and older ones
-- are read from here. The core writes a version here in the same transaction as the template.

CREATE TABLE template_versions (
    template_id  text        NOT NULL,
    version      integer     NOT NULL,
    spec         jsonb       NOT NULL,
    comment      text        NOT NULL DEFAULT '',
    created_by   text        NOT NULL DEFAULT '',
    created_at   timestamptz NOT NULL,
    PRIMARY KEY (template_id, version)
);

-- The versions the documents still hold.
INSERT INTO template_versions (template_id, version, spec, comment, created_by, created_at)
SELECT o.id, (v->>'version')::integer, v->'spec', coalesce(v->>'comment', ''), coalesce(v->>'created_by', ''),
       (v->>'created_at')::timestamptz
FROM config_objects o, jsonb_array_elements(o.doc->'versions') v
WHERE o.kind = 'template'
ON CONFLICT DO NOTHING;
