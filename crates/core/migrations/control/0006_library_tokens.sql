-- F21: library API clients have their own token kind. Existing extension,
-- Shortcut and migrate tokens retain their scopes, expiry and revocation.
-- SQLite cannot widen a CHECK constraint without rebuilding its table.
CREATE TABLE api_tokens_library (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('extension','shortcut','migrate','library')),
  token_hash BLOB NOT NULL UNIQUE,
  label TEXT,
  scopes TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER,
  revoked_at INTEGER,
  expires_at INTEGER,
  install_hash BLOB
);
INSERT INTO api_tokens_library
  (id, user_id, kind, token_hash, label, scopes, created_at, last_used_at,
   revoked_at, expires_at, install_hash)
SELECT id, user_id, kind, token_hash, label, scopes, created_at, last_used_at,
       revoked_at, expires_at, install_hash FROM api_tokens;
DROP TABLE api_tokens;
ALTER TABLE api_tokens_library RENAME TO api_tokens;
CREATE INDEX api_tokens_install ON api_tokens(user_id, install_hash) WHERE install_hash IS NOT NULL;
