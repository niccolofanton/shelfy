-- Re-creatable bundles; deliberately outside the user's storage quota.
CREATE TABLE exports (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  job_id INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  estimated_bytes INTEGER NOT NULL CHECK (estimated_bytes >= 0),
  bytes INTEGER CHECK (bytes >= 0),
  deleted_at INTEGER
);
CREATE UNIQUE INDEX exports_live_user ON exports(user_id) WHERE deleted_at IS NULL;
CREATE INDEX exports_expiry ON exports(expires_at);
