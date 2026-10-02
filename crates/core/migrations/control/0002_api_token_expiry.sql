-- control.sqlite schema v2: API tokens can expire.
-- Spec: docs/web-port/IMPLEMENTATION-PLAN.md §2.11 gives the migration CLI's `migrate`
-- token a 7-day TTL, which the §2.6 `api_tokens` table cannot express.
--
-- NULL means the token never expires (extension and Shortcut tokens, until revoked). The
-- token lookup refuses a token whose `expires_at` has passed.

ALTER TABLE api_tokens ADD COLUMN expires_at INTEGER;
