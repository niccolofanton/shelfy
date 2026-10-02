//! The account's sessions (plan §2.9 Account, §7.1 "session list with
//! remote logout"): `GET,DELETE /me/sessions`.
//!
//! [`list`] gives every usable session of the signed-in user, the current
//! one flagged. A session is named by [`public_id`]: 128 bits of a SHA-256
//! over a fixed prefix and the session's stored hash, in hex. It is stable
//! for the session's life and reveals neither the cookie nor what the
//! database stores. [`end`] signs one session out (the current one too: a
//! sign-out), [`end_others`] every session but the current one. Either
//! deletes the rows, writes `session.delete` to the audit log in the same
//! transaction (`scope` `current`, `remote` or `others`), and drops the
//! cached lookups, so a signed-out session stops working at once.

use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256};
use shelfy_core::repo::RepoError;

use super::{SessionUser, millis};
use crate::control::audit::{self, Entry};
use crate::control::sessions::{self, SessionRow};
use crate::error::ApiError;
use crate::ids::now_ms;
use crate::state::{AppState, blocking};
use crate::tokens::TokenHash;

/// Domain separation of [`public_id`].
const PUBLIC_ID_PREFIX: &[u8] = b"shelfy.session.public-id\0";

/// Hex digits in a [`public_id`].
pub const PUBLIC_ID_LEN: usize = 32;

/// The id of the session stored as `id_hash`, in the session list.
#[must_use]
pub fn public_id(id_hash: &TokenHash) -> String {
    let digest = Sha256::new()
        .chain_update(PUBLIC_ID_PREFIX)
        .chain_update(id_hash)
        .finalize();
    digest[..PUBLIC_ID_LEN / 2]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A session of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedSession {
    /// [`public_id`].
    pub id: String,
    /// Whether this is the session of the request.
    pub current: bool,
    /// Sign-in time, unix ms.
    pub created_at: i64,
    /// Last use, unix ms (recorded at most hourly).
    pub last_seen_at: i64,
    /// When it ends unless used again: the earlier of the absolute expiry
    /// and the idle one, unix ms.
    pub expires_at: i64,
    /// The browser's `User-Agent` at sign-in.
    pub user_agent: Option<String>,
}

/// How a session was signed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The session of the request: the response clears its cookie.
    Current,
    /// Another session.
    Other,
}

/// The usable sessions of `user`, the current one first, then the most
/// recently used.
///
/// # Errors
///
/// The control database failed.
pub async fn list(state: &AppState, user: &SessionUser) -> Result<Vec<ListedSession>, ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let rows =
        blocking(move || control.read(|conn| sessions::list_for_user(conn, &user_id))).await?;
    let idle = millis(state.auth().config().session_idle);
    let now = now_ms();
    let current = *user.session().id_hash();
    let mut listed: Vec<ListedSession> = rows
        .into_iter()
        .filter_map(|row| {
            let expires_at = row.expires_at.min(row.last_seen_at.saturating_add(idle));
            (now < expires_at).then(|| ListedSession {
                id: public_id(&row.id_hash),
                current: row.id_hash == current,
                created_at: row.created_at,
                last_seen_at: row.last_seen_at,
                expires_at,
                user_agent: row.user_agent,
            })
        })
        .collect();
    // Stable: the most recently used order stays among the others.
    listed.sort_by_key(|session| !session.current);
    Ok(listed)
}

/// Signs out the session of `user` named `id` ([`public_id`]); `None` when
/// the user has no such session.
///
/// # Errors
///
/// The control database failed.
pub async fn end(
    state: &AppState,
    user: &SessionUser,
    id: &str,
) -> Result<Option<Ended>, ApiError> {
    if id.len() != PUBLIC_ID_LEN {
        return Ok(None);
    }
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let current = *user.session().id_hash();
    let id = id.to_ascii_lowercase();
    let now = now_ms();
    let ended = blocking(move || {
        control.write(|tx| {
            let rows = sessions::list_for_user(tx, &user_id)?;
            let Some(row) = rows.iter().find(|row| public_id(&row.id_hash) == id) else {
                return Ok(None);
            };
            if !sessions::delete_of_user(tx, &user_id, &row.id_hash)? {
                return Ok(None);
            }
            let ended = if row.id_hash == current {
                Ended::Current
            } else {
                Ended::Other
            };
            let scope = match ended {
                Ended::Current => "current",
                Ended::Other => "remote",
            };
            record(tx, &user_id, scope, 1, now)?;
            Ok::<_, RepoError>(Some((row.id_hash, ended)))
        })
    })
    .await?;
    Ok(ended.map(|(id_hash, ended)| {
        state.auth().forget_session(&id_hash);
        ended
    }))
}

/// Signs out every session of `user` but the current one; returns how many.
///
/// # Errors
///
/// The control database failed.
pub async fn end_others(state: &AppState, user: &SessionUser) -> Result<usize, ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let current = *user.session().id_hash();
    let now = now_ms();
    let ended: Vec<TokenHash> = blocking(move || {
        control.write(|tx| {
            let mut ended = Vec::new();
            for SessionRow { id_hash, .. } in sessions::list_for_user(tx, &user_id)? {
                if id_hash != current && sessions::delete_of_user(tx, &user_id, &id_hash)? {
                    ended.push(id_hash);
                }
            }
            record(tx, &user_id, "others", ended.len(), now)?;
            Ok::<_, RepoError>(ended)
        })
    })
    .await?;
    for id_hash in &ended {
        state.auth().forget_session(id_hash);
    }
    Ok(ended.len())
}

/// Writes `session.delete`.
fn record(
    tx: &rusqlite::Connection,
    user_id: &str,
    scope: &str,
    count: usize,
    now: i64,
) -> Result<(), RepoError> {
    let meta = json!({ "scope": scope, "count": count });
    let entry = Entry {
        action: audit::SESSION_DELETE,
        actor_user_id: Some(user_id),
        target: Some(user_id),
        meta: Some(&meta),
    };
    audit::record(tx, &entry, now)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ids_are_stable_hex_unrelated_to_the_hash() {
        let a = [1_u8; 32];
        let id = public_id(&a);
        assert_eq!(id.len(), PUBLIC_ID_LEN);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(public_id(&a), id);
        assert_ne!(public_id(&[2_u8; 32]), id);
        let hex: String = a.iter().map(|b| format!("{b:02x}")).collect();
        assert!(!hex.contains(&id[..8]));
    }
}
