//! Authentication (plan D8, §2.11, §7.1; owner-only under E4).
//!
//! **How a user signs in today.** The owner asks for an email link
//! (`POST /api/v1/auth/magic-links`, when email is configured) or gets one
//! from the operator (`shelfy-server admin login-link`, the way in while SMTP
//! is optional). Opening it (`GET /api/v1/auth/magic/{token}`, or
//! `POST /api/v1/auth/magic-links/redeem` from a page) creates an opaque
//! server-side session held in the `__Host-shelfy_session` cookie. There is no
//! sign-up: links exist only for existing accounts.
//!
//! **How a route requires it.** The authentication layer
//! ([`session::authenticate`], in the stack of [`crate::app`]) verifies the
//! session cookie of every request and, when it signs a user in, inserts
//! [`CurrentUser`](crate::current_user::CurrentUser) and [`SessionUser`] into
//! the request extensions and names the user in the request span. A handler
//! takes [`CurrentUser`](crate::current_user::CurrentUser) (the user's data),
//! [`SessionUser`] (the role and the session) or [`RecentAuth`] (sensitive
//! actions); each answers 401 without a signed-in session. Cookie requests
//! that change state also pass the [`csrf`] guard, which runs first.
//!
//! | Module | Contents |
//! |---|---|
//! | [`cookie`] | the session cookie |
//! | [`session`] | the authentication layer, [`SessionUser`], [`RecentAuth`], session creation and sign-out |
//! | [`magic_link`] | sign-in links: email requests, minting, redemption |
//! | [`csrf`] | the Origin / `Sec-Fetch-Site` / `X-Shelfy-Client` guard |
//! | [`rate_limit`] | limits on sign-in requests |
//! | [`bearer`] | API tokens: a typed extractor, completed by P1-17 |
//! | [`openapi`] | the security schemes of the OpenAPI document |
//!
//! **Seams.**
//!
//! - Passkeys (P1-13): `auth/passkeys.rs` with `webauthn-rs`, RP ID and origin
//!   from `SHELFY_PUBLIC_URL`; a successful assertion calls
//!   [`session::create_session`] with method `passkey`, like a redeemed link.
//!   [`AuthMethods::passkeys`] turns true.
//! - Re-authentication (P1-13): `POST /auth/reauth/{start,finish}` set the
//!   session's `reauth_at` ([`crate::control::sessions::set_reauth`]) and call
//!   [`AuthState::forget_session`]; routes that need it take [`RecentAuth`].
//!   Links with purpose `reauth` already exist in the schema
//!   ([`crate::control::magic_links::Purpose`]).
//! - API tokens (P1-17): [`bearer::TokenUser`] reads `api_tokens`; P1-17 adds
//!   creation, revocation and `last_used_at`, and an extractor for routes that
//!   take the cookie or a token. The layer keeps inserting
//!   [`CurrentUser`](crate::current_user::CurrentUser) for cookie sessions
//!   only, so cookie routes keep refusing tokens.
//! - OpenAPI: the document's default security is the session
//!   ([`openapi::SecuritySchemes`]); a public route opts out with
//!   `security(())` in its `#[utoipa::path]`.

pub mod bearer;
pub mod cookie;
pub mod csrf;
pub mod magic_link;
pub mod openapi;
pub mod rate_limit;
pub mod session;

use std::sync::Arc;
use std::time::Duration;

use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use utoipa::ToSchema;

pub use session::{RecentAuth, SessionUser};

use rate_limit::{RateLimit, RateLimiter};
use session::CachedSession;

use crate::tokens::TokenHash;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);

/// Most sessions kept in the lookup cache.
const SESSION_CACHE_CAPACITY: u64 = 10_000;

/// Lifetimes and limits of authentication. The defaults are the plan's;
/// tests shorten them.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// A session ends after this long without use (§2.11: 30 days, sliding).
    pub session_idle: Duration,
    /// A session ends this long after sign-in, used or not (90 days).
    pub session_lifetime: Duration,
    /// A session's last use is written at most this often.
    pub session_touch_every: Duration,
    /// How long a session lookup is cached (§2.11: 60 s). Sign-out in this
    /// process takes effect at once; changes made by another process (the
    /// admin CLI) within this delay.
    pub session_cache_ttl: Duration,
    /// How long a sign-in link stays valid (15 minutes).
    pub magic_link_ttl: Duration,
    /// How recent a sign-in must be for [`RecentAuth`] (5 minutes).
    pub reauth_window: Duration,
    /// Sign-in requests per client IP (§2.9: 10 per minute).
    pub ip_limit: RateLimit,
    /// Sign-in emails per address (§2.11: 3 per hour).
    pub address_limit: RateLimit,
    /// Sign-in emails being sent at once; requests beyond it are dropped.
    pub mail_concurrency: usize,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_idle: 30 * DAY,
            session_lifetime: 90 * DAY,
            session_touch_every: HOUR,
            session_cache_ttl: MINUTE,
            magic_link_ttl: 15 * MINUTE,
            reauth_window: 5 * MINUTE,
            ip_limit: RateLimit {
                max: 10,
                window: MINUTE,
            },
            address_limit: RateLimit {
                max: 3,
                window: HOUR,
            },
            mail_concurrency: 4,
        }
    }
}

/// `duration` in milliseconds, saturating.
pub(crate) fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Runtime state of authentication, shared by every request.
pub struct AuthState {
    config: AuthConfig,
    sessions: Cache<TokenHash, Arc<CachedSession>>,
    ip_limiter: RateLimiter,
    address_limiter: RateLimiter,
    mail_slots: Arc<Semaphore>,
}

impl AuthState {
    /// The state for `config`.
    #[must_use]
    pub fn new(config: AuthConfig) -> Self {
        let sessions = Cache::builder()
            .max_capacity(SESSION_CACHE_CAPACITY)
            .time_to_live(config.session_cache_ttl.max(Duration::from_millis(1)))
            .build();
        Self {
            ip_limiter: RateLimiter::new(config.ip_limit),
            address_limiter: RateLimiter::new(config.address_limit),
            mail_slots: Arc::new(Semaphore::new(config.mail_concurrency.max(1))),
            sessions,
            config,
        }
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Drops the cached lookup of one session, after it changed.
    pub fn forget_session(&self, id_hash: &TokenHash) {
        self.sessions.invalidate(id_hash);
    }

    /// Drops every cached session lookup (sign-out everywhere, a user
    /// disabled). Lookups reload from the database.
    pub fn forget_all_sessions(&self) {
        self.sessions.invalidate_all();
    }

    /// Waits until no sign-in email is being prepared or sent. Tests use it
    /// to read the dev mailbox deterministically.
    pub async fn mail_idle(&self) {
        let slots = u32::try_from(self.config.mail_concurrency.max(1)).unwrap_or(u32::MAX);
        if let Ok(all) = self.mail_slots.acquire_many(slots).await {
            drop(all);
        }
    }

    pub(crate) fn session_cache(&self) -> &Cache<TokenHash, Arc<CachedSession>> {
        &self.sessions
    }

    pub(crate) fn ip_limiter(&self) -> &RateLimiter {
        &self.ip_limiter
    }

    pub(crate) fn address_limiter(&self) -> &RateLimiter {
        &self.address_limiter
    }

    pub(crate) fn mail_slots(&self) -> &Arc<Semaphore> {
        &self.mail_slots
    }
}

/// How a user can sign in on this instance; the sign-in page reads it before
/// anyone is signed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethods {
    /// "Email me a link" works: an email transport is configured (E4: SMTP
    /// is optional). Without it, the operator mints links with
    /// `shelfy-server admin login-link`.
    pub email_link: bool,
    /// Passkey sign-in (P1-13). Always false for now.
    pub passkeys: bool,
}
