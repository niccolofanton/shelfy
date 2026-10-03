-- control.sqlite schema v4: which browser installation holds an extension token (P2-03).
-- Spec: docs/web-port/IMPLEMENTATION-PLAN.md §2.6, §2.11 (device tokens); phases/P2.md C2 and
-- gap P2-G16: pairing the same installation again revokes the token it held before.
--
-- api_tokens.install_hash: SHA-256 (domain-separated) of the `installId` the extension sends to
--   `POST /extension/pair`, a random id it keeps for as long as it is installed. NULL for every
--   token that pairing did not mint (Shortcut, migrate, extension tokens minted from the account).
--   Hashed because it identifies a browser profile and the server never needs it back.
ALTER TABLE api_tokens ADD COLUMN install_hash BLOB;

-- Re-pairing looks up a user's working tokens of one installation.
CREATE INDEX api_tokens_install ON api_tokens(user_id, install_hash) WHERE install_hash IS NOT NULL;
