//! `audit_log` (plan §2.11): every destructive or admin action leaves a row.
//! Actions are `<object>.<verb>`; `actor_user_id` is empty for the CLI.
//! Rows never carry secrets, and `meta_json` never carries user content.

use rusqlite::{Connection, params};
use shelfy_core::repo::Result;

/// `admin create-owner` created the owner.
pub const OWNER_CREATE: &str = "owner.create";
/// An invite was created.
pub const INVITE_CREATE: &str = "invite.create";
/// A sign-in link was minted. `meta`: `via` (`email` or `cli`) and `purpose`
/// (`login` or `reauth`).
pub const MAGIC_LINK_CREATE: &str = "magic_link.create";
/// A user signed in: a session was created. `meta`: `method` (`magic_link`
/// or `passkey`) and `rotated`.
pub const SESSION_CREATE: &str = "session.create";
/// Sessions ended. `meta`: `scope` (`current` for a sign-out, `all` for a
/// sign-out everywhere) and `count`.
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
/// An API token was minted. `meta`: `via` (`cli`) and `kind` (`migrate`).
pub const API_TOKEN_CREATE: &str = "api_token.create";
/// A user's library was locked for maintenance (`admin user lock`).
pub const USER_LOCK: &str = "user.lock";
/// A user's library was unlocked (`admin user unlock`).
pub const USER_UNLOCK: &str = "user.unlock";
/// A user's library was replaced by a restored copy (`admin user
/// restore-db`). `meta`: `bytes` and `keptPrevious`.
pub const LIBRARY_RESTORE: &str = "library.restore";

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
