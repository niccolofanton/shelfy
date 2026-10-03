-- Job IDs outlive their rows in library checkpoints and taxonomy plans.
-- Keep allocation monotone when finished jobs are cleared or pruned.
CREATE TABLE job_id_counter (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  last_id INTEGER NOT NULL CHECK (typeof(last_id) = 'integer' AND last_id >= 0)
) WITHOUT ROWID;
INSERT INTO job_id_counter(singleton, last_id)
SELECT 1, max(coalesce((SELECT max(id) FROM jobs), 0),
              coalesce((SELECT max(job_id) FROM exports), 0), 0);
ALTER TABLE jobs ADD COLUMN incarnation TEXT NOT NULL DEFAULT ''
  CHECK (length(incarnation) IN (0, 36, 39));
-- Existing imports cannot safely adopt unbound legacy cursors. The prefix
-- distinguishes those rows from new admissions when a marker is unbound.
UPDATE jobs SET incarnation = 'legacy:' || lower(hex(randomblob(16)));
CREATE TRIGGER jobs_identity_highwater AFTER INSERT ON jobs BEGIN
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM job_id_counter WHERE singleton = 1)
    THEN RAISE(ABORT, 'job identifier counter is missing') END;
  SELECT CASE WHEN NEW.id <= (SELECT last_id FROM job_id_counter WHERE singleton = 1)
    THEN RAISE(ABORT, 'job identifiers cannot be reused') END;
  UPDATE job_id_counter SET last_id = NEW.id WHERE singleton = 1;
END;
-- An older writer ignores the counter and marker incarnation. Refuse its
-- next open rather than permit unsafe job execution after a rollback.
CREATE TABLE IF NOT EXISTS schema_compat (min_reader_version INTEGER NOT NULL);
DELETE FROM schema_compat;
INSERT INTO schema_compat(min_reader_version) VALUES (7);
