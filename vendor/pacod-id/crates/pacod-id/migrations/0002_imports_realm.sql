-- Imports record the realm the product's people went to. In "report", next to the counts, are
-- the ids an import brought ("brought"): the next import retires what the product removed
-- since, and the rollback check counts writes about these people.
--
-- 0001 is never edited once applied: a running pacod-id refuses to start when an applied
-- migration changed. Imports before this one went to the default realm of `import gw`.
ALTER TABLE imports ADD COLUMN realm text;
UPDATE imports SET realm = 'gateway';
ALTER TABLE imports ALTER COLUMN realm SET NOT NULL;
