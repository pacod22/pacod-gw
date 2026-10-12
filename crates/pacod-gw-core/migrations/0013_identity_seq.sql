-- pacod-id (identity concept 7.4): how far each management instance's copy of the role
-- bindings has followed pacod-id's feed, so that a member or token change can answer "applied
-- on n of m instances" as configuration changes do. NULL in local mode and on gateway-only
-- instances, which never consult identity.
ALTER TABLE instances ADD COLUMN authz_seq bigint;
