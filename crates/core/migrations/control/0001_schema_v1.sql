-- control.sqlite schema v1: users, auth, jobs and other cross-user state.
-- Spec: docs/web-port/IMPLEMENTATION-PLAN.md §2.6.
--
-- Migrations are append-only: once this file ships, a schema change is a new
-- numbered file. crates/core/tests/fixtures/schema/control-v1.sql freezes this
-- version and the schema tests fail if the two drift apart.
--
-- Secrets are 256-bit random values stored as their SHA-256 only. Timestamps are
-- unix milliseconds.

-- "SHLC": marks the file as a Shelfy control database.
PRAGMA application_id = 1397247043;

CREATE TABLE users (
  id TEXT PRIMARY KEY,                               -- ULID
  email TEXT NOT NULL UNIQUE COLLATE NOCASE,
  display_name TEXT,
  role TEXT NOT NULL CHECK (role IN ('owner','member')),
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled','deleting')),
  quota_bytes INTEGER NOT NULL,                      -- media + DB; owner: 0 = unlimited
  capture_daily_limit INTEGER NOT NULL DEFAULT 20,
  usage_bytes INTEGER NOT NULL DEFAULT 0, usage_updated_at INTEGER,
  disclaimer_version TEXT, disclaimer_accepted_at INTEGER,
  created_at INTEGER NOT NULL, last_seen_at INTEGER
);
CREATE TABLE invites (token_hash BLOB PRIMARY KEY, email TEXT, role TEXT NOT NULL DEFAULT 'member',
  created_by TEXT NOT NULL REFERENCES users(id), created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL, used_at INTEGER, used_by TEXT);
CREATE TABLE passkeys (id INTEGER PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  cred_id BLOB NOT NULL UNIQUE, passkey_json TEXT NOT NULL, label TEXT,
  created_at INTEGER NOT NULL, last_used_at INTEGER);
CREATE TABLE sessions (id_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL,
  reauth_at INTEGER, user_agent TEXT);
CREATE TABLE magic_links (token_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  purpose TEXT NOT NULL CHECK (purpose IN ('login','verify','reauth')),
  expires_at INTEGER NOT NULL, used_at INTEGER);
CREATE TABLE api_tokens (id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('extension','shortcut','migrate')),
  token_hash BLOB NOT NULL UNIQUE, label TEXT, scopes TEXT NOT NULL,
  created_at INTEGER NOT NULL, last_used_at INTEGER, revoked_at INTEGER);
CREATE TABLE pairing_codes (code_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER);
CREATE TABLE provider_keys (user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider_id TEXT NOT NULL, key_version INTEGER NOT NULL, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL,
  last4 TEXT NOT NULL, created_at INTEGER NOT NULL, last_used_at INTEGER,
  PRIMARY KEY (user_id, provider_id));
CREATE TABLE jobs (
  id INTEGER PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL, dedupe_key TEXT,
  state TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed','cancelled')),
  priority INTEGER NOT NULL DEFAULT 100,
  payload_json TEXT NOT NULL DEFAULT '{}',
  attempts INTEGER NOT NULL DEFAULT 0, max_attempts INTEGER NOT NULL,
  run_at INTEGER NOT NULL, lease_until INTEGER,
  progress REAL, stage TEXT, error_code TEXT, error_detail TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, finished_at INTEGER
);
CREATE UNIQUE INDEX jobs_active_dedupe ON jobs(user_id, kind, dedupe_key)
  WHERE state IN ('queued','running') AND dedupe_key IS NOT NULL;
CREATE INDEX jobs_ready ON jobs(kind, state, run_at);
CREATE INDEX jobs_user ON jobs(user_id, state, kind);
CREATE TABLE queue_state (user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE, kind TEXT NOT NULL,
  paused INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (user_id, kind));
CREATE TABLE uploads (id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  purpose TEXT NOT NULL, length INTEGER NOT NULL, upload_offset INTEGER NOT NULL DEFAULT 0, meta_json TEXT,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, completed_at INTEGER);
CREATE TABLE idempotency (user_id TEXT NOT NULL, key TEXT NOT NULL, status INTEGER NOT NULL, body BLOB,
  created_at INTEGER NOT NULL, PRIMARY KEY (user_id, key));          -- pruned after 24 h
CREATE TABLE usage_daily (user_id TEXT NOT NULL, day TEXT NOT NULL,
  ai_calls INTEGER NOT NULL DEFAULT 0, ai_in_tokens INTEGER NOT NULL DEFAULT 0, ai_out_tokens INTEGER NOT NULL DEFAULT 0,
  captures INTEGER NOT NULL DEFAULT 0, ingest_items INTEGER NOT NULL DEFAULT 0, bytes_in INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (user_id, day));
CREATE TABLE audit_log (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, actor_user_id TEXT, action TEXT NOT NULL,
  target TEXT, ip_hash BLOB, meta_json TEXT);
CREATE TABLE feature_flags (key TEXT PRIMARY KEY, value_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
