//! Rate limits (plan §2.9).
//!
//! | Scope | Default | Counted per | Requests | Layer |
//! |---|---|---|---|---|
//! | user | 20 per second, 60 at once | user | every `/api/v1/*` route a signed-in user calls, with a session or an API token | [`by_user`] |
//! | search | 5 per second, 5 at once | user | `GET /api/v1/search`, and `GET /api/v1/posts` and `/api/v1/posts/count` with a text query (`q` or `concept`) | [`by_user`] |
//! | client errors | 10 per minute, 10 at once | user | `POST /api/v1/client-errors` | [`by_user`] |
//! | sign-in | 10 per minute | client address | every `/api/v1/auth/*` route, signed in or not, but the device poll ([`UNCOUNTED_SIGN_IN_ROUTES`]); the extension's pairing ([`COUNTED_SIGN_IN_ROUTES`]); an answer 403 `reauth_required` is refunded | [`by_client`] |
//!
//! A request over a limit answers 429 `rate_limited` with `Retry-After`, the
//! seconds until it would pass, before the handler runs. A limit does not
//! count the requests it refuses. A search, or a client error report, also
//! counts against the user's limit, which is checked second: a request that
//! the user's limit refuses has spent its search (or report) slot.
//!
//! **Where.** [`by_client`] runs right inside the security headers
//! ([`crate::app`]), before the CSRF guard and the access gate: a flood of
//! sign-in attempts costs neither a session lookup nor a body. The client is
//! the TCP peer, or `CF-Connecting-IP` when the peer is a trusted proxy
//! ([`crate::net`]); an IPv6 client counts by its /64. Its limit and its
//! state are the sign-in ones of [`crate::auth`] (`AuthConfig::ip_limit`);
//! the per-address limit on sign-in emails stays in the handler. [`by_user`]
//! is a route layer right after the access gate, which has named the user;
//! anonymous requests, `/health` and `/media/*` (the gallery loads dozens of
//! thumbnails at once, and the browser caches them for good) are not counted.
//! Both layers sit inside the request observation and the security headers,
//! so a 429 is logged, counted in the metrics and carries the headers like
//! any answer.
//!
//! **Algorithm.** The user limits are token buckets: GCRA, one theoretical
//! arrival time per key in an atomic, so a check takes no lock. The sign-in
//! limit is the sliding window of [`crate::auth::rate_limit`].
//!
//! **Memory.** Each limiter is a `moka` cache of at most
//! [`RateLimitConfig::max_keys`] keys, least recently used out first. A key
//! idle for as long as its bucket takes to refill is dropped: its state is
//! then the same as a new key's, so dropping it loses nothing. Keys are
//! SHA-256 digests: no address or user id is held in clear.
//!
//! Seams: P3 adds AI suggest (1 per second) to [`route_scope`]; a new route
//! is covered by the user limit as soon as it is under `/api/v1`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use moka::policy::EvictionPolicy;
use moka::sync::Cache;

use crate::auth::rate_limit::{Key, ip_key, key};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::ids::now_ms;
use crate::net;
use crate::state::AppState;

/// Prefix of the routes the user limits cover.
pub const API_PREFIX: &str = "/api/v1/";
/// Prefix of the sign-in routes, limited per client address.
pub const AUTH_PREFIX: &str = "/api/v1/auth/";

/// Sign-in routes that the limit per client address does not count: the
/// device poll of the migration CLI (P1-17). The CLI polls every few seconds
/// (RFC 8628's interval is 5 s: 12 polls a minute) while the user approves
/// its code in a browser that usually shares its address; counted, the polls
/// alone would spend that browser's sign-in budget, and its re-authentication
/// and approval would answer 429. A poll needs the 256-bit device code, and
/// each device code is paced on its own instead ([`crate::auth::device`]:
/// `slow_down`, and at most 20 polls a minute).
pub const UNCOUNTED_SIGN_IN_ROUTES: &[(Method, &str)] =
    &[(Method::POST, "/api/v1/auth/device/poll")];

/// Routes outside `/api/v1/auth/` that the limit per client address counts
/// as sign-in requests: the browser extension's pairing (P2-03), a public
/// route that exchanges a code for a token. A code is 256 random bits, so
/// the limit guards the database from floods rather than guesses.
pub const COUNTED_SIGN_IN_ROUTES: &[(Method, &str)] = &[(Method::POST, "/api/v1/extension/pair")];

/// Most keys one limiter tracks.
pub const MAX_KEYS: u64 = 10_000;

/// A rate: one request per `period` on average, `burst` at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota {
    period: Duration,
    burst: u32,
}

impl Quota {
    /// `n` requests per second, `n` at once.
    ///
    /// # Panics
    ///
    /// When `n` is 0.
    #[must_use]
    pub const fn per_second(n: u32) -> Self {
        Self::every(Duration::from_secs(1), n)
    }

    /// `n` requests per minute, `n` at once.
    ///
    /// # Panics
    ///
    /// When `n` is 0.
    #[must_use]
    pub const fn per_minute(n: u32) -> Self {
        Self::every(Duration::from_secs(60), n)
    }

    /// `n` requests per hour, `n` at once.
    ///
    /// # Panics
    ///
    /// When `n` is 0.
    #[must_use]
    pub const fn per_hour(n: u32) -> Self {
        Self::every(Duration::from_secs(3600), n)
    }

    const fn every(window: Duration, n: u32) -> Self {
        assert!(n > 0, "a quota admits at least one request");
        let nanos = window.as_nanos() / n as u128;
        Self {
            period: Duration::from_nanos(nanos as u64),
            burst: n,
        }
    }

    /// The same rate, with `burst` requests at once.
    ///
    /// # Panics
    ///
    /// When `burst` is 0.
    #[must_use]
    pub const fn burst(self, burst: u32) -> Self {
        assert!(burst > 0, "a quota admits at least one request");
        Self { burst, ..self }
    }

    /// The time between two requests at the sustained rate.
    #[must_use]
    pub const fn period(&self) -> Duration {
        self.period
    }

    /// The requests admitted at once.
    #[must_use]
    pub const fn burst_size(&self) -> u32 {
        self.burst
    }

    /// How long an empty bucket takes to refill: after this long idle, a
    /// key is as good as new.
    fn refill(&self) -> Duration {
        self.period.saturating_mul(self.burst)
    }
}

/// The user limits; `None` turns one off. The defaults are the plan's (§2.9).
/// The sign-in limit per client address is `AuthConfig::ip_limit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// Every `/api/v1` request of a signed-in user: 20 per second, 60 at once.
    pub user: Option<Quota>,
    /// Searches of a user: 5 per second.
    pub search: Option<Quota>,
    /// Client error reports of a user: 10 per minute (P1 assumption).
    pub client_errors: Option<Quota>,
    /// Most keys each limiter tracks.
    pub max_keys: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            user: Some(Quota::per_second(20).burst(60)),
            search: Some(Quota::per_second(5)),
            client_errors: Some(Quota::per_minute(10)),
            max_keys: MAX_KEYS,
        }
    }
}

impl RateLimitConfig {
    /// No user limit: for in-process benchmarks and tests that send many
    /// requests on purpose.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            user: None,
            search: None,
            client_errors: None,
            max_keys: MAX_KEYS,
        }
    }
}

/// What a limit counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every `/api/v1` request of a user.
    User,
    /// Searches.
    Search,
    /// Client error reports.
    ClientErrors,
}

impl Scope {
    /// The `detail` of the 429 problem.
    fn detail(self) -> &'static str {
        match self {
            Self::User => "too many requests from this user",
            Self::Search => "too many searches from this user",
            Self::ClientErrors => "too many error reports from this user",
        }
    }
}

/// The scope of its own, besides [`Scope::User`], of a request to the route
/// `path` (a template) with `method` and the query string `query`.
#[must_use]
pub fn route_scope(method: &Method, path: &str, query: Option<&str>) -> Option<Scope> {
    let read = *method == Method::GET || *method == Method::HEAD;
    match path {
        "/api/v1/search" if read => Some(Scope::Search),
        "/api/v1/posts" | "/api/v1/posts/count" if read && has_text_query(query) => {
            Some(Scope::Search)
        }
        "/api/v1/client-errors" if *method == Method::POST => Some(Scope::ClientErrors),
        _ => None,
    }
}

/// Whether a list query searches text: a non-empty `q` or `concept`.
fn has_text_query(query: Option<&str>) -> bool {
    let Some(query) = query else {
        return false;
    };
    url::form_urlencoded::parse(query.as_bytes())
        .any(|(name, value)| (name == "q" || name == "concept") && !value.trim().is_empty())
}

/// A [`Quota`] per key (GCRA), in bounded memory.
pub struct Limiter {
    quota: Quota,
    /// Each key's theoretical arrival time: when its bucket is full again,
    /// in nanoseconds on the limiter's clock.
    arrivals: Cache<Key, Arc<AtomicU64>>,
}

impl Limiter {
    /// A limiter for `quota` that tracks at most `max_keys` keys.
    #[must_use]
    pub fn new(quota: Quota, max_keys: u64) -> Self {
        Self {
            quota,
            arrivals: Cache::builder()
                .max_capacity(max_keys)
                .time_to_idle(quota.refill().max(Duration::from_millis(1)))
                .eviction_policy(EvictionPolicy::lru())
                .build(),
        }
    }

    /// Counts a request of `key` now.
    ///
    /// # Errors
    ///
    /// Over the limit: how long until the request would pass. The refused
    /// request is not counted.
    pub fn check(&self, key: &Key) -> Result<(), Duration> {
        self.check_at(key, clock())
    }

    /// Counts a request of `key` at `now`, a time on a monotonic clock that
    /// starts at or after zero.
    ///
    /// # Errors
    ///
    /// Like [`Limiter::check`].
    pub fn check_at(&self, key: &Key, now: Duration) -> Result<(), Duration> {
        let period = nanos(self.quota.period);
        let tolerance = period.saturating_mul(u64::from(self.quota.burst - 1));
        let now = nanos(now);
        let arrival = self.arrivals.get_with(*key, Arc::default);
        let mut seen = arrival.load(Ordering::Acquire);
        loop {
            let start = seen.max(now);
            let ahead = start - now;
            if ahead > tolerance {
                return Err(Duration::from_nanos(ahead - tolerance));
            }
            match arrival.compare_exchange_weak(
                seen,
                start.saturating_add(period),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => seen = actual,
            }
        }
    }

    /// Keys tracked now, once the cache's pending work has run. For tests.
    #[must_use]
    pub fn tracked(&self) -> u64 {
        self.arrivals.run_pending_tasks();
        self.arrivals.entry_count()
    }
}

/// The time since the first call, on a monotonic clock.
fn clock() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// The user limiters of a server, from its [`RateLimitConfig`].
pub struct RateLimits {
    user: Option<Limiter>,
    search: Option<Limiter>,
    client_errors: Option<Limiter>,
}

impl RateLimits {
    /// The limiters of `config`.
    #[must_use]
    pub fn new(config: &RateLimitConfig) -> Self {
        let limiter = |quota: Option<Quota>| quota.map(|q| Limiter::new(q, config.max_keys));
        Self {
            user: limiter(config.user),
            search: limiter(config.search),
            client_errors: limiter(config.client_errors),
        }
    }

    /// The limiter of `scope`, if it is on.
    #[must_use]
    pub fn limiter(&self, scope: Scope) -> Option<&Limiter> {
        match scope {
            Scope::User => self.user.as_ref(),
            Scope::Search => self.search.as_ref(),
            Scope::ClientErrors => self.client_errors.as_ref(),
        }
    }

    /// Counts a request of `user_id` in `scope`.
    ///
    /// # Errors
    ///
    /// Over the limit: how long until the request would pass.
    pub fn check(&self, scope: Scope, user_id: &str) -> Result<(), Duration> {
        match self.limiter(scope) {
            Some(limiter) => limiter.check(&key("user", user_id)),
            None => Ok(()),
        }
    }
}

/// 429 `rate_limited` with `Retry-After`: `wait` in whole seconds, at least 1.
fn too_many(wait: Duration, detail: &'static str) -> Response {
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let seconds = u32::try_from(seconds).unwrap_or(u32::MAX).max(1);
    ApiError::new(ErrorCode::RateLimited)
        .with_retry_after(seconds)
        .with_detail(detail)
        .into_response()
}

/// Route layer, right after the access gate: the limits of the signed-in
/// user ([`Scope`]) on the `/api/v1` routes. The route's own scope is
/// checked first, so a refused search does not spend the user's slot.
pub async fn by_user(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let scopes = match (
        request.extensions().get::<CurrentUser>(),
        request.extensions().get::<MatchedPath>(),
    ) {
        (Some(user), Some(path)) if path.as_str().starts_with(API_PREFIX) => {
            let own = route_scope(request.method(), path.as_str(), request.uri().query());
            Some((user.id().to_owned(), own))
        }
        _ => None,
    };
    if let Some((user, own)) = scopes {
        let limits = state.rate_limits();
        for scope in own.into_iter().chain([Scope::User]) {
            if let Err(wait) = limits.check(scope, &user) {
                return too_many(wait, scope.detail());
            }
        }
    }
    next.run(request).await
}

/// Whether the limit per client address counts `method path` (a route
/// template): the `/api/v1/auth/*` routes, but [`UNCOUNTED_SIGN_IN_ROUTES`],
/// and [`COUNTED_SIGN_IN_ROUTES`].
#[must_use]
pub fn counts_as_sign_in(method: &Method, path: &str) -> bool {
    let listed = |routes: &[(Method, &str)]| routes.iter().any(|(m, p)| m == method && *p == path);
    (path.starts_with(AUTH_PREFIX) && !listed(UNCOUNTED_SIGN_IN_ROUTES))
        || listed(COUNTED_SIGN_IN_ROUTES)
}

/// Whether `response` is a 403 `reauth_required`: the session must
/// re-authenticate before the route does anything, so the request was no
/// sign-in attempt and the limit per client address takes its hit back.
#[must_use]
pub fn is_reauth_required(response: &Response) -> bool {
    response.extensions().get::<ErrorCode>() == Some(&ErrorCode::ReauthRequired)
}

/// Layer right inside the security headers: the sign-in limit per client
/// address, on the `/api/v1/auth/*` routes and the extension's pairing
/// ([`counts_as_sign_in`]). An answer 403 `reauth_required`
/// ([`is_reauth_required`]) is not counted (F10): the `/device` page that
/// approves a code while its session still has to re-authenticate would
/// otherwise spend the budget its re-authentication and approval need. That
/// answer comes from `RecentAuth`, before the route reads its body, and
/// needs a signed-in session, so it tells nothing about a code or a
/// credential.
pub async fn by_client(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let sign_in = request
        .extensions()
        .get::<MatchedPath>()
        .is_some_and(|path| counts_as_sign_in(request.method(), path.as_str()));
    if !sign_in {
        return next.run(request).await;
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let client = net::client_ip(request.headers(), peer, &state.config().trusted_proxies);
    let key = ip_key(client);
    let at = now_ms();
    if let Err(seconds) = state.auth().ip_limiter().hit(&key, at) {
        let wait = Duration::from_secs(u64::from(seconds));
        return too_many(wait, "too many sign-in requests from this address");
    }
    let response = next.run(request).await;
    if is_reauth_required(&response) {
        state.auth().ip_limiter().refund(&key, at);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn the_sign_in_limit_counts_the_auth_routes_but_the_device_poll() {
        for (method, path) in [
            (Method::GET, "/api/v1/auth/methods"),
            (Method::POST, "/api/v1/auth/device/start"),
            (Method::POST, "/api/v1/auth/device/approve"),
            (Method::GET, "/api/v1/auth/device/poll"),
        ] {
            assert!(counts_as_sign_in(&method, path), "{method} {path}");
        }
        assert!(!counts_as_sign_in(
            &Method::POST,
            "/api/v1/auth/device/poll"
        ));
        assert!(!counts_as_sign_in(&Method::GET, "/api/v1/me"));
        // The extension's pairing counts; its other routes do not.
        assert!(counts_as_sign_in(&Method::POST, "/api/v1/extension/pair"));
        assert!(!counts_as_sign_in(&Method::GET, "/api/v1/extension/pair"));
        assert!(!counts_as_sign_in(&Method::GET, "/api/v1/extension/config"));
        assert!(!counts_as_sign_in(
            &Method::POST,
            "/api/v1/me/tokens/pairing-code"
        ));
    }

    fn alice() -> Key {
        key("user", "01J9Z3B8K4QW6TFX0V7G2N5RCA")
    }

    #[test]
    fn the_plan_quotas() {
        let config = RateLimitConfig::default();
        let user = config.user.unwrap();
        assert_eq!((user.period(), user.burst_size()), (50 * MS, 60));
        let search = config.search.unwrap();
        assert_eq!((search.period(), search.burst_size()), (200 * MS, 5));
        let reports = config.client_errors.unwrap();
        assert_eq!(
            (reports.period(), reports.burst_size()),
            (Duration::from_secs(6), 10)
        );
        assert_eq!(Quota::per_hour(3).period(), Duration::from_secs(1200));
    }

    #[test]
    fn a_user_gets_60_at_once_then_20_a_second() {
        let limiter = Limiter::new(RateLimitConfig::default().user.unwrap(), MAX_KEYS);
        let t0 = Duration::from_secs(1_000);
        for i in 0..60 {
            assert_eq!(limiter.check_at(&alice(), t0), Ok(()), "request {i}");
        }
        assert_eq!(limiter.check_at(&alice(), t0), Err(50 * MS));
        assert_eq!(limiter.check_at(&alice(), t0 + 20 * MS), Err(30 * MS));
        // Refused requests were not counted: one slot every 50 ms.
        assert_eq!(limiter.check_at(&alice(), t0 + 50 * MS), Ok(()));
        assert_eq!(limiter.check_at(&alice(), t0 + 50 * MS), Err(50 * MS));
        let bob = key("user", "01J9Z3B8K4QW6TFX0V7G2N5RCB");
        assert_eq!(limiter.check_at(&bob, t0), Ok(()), "keys are separate");
        // Idle for a refill, the burst is back.
        let later = t0 + Duration::from_secs(4);
        for i in 0..60 {
            assert_eq!(limiter.check_at(&alice(), later), Ok(()), "request {i}");
        }
        assert!(limiter.check_at(&alice(), later).is_err());
    }

    #[test]
    fn searches_and_reports_have_their_own_pace() {
        let search = Limiter::new(Quota::per_second(5), MAX_KEYS);
        let t0 = Duration::from_secs(10);
        for _ in 0..5 {
            assert_eq!(search.check_at(&alice(), t0), Ok(()));
        }
        assert_eq!(search.check_at(&alice(), t0), Err(200 * MS));
        assert_eq!(search.check_at(&alice(), t0 + 200 * MS), Ok(()));

        let reports = Limiter::new(Quota::per_minute(10), MAX_KEYS);
        for _ in 0..10 {
            assert_eq!(reports.check_at(&alice(), t0), Ok(()));
        }
        assert_eq!(
            reports.check_at(&alice(), t0 + Duration::from_secs(1)),
            Err(Duration::from_secs(5))
        );
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        let response = too_many(50 * MS, "x");
        assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "1");
        let response = too_many(Duration::from_millis(5_001), "x");
        assert_eq!(response.headers()["retry-after"], "6");
        let response = too_many(Duration::from_secs(5), "x");
        assert_eq!(response.headers()["retry-after"], "5");
    }

    #[test]
    fn memory_is_bounded() {
        let limiter = Limiter::new(Quota::per_minute(10), 100);
        let t0 = Duration::from_secs(10);
        for n in 0..1_000 {
            let _ = limiter.check_at(&key("user", &n.to_string()), t0);
        }
        assert!(limiter.tracked() <= 100, "{}", limiter.tracked());
        // The newest key is tracked: a full cache does not open the limit.
        let newest = key("user", "999");
        assert_eq!(limiter.check_at(&newest, t0), Ok(()));
        for _ in 0..8 {
            let _ = limiter.check_at(&newest, t0);
        }
        assert!(limiter.check_at(&newest, t0).is_err());
    }

    #[test]
    fn routes_with_a_scope_of_their_own() {
        let get = &Method::GET;
        assert_eq!(
            route_scope(get, "/api/v1/search", Some("q=lamp")),
            Some(Scope::Search)
        );
        assert_eq!(
            route_scope(get, "/api/v1/search", None),
            Some(Scope::Search)
        );
        assert_eq!(
            route_scope(&Method::HEAD, "/api/v1/posts", Some("q=lamp")),
            Some(Scope::Search)
        );
        assert_eq!(
            route_scope(get, "/api/v1/posts/count", Some("platform=x&concept=chair")),
            Some(Scope::Search)
        );
        for query in [
            None,
            Some("q="),
            Some("q=%20+"),
            Some("tag=lamp"),
            Some("qq=1"),
        ] {
            assert_eq!(route_scope(get, "/api/v1/posts", query), None, "{query:?}");
        }
        assert_eq!(
            route_scope(&Method::POST, "/api/v1/client-errors", None),
            Some(Scope::ClientErrors)
        );
        assert_eq!(route_scope(get, "/api/v1/posts/{key}", Some("q=x")), None);
        assert_eq!(route_scope(&Method::POST, "/api/v1/search", None), None);
    }
}
