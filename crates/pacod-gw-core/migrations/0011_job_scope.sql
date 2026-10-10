-- A run of a job can be limited to some models: activating a model asks the price tracker for
-- that model only, without fetching every source for every model.

-- The models a pending request names; NULL asks for a full run. Several requests for single
-- models add up; a request for a full run replaces them.
ALTER TABLE jobs ADD COLUMN run_models text[];

-- The models a run covers; NULL for all. Kept with the run, so that a run handed out again
-- after a restart of the analytics service covers the same models.
ALTER TABLE job_runs ADD COLUMN models text[];
