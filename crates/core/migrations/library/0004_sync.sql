-- library.sqlite schema v4: the sync-run and sync-source columns the browser
-- extension's sync needs (P2-09; plan §2.16, §2.9, §2.7; P2 contracts C4, C8).
--
-- Schema v1 shipped placeholder `sync_runs` and `sync_sources` tables. A run now
-- records its trigger, the listing it walked (kind, external id, name), the
-- collection it mapped into, the pages it scanned, why it stopped, the cursor a
-- page cap left behind, whether it was incremental and the stop-after-known
-- threshold it used, plus `updated_at` so an idle run can be stopped. A source
-- records the listing it stands for, when its last full walk reached the end of
-- the feed (which makes the next run incremental, P2-G1) and the resume cursor a
-- capped walk stored (P2-G2).
--
-- Additive (§3.8 expand/contract): an older build ignores the new columns, and
-- its plain writes still work. The listing kinds, triggers, states and stop
-- reasons are validated in crates/core/src/repo/sync.rs, not by a CHECK, so the
-- v1 fixture's placeholder rows stay valid.

ALTER TABLE sync_runs ADD COLUMN trigger TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE sync_runs ADD COLUMN listing_external_id TEXT;
ALTER TABLE sync_runs ADD COLUMN listing_name TEXT;
ALTER TABLE sync_runs ADD COLUMN collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL;
ALTER TABLE sync_runs ADD COLUMN pages INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sync_runs ADD COLUMN stop_reason TEXT;
ALTER TABLE sync_runs ADD COLUMN resume_cursor TEXT;
ALTER TABLE sync_runs ADD COLUMN incremental INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sync_runs ADD COLUMN stop_after_known INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sync_runs ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;

ALTER TABLE sync_sources ADD COLUMN source_kind TEXT;
ALTER TABLE sync_sources ADD COLUMN external_id TEXT;
ALTER TABLE sync_sources ADD COLUMN source_name TEXT;
ALTER TABLE sync_sources ADD COLUMN last_full_at INTEGER;
ALTER TABLE sync_sources ADD COLUMN resume_cursor TEXT;

-- GET /sync-runs lists a user's runs newest first, paged by a (started_at, id)
-- cursor; platform and state are filters over the small per-user table.
CREATE INDEX sync_runs_recent ON sync_runs(started_at DESC, id DESC);
