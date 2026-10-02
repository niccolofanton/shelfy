-- control.sqlite schema v3: the account (P1-17), and passkey ids that are never reused (F5).
-- Spec: docs/web-port/IMPLEMENTATION-PLAN.md §2.6, §2.11, §2.13, §7.2.
--
-- users:
--   privacy_version, privacy_accepted_at: the privacy notice the user accepted, and when.
--     `POST /me/consent` stores it beside the disclaimer's version and time, which v1 has.
--   usage_media_bytes, usage_db_bytes: the two parts of usage_bytes (media + DB file), as the
--     `usage.recompute` job last counted them.
ALTER TABLE users ADD COLUMN privacy_version TEXT;
ALTER TABLE users ADD COLUMN privacy_accepted_at INTEGER;
ALTER TABLE users ADD COLUMN usage_media_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN usage_db_bytes INTEGER NOT NULL DEFAULT 0;

-- passkeys: AUTOINCREMENT, so a deleted passkey's id never names a new one; the audit log names
-- passkeys by id. SQLite cannot add it to an existing column, so the table is rebuilt with the
-- same columns and rows. The sequence starts above every id the table or the audit log has
-- used, so the ids freed before this migration are not reused either.
ALTER TABLE passkeys RENAME TO passkeys_v2;
CREATE TABLE passkeys (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  cred_id BLOB NOT NULL UNIQUE, passkey_json TEXT NOT NULL, label TEXT,
  created_at INTEGER NOT NULL, last_used_at INTEGER);
INSERT INTO passkeys (id, user_id, cred_id, passkey_json, label, created_at, last_used_at)
  SELECT id, user_id, cred_id, passkey_json, label, created_at, last_used_at FROM passkeys_v2;
DROP TABLE passkeys_v2;
INSERT INTO sqlite_sequence (name, seq) SELECT 'passkeys', 0
  WHERE NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'passkeys');
UPDATE sqlite_sequence SET seq = max(seq, (
    SELECT coalesce(max(CAST(json_extract(meta_json, '$.id') AS INTEGER)), 0) FROM audit_log
    WHERE action LIKE 'passkey.%' AND json_valid(meta_json)))
  WHERE name = 'passkeys';
