-- OpenAI's Files and Batches next to Anthropic's (concept section 8.3): which provider API an
-- object belongs to. Objects recorded before this migration are Anthropic's, the only API there
-- was. Additive: an older instance ignores the column and keeps writing Anthropic's objects,
-- which take the default.
ALTER TABLE provider_objects ADD COLUMN IF NOT EXISTS api text NOT NULL DEFAULT 'anthropic';
CREATE INDEX IF NOT EXISTS provider_objects_by_api ON provider_objects (workspace_id, api, kind, created_at DESC, id DESC);
