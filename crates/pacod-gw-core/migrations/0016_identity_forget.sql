-- Forgetting an account pacod-id deleted (identity concept 6.9) finds the records of what it did
-- and the records about it by these columns.
CREATE INDEX audit_log_actor ON audit_log (actor);
CREATE INDEX audit_log_object_id ON audit_log (object_id);
CREATE INDEX template_versions_created_by ON template_versions (created_by);
