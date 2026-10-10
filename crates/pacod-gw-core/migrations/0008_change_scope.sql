-- The organisation and workspace each configuration change belongs to, so that the live feed
-- (`GET /events`) tells each user only about what they may read. They are written with the
-- change: a deleted object can no longer be looked up to find its scope.
--
-- Nullable: global objects (providers, models, users) have neither, and rows written before
-- this migration, or by an instance of the previous version during a rolling upgrade, have
-- none and are shown to platform roles only.

ALTER TABLE config_changes ADD COLUMN organisation_id text, ADD COLUMN workspace_id text;
