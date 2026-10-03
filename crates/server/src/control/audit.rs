//! `audit_log` (plan §2.11): every destructive or admin action leaves a row.
//! Actions are `<object>.<verb>`; `actor_user_id` is empty for the CLI.
//! Rows never carry secrets, and `meta_json` never carries user content.

use rusqlite::{Connection, params};
use shelfy_core::repo::Result;

/// `admin create-owner` created the owner.
pub const OWNER_CREATE: &str = "owner.create";
/// `admin create-user` created a member account (E6 test accounts, such as
/// the mock account). `meta`: `via` (always `"admin"`, the only way a member
/// is created while the instance has no invite redemption route).
pub const USER_CREATE: &str = "user.create";
/// An invite was created.
pub const INVITE_CREATE: &str = "invite.create";
/// A sign-in link was minted. `meta`: `via` (`email` or `cli`) and `purpose`
/// (`login` or `reauth`).
pub const MAGIC_LINK_CREATE: &str = "magic_link.create";
/// A user signed in: a session was created. `meta`: `method` (`magic_link`
/// or `passkey`) and `rotated`.
pub const SESSION_CREATE: &str = "session.create";
/// Sessions ended. `meta`: `scope` and `count`. `scope` is `current` (a
/// sign-out), `all` (a sign-out everywhere), `remote` (another session
/// signed out from the session list) or `others` (every other session).
pub const SESSION_DELETE: &str = "session.delete";
/// A signed-in user proved who they are again (re-authentication). `meta`:
/// `method` (`passkey` or `magic_link`).
pub const SESSION_REAUTH: &str = "session.reauth";
/// A passkey was registered. `meta`: `id` (the passkey's row id).
pub const PASSKEY_CREATE: &str = "passkey.create";
/// A passkey was removed. `meta`: `id`.
pub const PASSKEY_DELETE: &str = "passkey.delete";
/// A passkey signed with a counter that did not grow: the authenticator may
/// have been cloned, and the sign-in was refused. `meta`: `id`.
pub const PASSKEY_CLONE_SUSPECTED: &str = "passkey.clone_suspected";
/// An API token was minted. `meta`: `id` (the token's id), `kind`
/// (`extension`, `shortcut` or `migrate`) and `via`: `cli` (`admin
/// migrate-token`), `account` (`POST /me/tokens`), `device` (the device
/// flow, whose approver is the actor) or `pairing` (`POST /extension/pair`,
/// whose code's owner is the actor).
pub const API_TOKEN_CREATE: &str = "api_token.create";
/// An API token was revoked. `meta`: `id` and `kind`; plus `via: pairing`
/// when pairing the same browser installation again replaced it (P2-G16).
/// Without `via`, the account revoked it.
pub const API_TOKEN_REVOKE: &str = "api_token.revoke";
/// A signed-in user asked for a pairing code (`POST /me/tokens/pairing-code`).
/// `meta`: `kind` (`extension`) and `expiresAt`. Never the code.
pub const PAIRING_CODE_CREATE: &str = "pairing_code.create";
/// The operator set a server-wide flag (`admin flags set`). `meta`: `key`
/// and `value` (JSON). Flags hold no secrets.
pub const FLAG_SET: &str = "flag.set";
/// The operator removed a flag, so its default applies (`admin flags
/// unset`). `meta`: `key`.
pub const FLAG_UNSET: &str = "flag.unset";
/// A signed-in user approved a device code (the migration CLI's sign-in).
/// `meta`: `scope` (what the device gets: `migrate`).
pub const DEVICE_APPROVE: &str = "device.approve";
/// A user accepted the disclaimer and the privacy notice. `meta`:
/// `disclaimerVersion` and `privacyVersion`.
pub const CONSENT_ACCEPT: &str = "consent.accept";
/// A user's library was locked for maintenance (`admin user lock`).
pub const USER_LOCK: &str = "user.lock";
/// A user's library was unlocked (`admin user unlock`).
pub const USER_UNLOCK: &str = "user.unlock";
/// A user's library was replaced by a restored copy (`admin user
/// restore-db`). `meta`: `bytes` and `keptPrevious`.
pub const LIBRARY_RESTORE: &str = "library.restore";
/// A user's limits changed (`admin user limits`). `meta`: `via`, the
/// limits now in force (`quotaBytes`, `captureDailyLimit`) and those before
/// (`previous`).
pub const USER_LIMITS: &str = "user.limits";

/// One audit row.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    /// What happened (`<object>.<verb>`).
    pub action: &'a str,
    /// Who did it; `None` for the operator CLI.
    pub actor_user_id: Option<&'a str>,
    /// What it applied to (a user id, an invite reference).
    pub target: Option<&'a str>,
    /// Extra facts as JSON; no secrets, no user content.
    pub meta: Option<&'a serde_json::Value>,
}

/// Appends `entry`; returns its id.
///
/// # Errors
///
/// The insert failed.
pub fn record(conn: &Connection, entry: &Entry<'_>, now: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO audit_log (at, actor_user_id, action, target, meta_json) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            now,
            entry.actor_user_id,
            entry.action,
            entry.target,
            entry.meta.map(serde_json::Value::to_string),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}
