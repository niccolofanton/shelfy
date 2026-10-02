//! Opaque server-side sessions (plan §2.11).
//!
//! - **Token.** 256 random bits in the `__Host-shelfy_session` cookie
//!   ([`super::cookie`]); `sessions` stores only its SHA-256.
//! - **Expiry.** 30 days without use (sliding: [`resolve`] records a use at
//!   most once per [`AuthConfig::session_touch_every`]) and 90 days after
//!   sign-in, whichever comes first. A disabled user's sessions stop working.
//! - **Rotation.** Signing in replaces the session the browser already held,
//!   so a session id never survives a sign-in.
//! - **Cache.** Lookups are cached for 60 s (moka). Sign-outs in this process
//!   drop the cached entries at once, and a lookup that raced a sign-out
//!   never caches what it read ([`super::AuthState::cache_session`]). Cookies
//!   that sign nobody in are remembered for 30 s, so a stale or forged cookie
//!   costs one database read, not one per request.
//! - **Who uses it.** The access gate ([`super::access::gate`]) resolves the
//!   cookie of every request to a route that accepts a session and inserts
//!   [`CurrentUser`] and [`SessionUser`]. A request with an `Authorization`
//!   header never gets a user from its cookie, so a token can never reach a
//!   cookie route (§2.9).
//! - **Re-authentication.** [`RecentAuth`] for actions that need a sign-in
//!   from the last 5 minutes.

use std::fmt;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap};
use rusqlite::Connection;
use serde_json::json;
use shelfy_core::repo::RepoError;

use super::{AuthConfig, cookie, millis};
use crate::control::audit::{self, Entry};
use crate::control::sessions::{self, NewSession, Session};
use crate::control::users::Role;
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::ids::now_ms;
use crate::state::{AppState, blocking};
use crate::tokens::{SecretToken, TokenHash, hash_token, is_token_shaped};

/// A usable session, as cached.
#[derive(Clone, Debug)]
pub(crate) struct CachedSession {
    user_id: Arc<str>,
    role: Role,
    created_at: i64,
    expires_at: i64,
    last_seen_at: i64,
    reauth_at: Option<i64>,
}

impl CachedSession {
    fn from_row(session: Session) -> Self {
        Self {
            user_id: session.user_id.into(),
            role: session.role,
            created_at: session.created_at,
            expires_at: session.expires_at,
            last_seen_at: session.last_seen_at,
            reauth_at: session.reauth_at,
        }
    }

    fn is_usable(&self, now: i64, idle_ms: i64) -> bool {
        now < self.expires_at && now < self.last_seen_at.saturating_add(idle_ms)
    }
}

/// The user a session cookie signed in, with the role and the session.
/// The access gate inserts it next to the [`CurrentUser`]. Routes that only
/// act on the user's data take [`CurrentUser`]; routes that need the role or
/// the session (owner-only routes, [`RecentAuth`]) take this. Without a
/// signed-in session, 401 `unauthorized`.
#[derive(Clone, Debug)]
pub struct SessionUser {
    user_id: Arc<str>,
    role: Role,
    session: SessionInfo,
}

/// The session behind a [`SessionUser`].
#[derive(Clone, Copy)]
pub struct SessionInfo {
    id_hash: TokenHash,
    /// Sign-in time, unix ms.
    pub created_at: i64,
    /// Absolute expiry, unix ms.
    pub expires_at: i64,
    /// Last proof of identity (sign-in or re-authentication), unix ms.
    pub reauth_at: Option<i64>,
}

impl SessionInfo {
    /// The session's key in `sessions` (the SHA-256 of its cookie).
    #[must_use]
    pub fn id_hash(&self) -> &TokenHash {
        &self.id_hash
    }
}

impl fmt::Debug for SessionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionInfo")
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("reauth_at", &self.reauth_at)
            .finish_non_exhaustive()
    }
}

impl SessionUser {
    /// The user id (ULID).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.user_id
    }

    /// The user's role.
    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    /// The session.
    #[must_use]
    pub fn session(&self) -> &SessionInfo {
        &self.session
    }

    /// Refuses anyone but the owner, for the admin routes.
    ///
    /// # Errors
    ///
    /// 403 `forbidden` for a member.
    pub fn require_owner(&self) -> Result<(), ApiError> {
        match self.role {
            Role::Owner => Ok(()),
            Role::Member => Err(ApiError::new(ErrorCode::Forbidden)),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for SessionUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))
    }
}

/// A [`SessionUser`] who proved their identity within
/// [`AuthConfig::reauth_window`] (§2.11: 5 minutes): required for account
/// deletion, resets, token creation and passkey removal. A sign-in counts;
/// P1-13 adds re-authentication without signing out. Otherwise the request
/// answers 403 `reauth_required`, and the SPA opens its re-auth dialog.
#[derive(Clone, Debug)]
pub struct RecentAuth(pub SessionUser);

impl FromRequestParts<AppState> for RecentAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = SessionUser::from_request_parts(parts, state).await?;
        let window = millis(state.auth().config().reauth_window);
        match user.session.reauth_at {
            Some(at) if now_ms().saturating_sub(at) <= window => Ok(Self(user)),
            _ => Err(ApiError::new(ErrorCode::ReauthRequired)),
        }
    }
}

/// The user the request's session cookie signs in at `now`, if any. A
/// request with an `Authorization` header has none ([`cookie::session_token`]).
///
/// # Errors
///
/// The control database failed.
pub async fn from_cookie(
    state: &AppState,
    headers: &HeaderMap,
    now: i64,
) -> Result<Option<SessionUser>, ApiError> {
    match cookie::session_token(headers) {
        Some(token) => resolve(state, token, now).await,
        None => Ok(None),
    }
}

/// Puts `user` into the request: [`CurrentUser`] and [`SessionUser`] in the
/// extensions, its id in the request span.
pub fn attach(extensions: &mut Extensions, user: SessionUser) {
    tracing::Span::current().record("user_id", user.id());
    extensions.insert(CurrentUser::new(user.id()));
    extensions.insert(user);
}

/// The user that the session token `token` signs in at `now`, if any.
/// Records the use when the last one is older than
/// [`AuthConfig::session_touch_every`].
///
/// # Errors
///
/// The control database failed.
pub async fn resolve(
    state: &AppState,
    token: &str,
    now: i64,
) -> Result<Option<SessionUser>, ApiError> {
    if !is_token_shaped(token) {
        return Ok(None);
    }
    let id_hash = hash_token(token);
    let auth = state.auth();
    if auth.is_known_miss(&id_hash) {
        return Ok(None);
    }
    let config = auth.config();
    let idle = millis(config.session_idle);

    let entry = if let Some(entry) = auth.cached_session(&id_hash) {
        entry
    } else {
        let seen = auth.revision();
        let control = Arc::clone(state.control());
        let found = blocking(move || control.read(|conn| sessions::find(conn, &id_hash))).await?;
        match found {
            Some(session) if session.is_usable(now, idle) => {
                let entry = Arc::new(CachedSession::from_row(session));
                auth.cache_session(id_hash, Arc::clone(&entry), seen);
                entry
            }
            _ => {
                auth.remember_miss(id_hash);
                return Ok(None);
            }
        }
    };
    if !entry.is_usable(now, idle) {
        auth.drop_session(&id_hash);
        auth.remember_miss(id_hash);
        return Ok(None);
    }

    let entry = if now.saturating_sub(entry.last_seen_at) >= millis(config.session_touch_every) {
        let seen = auth.revision();
        let control = Arc::clone(state.control());
        let exists =
            blocking(move || control.write(|tx| sessions::touch(tx, &id_hash, now))).await?;
        if !exists {
            auth.drop_session(&id_hash);
            auth.remember_miss(id_hash);
            return Ok(None);
        }
        let touched = Arc::new(CachedSession {
            last_seen_at: now,
            ..(*entry).clone()
        });
        auth.cache_session(id_hash, Arc::clone(&touched), seen);
        touched
    } else {
        entry
    };

    Ok(Some(SessionUser {
        user_id: Arc::clone(&entry.user_id),
        role: entry.role,
        session: SessionInfo {
            id_hash,
            created_at: entry.created_at,
            expires_at: entry.expires_at,
            reauth_at: entry.reauth_at,
        },
    }))
}

/// How a user proved who they are, recorded in the audit log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInMethod {
    /// A sign-in link (email or `admin login-link`).
    MagicLink,
}

impl SignInMethod {
    /// The audit value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MagicLink => "magic_link",
        }
    }
}

/// Creates a session for `user_id` inside the caller's write transaction
/// (the one that verified the proof of identity), and returns its token, the
/// only copy, for the cookie.
///
/// It deletes `replaced` (the session the browser already held: rotation)
/// and every expired session, and writes `session.create` to the audit log.
/// The caller drops `replaced` from the cache after the commit
/// ([`super::AuthState::forget_session`]).
///
/// # Errors
///
/// A query failed.
pub fn create_session(
    tx: &Connection,
    config: &AuthConfig,
    user_id: &str,
    replaced: Option<&TokenHash>,
    user_agent: Option<&str>,
    method: SignInMethod,
    now: i64,
) -> Result<SecretToken, RepoError> {
    let rotated = match replaced {
        Some(old) => sessions::delete(tx, old)?.is_some(),
        None => false,
    };
    sessions::prune(tx, now, millis(config.session_idle))?;
    let token = SecretToken::generate();
    let id_hash = token.hash();
    let session = NewSession {
        id_hash: &id_hash,
        user_id,
        expires_at: now.saturating_add(millis(config.session_lifetime)),
        user_agent,
    };
    sessions::insert(tx, &session, now)?;
    sessions::touch(tx, &id_hash, now)?;
    let meta = json!({ "method": method.as_str(), "rotated": rotated });
    let entry = Entry {
        action: audit::SESSION_CREATE,
        actor_user_id: Some(user_id),
        target: Some(user_id),
        meta: Some(&meta),
    };
    audit::record(tx, &entry, now)?;
    Ok(token)
}

/// Ends the session of the cookie value `token` (sign-out). Returns its
/// user when a session existed; an unknown or malformed token is not an
/// error.
///
/// # Errors
///
/// The control database failed.
pub async fn end_session(state: &AppState, token: &str) -> Result<Option<String>, ApiError> {
    if !is_token_shaped(token) {
        return Ok(None);
    }
    let id_hash = hash_token(token);
    let control = Arc::clone(state.control());
    let now = now_ms();
    let ended = blocking(move || {
        control.write(|tx| {
            let Some(user_id) = sessions::delete(tx, &id_hash)? else {
                return Ok(None);
            };
            let meta = json!({ "scope": "current", "count": 1 });
            let entry = Entry {
                action: audit::SESSION_DELETE,
                actor_user_id: Some(&user_id),
                target: Some(&user_id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok::<_, RepoError>(Some(user_id))
        })
    })
    .await?;
    state.auth().forget_session(&id_hash);
    Ok(ended)
}

/// Ends every session of `user_id` (sign-out everywhere), the current one
/// included. Returns how many ended.
///
/// # Errors
///
/// The control database failed.
pub async fn end_all_sessions(state: &AppState, user_id: &str) -> Result<usize, ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user_id.to_owned();
    let now = now_ms();
    let ended = blocking(move || {
        control.write(|tx| {
            let count = sessions::delete_for_user(tx, &user_id)?;
            let meta = json!({ "scope": "all", "count": count });
            let entry = Entry {
                action: audit::SESSION_DELETE,
                actor_user_id: Some(&user_id),
                target: Some(&user_id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok::<_, RepoError>(count)
        })
    })
    .await?;
    state.auth().forget_all_sessions();
    Ok(ended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthState;

    fn entry() -> Arc<CachedSession> {
        Arc::new(CachedSession {
            user_id: "01OWNER0000000000000000000".into(),
            role: Role::Owner,
            created_at: 0,
            expires_at: i64::MAX,
            last_seen_at: 0,
            reauth_at: None,
        })
    }

    #[test]
    fn a_lookup_that_raced_a_revocation_is_never_cached() {
        let auth = AuthState::new(AuthConfig::default());
        let hash = [7_u8; 32];

        // No revocation since the read: cached.
        let seen = auth.revision();
        auth.cache_session(hash, entry(), seen);
        assert!(auth.cached_session(&hash).is_some());

        // A sign-out between the read and the insert: dropped, not cached.
        let seen = auth.revision();
        auth.forget_session(&hash);
        auth.cache_session(hash, entry(), seen);
        assert!(auth.cached_session(&hash).is_none());

        // Sign-out everywhere, likewise; the next lookup caches again.
        let seen = auth.revision();
        auth.forget_all_sessions();
        auth.cache_session(hash, entry(), seen);
        assert!(auth.cached_session(&hash).is_none());
        let seen = auth.revision();
        auth.cache_session(hash, entry(), seen);
        assert!(auth.cached_session(&hash).is_some());
    }

    #[test]
    fn concurrent_revocations_never_leave_a_stale_entry() {
        // A lookup inserts what it read while a revocation runs: whichever
        // order they take, no entry from the earlier read survives.
        let auth = Arc::new(AuthState::new(AuthConfig::default()));
        let hash = [9_u8; 32];
        for _ in 0..200 {
            let seen = auth.revision();
            let revoker = {
                let auth = Arc::clone(&auth);
                std::thread::spawn(move || auth.forget_session(&hash))
            };
            auth.cache_session(hash, entry(), seen);
            revoker.join().unwrap();
            assert!(auth.cached_session(&hash).is_none());
        }
    }
}
