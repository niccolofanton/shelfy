//! Re-authentication (plan §2.11): a signed-in user proves who they are
//! again, without signing out, and the routes that take
//! [`RecentAuth`](super::RecentAuth) open for 5 minutes.
//!
//! | Proof | Flow |
//! |---|---|
//! | a passkey | `POST /auth/reauth/start` `{method: passkey}` → `navigator.credentials.get()` → `POST /auth/reauth/finish` `{method: passkey, ceremonyId, credential}` ([`super::passkeys`]) |
//! | an emailed link | `POST /auth/reauth/start` `{method: email}` (202) → the email's link `<public url>/login/reauth#<token>` → the SPA page posts `POST /auth/reauth/finish` `{method: link, token}` |
//! | an operator's link | `shelfy-server admin login-link --purpose reauth` prints the same kind of link, for when email is off |
//!
//! A link works once, for 15 minutes, and only for the signed-in account it
//! was minted for; nothing redeems it on `GET`. Either way, the transaction
//! that checks the proof also moves the session's `reauth_at` to now and
//! writes `session.reauth` to the audit log ([`mark`]); then the session's
//! cached lookup is dropped, so the next request sees the new time.

use std::sync::Arc;

use rusqlite::Connection;
use serde_json::json;
use shelfy_core::repo::RepoError;

use super::session::{SessionUser, SignInMethod};
use crate::control::audit::{self, Entry};
use crate::control::magic_links::{self, Purpose};
use crate::control::sessions;
use crate::error::{ApiError, ErrorCode};
use crate::ids::now_ms;
use crate::state::{AppState, blocking};
use crate::tokens::{TokenHash, hash_token, is_token_shaped};

/// Records a proof of identity for session `id_hash` of `user_id` at `now`,
/// inside the caller's write transaction (the one that checked the proof),
/// and writes `session.reauth`. Returns whether the session exists. The
/// caller drops the session's cached lookup after the commit
/// ([`super::AuthState::forget_session`]).
///
/// # Errors
///
/// A query failed.
pub(crate) fn mark(
    tx: &Connection,
    id_hash: &TokenHash,
    user_id: &str,
    method: SignInMethod,
    now: i64,
) -> Result<bool, RepoError> {
    if !sessions::set_reauth(tx, id_hash, now)? {
        return Ok(false);
    }
    let meta = json!({ "method": method.as_str() });
    let entry = Entry {
        action: audit::SESSION_REAUTH,
        actor_user_id: Some(user_id),
        target: Some(user_id),
        meta: Some(&meta),
    };
    audit::record(tx, &entry, now)?;
    Ok(true)
}

/// Re-authenticates `user`'s session with the re-authentication link
/// `token`. Returns false, and leaves the link unused, when it is unknown,
/// used, expired, not a re-authentication link, or another account's; that
/// is found on a reader first, so only a usable link takes the writer.
///
/// # Errors
///
/// 401 when the session ended meanwhile; the control database failed.
pub async fn with_link(
    state: &AppState,
    user: &SessionUser,
    token: &str,
) -> Result<bool, ApiError> {
    if !is_token_shaped(token) {
        return Ok(false);
    }
    let link_hash = hash_token(token);
    let now = now_ms();
    let reader = Arc::clone(state.control());
    let usable = blocking(move || {
        reader.read(|conn| magic_links::is_redeemable(conn, &link_hash, Purpose::Reauth, now))
    })
    .await?;
    if !usable {
        return Ok(false);
    }
    let control = Arc::clone(state.control());
    let session = *user.session().id_hash();
    let user_id = user.id().to_owned();
    let outcome = blocking(move || {
        control.write(|tx| {
            if sessions::find(tx, &session)?.is_none() {
                return Ok(None);
            }
            if !magic_links::consume_for_user(tx, &link_hash, Purpose::Reauth, &user_id, now)? {
                return Ok(Some(false));
            }
            mark(tx, &session, &user_id, SignInMethod::MagicLink, now)?;
            Ok::<_, RepoError>(Some(true))
        })
    })
    .await;
    state.auth().forget_session(&session);
    outcome?.ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))
}
