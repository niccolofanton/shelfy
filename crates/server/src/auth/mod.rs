//! Authentication (plan D8, §2.11, §7.1; owner-only under E4).
//!
//! **How a user signs in.** With a passkey, username-less
//! (`POST /api/v1/auth/passkeys/login/{start,finish}`, [`passkeys`]), or with
//! a link. The owner asks for an email link (`POST /api/v1/auth/magic-links`,
//! when email is configured) or gets one from the operator
//! (`shelfy-server admin login-link`, the way in while SMTP is optional, and
//! the way to register a first passkey). The link is
//! `<public url>/login/magic#<token>`: the SPA's sign-in page reads the token
//! from the fragment and, after a click, redeems it with
//! `POST /api/v1/auth/magic-links/redeem`. Either way the server creates an
//! opaque session held in the `__Host-shelfy_session` cookie. Nothing signs
//! in on `GET` ([`magic_link`]). There is no sign-up: links and passkeys
//! exist only for existing accounts.
//!
//! **Re-authentication.** Sensitive routes take [`RecentAuth`]: a sign-in or
//! a re-authentication ([`reauth`]: a passkey, or a link with purpose
//! `reauth`) in the last 5 minutes.
//!
//! **Deny by default.** Every route needs a signed-in session unless the
//! router's access policy ([`access`], lists in [`crate::routes`]) makes it
//! public or opens it to scoped API tokens. The gate ([`access::gate`]) runs
//! after routing and before the handler: it verifies the session cookie (or
//! the token) the route accepts, inserts the user into the request extensions
//! ([`CurrentUser`](crate::current_user::CurrentUser), [`SessionUser`],
//! [`bearer::TokenPrincipal`]) and names it in the request span, or answers
//! 401 itself. Handlers take the user as an extractor: [`CurrentUser`]
//! (the user's data), [`SessionUser`] (role and session), [`RecentAuth`]
//! (sensitive actions) or [`bearer::TokenUser`]. Unsafe requests without an
//! `Authorization` header also pass the [`csrf`] guard, which runs first.
//!
//! | Module | Contents |
//! |---|---|
//! | [`access`] | the access policy and the gate |
//! | [`cookie`] | the session cookie |
//! | [`session`] | sessions: resolution, [`SessionUser`], [`RecentAuth`], creation and sign-out |
//! | [`magic_link`] | sign-in and re-authentication links: email requests, minting, redemption |
//! | [`passkeys`] | passkeys: registration, username-less sign-in, re-authentication |
//! | [`reauth`] | re-authentication: the session's `reauth_at`, links with purpose `reauth` |
//! | [`csrf`] | the Origin / `Sec-Fetch-Site` / `X-Shelfy-Client` guard |
//! | [`rate_limit`] | limits on sign-in requests |
//! | [`bearer`] | API tokens: verification and [`bearer::TokenUser`]; P1-17 mints them |
//! | [`openapi`] | the security schemes of the OpenAPI document |
//!
//! **Seams.**
//!
//! - Sensitive routes (P1-17: token creation, device-code approval; account
//!   reset and deletion) take [`RecentAuth`]; without a recent proof they
//!   answer 403 `reauth_required`, and the SPA re-authenticates and retries.
//! - API tokens (P1-17): [`bearer`] verifies `api_tokens`; P1-17 adds minting,
//!   revocation and `last_used_at`. A route opens to tokens in
//!   [`crate::routes::TOKEN_ROUTES`].
//! - OpenAPI: the document's default security is the session
//!   ([`openapi::SecuritySchemes`]); a public route also declares
//!   `security(())` and a token route `security(("bearer" = ["<scope>"]))` in
//!   its `#[utoipa::path]`. The authz test checks that the document and the
//!   access policy agree.

pub mod access;
pub mod bearer;
pub mod cookie;
pub mod csrf;
pub mod magic_link;
pub mod openapi;
pub mod passkeys;
pub mod rate_limit;
pub mod reauth;
pub mod session;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use utoipa::ToSchema;

pub use session::{RecentAuth, SessionUser};

use passkeys::Passkeys;
use rate_limit::{RateLimit, RateLimiter};
use session::CachedSession;

use crate::config::PublicUrl;
use crate::tokens::TokenHash;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);

/// Most sessions kept in the lookup cache.
const SESSION_CACHE_CAPACITY: u64 = 10_000;
/// Most unknown session cookies remembered as misses.
const MISS_CACHE_CAPACITY: u64 = 10_000;

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
    /// How long a cookie that signs nobody in is remembered as such, so a
    /// stale or forged cookie does not cost a database read per request.
    pub session_miss_ttl: Duration,
    /// How long a sign-in link stays valid (15 minutes).
    pub magic_link_ttl: Duration,
    /// How long a passkey ceremony may take, from its options to the
    /// browser's answer (§2.11: 5 minutes); also the `timeout` the options
    /// give the browser.
    pub passkey_ceremony_ttl: Duration,
    /// How recent a sign-in must be for [`RecentAuth`] (5 minutes).
    pub reauth_window: Duration,
    /// Sign-in requests per client address (§2.9: 10 per minute; an IPv6
    /// client counts by its /64).
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
            session_miss_ttl: Duration::from_secs(30),
            magic_link_ttl: 15 * MINUTE,
            passkey_ceremony_ttl: 5 * MINUTE,
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
    /// Session cookies that signed nobody in, recently.
    misses: Cache<TokenHash, ()>,
    /// Bumped by every revocation, so a lookup that raced one does not cache
    /// what it read ([`AuthState::cache_session`]).
    revocations: AtomicU64,
    ip_limiter: RateLimiter,
    address_limiter: RateLimiter,
    mail_slots: Arc<Semaphore>,
    passkeys: Passkeys,
}

impl AuthState {
    /// The state for `config`, with the passkey relying party of
    /// `public_url`.
    #[must_use]
    pub fn new(config: AuthConfig, public_url: &PublicUrl) -> Self {
        let sessions = Cache::builder()
            .max_capacity(SESSION_CACHE_CAPACITY)
            .time_to_live(config.session_cache_ttl.max(Duration::from_millis(1)))
            .build();
        let misses = Cache::builder()
            .max_capacity(MISS_CACHE_CAPACITY)
            .time_to_live(config.session_miss_ttl.max(Duration::from_millis(1)))
            .build();
        Self {
            ip_limiter: RateLimiter::new(config.ip_limit),
            address_limiter: RateLimiter::new(config.address_limit),
            mail_slots: Arc::new(Semaphore::new(config.mail_concurrency.max(1))),
            passkeys: Passkeys::new(public_url, config.passkey_ceremony_ttl),
            sessions,
            misses,
            revocations: AtomicU64::new(0),
            config,
        }
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// The passkey relying party and its ceremonies in flight.
    #[must_use]
    pub fn passkeys(&self) -> &Passkeys {
        &self.passkeys
    }

    /// Drops the cached lookup of one session, after it changed (sign-out,
    /// rotation, re-authentication).
    pub fn forget_session(&self, id_hash: &TokenHash) {
        // Count first, then drop: a lookup that read the row before the
        // change either sees the new count or loses its entry to this drop.
        self.revocations.fetch_add(1, Ordering::SeqCst);
        self.sessions.invalidate(id_hash);
    }

    /// Drops every cached session lookup (sign-out everywhere, a user
    /// disabled). Lookups reload from the database.
    pub fn forget_all_sessions(&self) {
        self.revocations.fetch_add(1, Ordering::SeqCst);
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

    /// The revocation count; read it before a database lookup whose result
    /// goes to [`AuthState::cache_session`].
    pub(crate) fn revision(&self) -> u64 {
        self.revocations.load(Ordering::SeqCst)
    }

    /// The cached lookup of a session.
    pub(crate) fn cached_session(&self, id_hash: &TokenHash) -> Option<Arc<CachedSession>> {
        self.sessions.get(id_hash)
    }

    /// Caches a session read from the database when no revocation happened
    /// since `seen` ([`AuthState::revision`] before the read). A revocation
    /// that lands between the check and the insert is caught by the second
    /// check, which drops the entry again; so a revoked session is never
    /// served from the cache.
    pub(crate) fn cache_session(&self, id_hash: TokenHash, entry: Arc<CachedSession>, seen: u64) {
        if self.revision() != seen {
            return;
        }
        self.sessions.insert(id_hash, entry);
        if self.revision() != seen {
            self.sessions.invalidate(&id_hash);
        }
    }

    /// Drops a cached session that turned out unusable.
    pub(crate) fn drop_session(&self, id_hash: &TokenHash) {
        self.sessions.invalidate(id_hash);
    }

    /// Whether `id_hash` recently signed nobody in.
    pub(crate) fn is_known_miss(&self, id_hash: &TokenHash) -> bool {
        self.misses.contains_key(id_hash)
    }

    /// Remembers that `id_hash` signs nobody in, for
    /// [`AuthConfig::session_miss_ttl`].
    pub(crate) fn remember_miss(&self, id_hash: TokenHash) {
        self.misses.insert(id_hash, ());
    }

    /// Forgets a remembered miss (a new session with this hash).
    pub(crate) fn forget_miss(&self, id_hash: &TokenHash) {
        self.misses.invalidate(id_hash);
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
    /// Passkeys work: the public URL is an https origin or localhost, with a
    /// domain name. Says nothing about which accounts have passkeys.
    pub passkeys: bool,
}
