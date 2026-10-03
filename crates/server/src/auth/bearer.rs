//! API tokens (plan §2.9 Auth, §2.11 device tokens): bearer requests from the
//! extension, the iOS Shortcut, migration CLI and library API clients.
//!
//! This module verifies tokens; [`super::api_tokens`] mints them (`POST
//! /me/tokens`, the device flow of [`super::device`], `admin
//! migrate-token`), and the account lists and revokes them (`GET,DELETE
//! /me/tokens`).
//!
//! A request sends `Authorization: Bearer shx_<43 characters>`; the SHA-256
//! of that whole value must match an unrevoked, unexpired token of an active
//! user (`api_tokens.expires_at`, control schema v2). A
//! route accepts tokens only when the access policy says so
//! ([`crate::routes::TOKEN_ROUTES`], with the scopes it takes, a
//! [`ScopeSet`]: one of them is enough): the gate
//! ([`super::access::gate`]) then verifies the token, checks the scope,
//! records the use ([`record_use`]: `last_used_at`, at most once per
//! [`super::AuthConfig::token_touch_every`]) and inserts [`TokenPrincipal`]
//! and [`CurrentUser`] into the request. On every
//! other route a token is refused, even next to a valid session cookie, and
//! no request with an `Authorization` header authenticates with its cookies.
//! Token requests skip the cookie CSRF check ([`super::csrf`]). Tokens are
//! not cached: a revocation takes effect with the next request.
//!
//! Handlers take [`CurrentUser`] (the user's data) or [`TokenUser<S>`] (the
//! token, with scope `S` checked again at the type level).

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::millis;
use crate::control::api_tokens::{self, TokenKind};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::ids::now_ms;
use crate::state::{AppState, blocking};
use crate::tokens::{hash_token, is_token_shaped};

/// Prefix of every API token, so leaked tokens are easy to recognize.
pub const TOKEN_PREFIX: &str = "shx_";

/// What a token may do (§2.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[schema(as = TokenScope)]
pub enum Scope {
    /// `POST /ingest/batches`, sync runs.
    #[serde(rename = "ingest")]
    Ingest,
    /// `GET /ingest/tasks`, task completion.
    #[serde(rename = "tasks")]
    Tasks,
    /// tus uploads.
    #[serde(rename = "uploads")]
    Uploads,
    /// `POST /posts/lookup`.
    #[serde(rename = "lookup")]
    Lookup,
    /// `POST /links` (the iOS Shortcut).
    #[serde(rename = "links:create")]
    LinksCreate,
    /// The migration routes (the CLI).
    #[serde(rename = "migrate")]
    Migrate,
    /// Read the user's posts, collections, search, stats, trash and media.
    #[serde(rename = "library:read")]
    LibraryRead,
    /// Edit the user's library. Does not grant `library:read`.
    #[serde(rename = "library:write")]
    LibraryWrite,
}

impl Scope {
    /// Every scope, in the order `api_tokens.scopes` lists them.
    pub const ALL: [Self; 8] = [
        Self::Ingest,
        Self::Tasks,
        Self::Uploads,
        Self::Lookup,
        Self::LinksCreate,
        Self::Migrate,
        Self::LibraryRead,
        Self::LibraryWrite,
    ];

    /// The scope named `name` in `api_tokens.scopes`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.as_str() == name)
    }

    /// The scopes of a stored `api_tokens.scopes`; unknown names are
    /// skipped.
    #[must_use]
    pub fn parse_list(stored: &str) -> Vec<Self> {
        stored
            .split_ascii_whitespace()
            .filter_map(Self::parse)
            .collect()
    }

    /// `scopes` as `api_tokens.scopes` stores them: space-separated, in the
    /// order of [`Scope::ALL`], each once.
    #[must_use]
    pub fn list(scopes: &[Self]) -> String {
        Self::ALL
            .into_iter()
            .filter(|scope| scopes.contains(scope))
            .map(Self::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The scope's name in `api_tokens.scopes`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Tasks => "tasks",
            Self::Uploads => "uploads",
            Self::Lookup => "lookup",
            Self::LinksCreate => "links:create",
            Self::Migrate => "migrate",
            Self::LibraryRead => "library:read",
            Self::LibraryWrite => "library:write",
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// A set of scopes. A route that takes tokens names the scopes it accepts,
/// and a token holding any one of them gets in
/// ([`crate::routes::TOKEN_ROUTES`]): the tus upload routes take the
/// web app's `uploads` and the migration CLI's `migrate`, and the handler
/// matches the scope to what is uploaded (P4-08).
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScopeSet(u16);

impl ScopeSet {
    /// No scope.
    pub const NONE: Self = Self(0);

    /// The set of `scopes`.
    #[must_use]
    pub const fn of(scopes: &[Scope]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < scopes.len() {
            bits |= scopes[i].bit();
            i += 1;
        }
        Self(bits)
    }

    /// The set of one scope.
    #[must_use]
    pub const fn one(scope: Scope) -> Self {
        Self(scope.bit())
    }

    /// This set plus every scope of `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `scope` is in the set.
    #[must_use]
    pub const fn contains(self, scope: Scope) -> bool {
        self.0 & scope.bit() != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The scopes, in the order of [`Scope::ALL`].
    pub fn iter(self) -> impl Iterator<Item = Scope> {
        Scope::ALL
            .into_iter()
            .filter(move |scope| self.contains(*scope))
    }
}

impl From<Scope> for ScopeSet {
    fn from(scope: Scope) -> Self {
        Self::one(scope)
    }
}

impl From<&[Scope]> for ScopeSet {
    fn from(scopes: &[Scope]) -> Self {
        Self::of(scopes)
    }
}

impl fmt::Debug for ScopeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set()
            .entries(self.iter().map(Scope::as_str))
            .finish()
    }
}

impl fmt::Display for ScopeSet {
    /// The names, separated by `, `.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, scope) in self.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            f.write_str(scope.as_str())?;
        }
        Ok(())
    }
}

/// A scope required at the type level, by [`TokenUser`].
pub trait RequiredScope: Send + Sync + 'static {
    /// The scope.
    const SCOPE: Scope;
}

/// The marker types of [`RequiredScope`].
pub mod scopes {
    use super::{RequiredScope, Scope};

    macro_rules! scope {
        ($($name:ident),* $(,)?) => {$(
            #[doc = concat!("Requires [`Scope::", stringify!($name), "`].")]
            #[derive(Clone, Copy, Debug)]
            pub struct $name;

            impl RequiredScope for $name {
                const SCOPE: Scope = Scope::$name;
            }
        )*};
    }

    scope!(
        Ingest,
        Tasks,
        Uploads,
        Lookup,
        LinksCreate,
        Migrate,
        LibraryRead,
        LibraryWrite
    );
}

/// A verified API token, inserted by the gate on token routes.
#[derive(Clone, Debug)]
pub struct TokenPrincipal {
    user_id: Arc<str>,
    token_id: String,
    kind: TokenKind,
    scopes: Vec<Scope>,
    last_used_at: Option<i64>,
}

impl TokenPrincipal {
    /// The user the token acts for.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The token's id.
    #[must_use]
    pub fn token_id(&self) -> &str {
        &self.token_id
    }

    /// Who holds the token. An `extension` token's requests also mark the
    /// extension's presence and pass its version gate
    /// ([`crate::extension::seen`], [`crate::extension::admit`]).
    #[must_use]
    pub fn kind(&self) -> TokenKind {
        self.kind
    }

    /// Whether the token has `scope`.
    #[must_use]
    pub fn has(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Whether the token has at least one scope of `scopes`.
    #[must_use]
    pub fn has_any(&self, scopes: ScopeSet) -> bool {
        self.scopes.iter().any(|scope| scopes.contains(*scope))
    }

    /// A verified token of `user_id` with `scopes`, for unit tests: of kind
    /// `migrate` when it has that scope, `library` for library scopes,
    /// `extension` otherwise.
    #[cfg(test)]
    pub(crate) fn for_tests(user_id: &str, scopes: &[Scope]) -> Self {
        let kind = if scopes.contains(&Scope::Migrate) {
            TokenKind::Migrate
        } else if scopes.contains(&Scope::LibraryRead) || scopes.contains(&Scope::LibraryWrite) {
            TokenKind::Library
        } else {
            TokenKind::Extension
        };
        Self {
            user_id: user_id.into(),
            token_id: "T".to_owned(),
            kind,
            scopes: scopes.to_vec(),
            last_used_at: None,
        }
    }

    /// Puts the token into the request: [`TokenPrincipal`] and
    /// [`CurrentUser`] in the extensions, the user id in the request span.
    pub fn attach(self, extensions: &mut Extensions) {
        tracing::Span::current().record("user_id", self.user_id());
        extensions.insert(CurrentUser::new(self.user_id()));
        extensions.insert(self);
    }
}

/// The token of `Authorization: Bearer shx_…`, if well formed.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    let secret = token.strip_prefix(TOKEN_PREFIX)?;
    (scheme.eq_ignore_ascii_case("bearer") && is_token_shaped(secret)).then_some(token)
}

/// The token of the request's `Authorization` header, if it is a well-formed
/// bearer token of an active user, not revoked and not expired.
///
/// # Errors
///
/// The control database failed.
pub async fn verify(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<TokenPrincipal>, ApiError> {
    let Some(token) = bearer_token(headers) else {
        return Ok(None);
    };
    let token_hash = hash_token(token);
    let control = Arc::clone(state.control());
    let now = now_ms();
    let found =
        blocking(move || control.read(|conn| api_tokens::find_active(conn, &token_hash, now)))
            .await?;
    Ok(found.map(|token| TokenPrincipal {
        user_id: token.user_id.into(),
        scopes: Scope::parse_list(&token.scopes),
        // The schema's CHECK constraint admits only known kinds; an unknown
        // one would get the strictest treatment, the extension's.
        kind: TokenKind::parse(&token.kind).unwrap_or(TokenKind::Extension),
        token_id: token.id,
        last_used_at: token.last_used_at,
    }))
}

/// Records that `token` authenticated a request now (`last_used_at`, shown
/// in the account's token list), unless its last recorded use is more recent
/// than [`super::AuthConfig::token_touch_every`]. A failed write is logged
/// and does not fail the request.
pub async fn record_use(state: &AppState, token: &TokenPrincipal) {
    let now = now_ms();
    let every = millis(state.auth().config().token_touch_every);
    if token
        .last_used_at
        .is_some_and(|at| now.saturating_sub(at) < every)
    {
        return;
    }
    let control = Arc::clone(state.control());
    let id = token.token_id.clone();
    let written = blocking(move || control.write(|tx| api_tokens::touch(tx, &id, now))).await;
    if let Err(err) = written {
        tracing::warn!(error = %err, "recording an API token's use failed");
    }
}

/// The user of a bearer request whose token has scope `S`. Without a
/// verified token (the route does not accept tokens, or the request sent
/// none), 401 with `WWW-Authenticate: Bearer`; without scope `S`, 403.
pub struct TokenUser<S> {
    user_id: Arc<str>,
    token_id: String,
    _scope: PhantomData<fn() -> S>,
}

impl<S> TokenUser<S> {
    /// The user the token acts for.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.user_id
    }

    /// The token's id.
    #[must_use]
    pub fn token_id(&self) -> &str {
        &self.token_id
    }
}

impl<S> Clone for TokenUser<S> {
    fn clone(&self) -> Self {
        Self {
            user_id: Arc::clone(&self.user_id),
            token_id: self.token_id.clone(),
            _scope: PhantomData,
        }
    }
}

impl<S> fmt::Debug for TokenUser<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenUser")
            .field("user_id", &self.user_id)
            .field("token_id", &self.token_id)
            .finish()
    }
}

/// The rejection of token checks: a problem, plus `WWW-Authenticate: Bearer`
/// on a 401 (RFC 6750).
#[derive(Debug)]
pub struct BearerRejection(pub ApiError);

impl BearerRejection {
    /// 401: no usable token.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self(ApiError::new(ErrorCode::Unauthorized))
    }

    /// 403: the token lacks `scopes` (one scope, or every scope of a set).
    #[must_use]
    pub fn missing_scope(scopes: impl Into<ScopeSet>) -> Self {
        let scopes = scopes.into();
        let detail = match scopes.iter().count() {
            1 => format!("the token lacks the {scopes} scope"),
            _ => format!("the token lacks every scope this route takes: {scopes}"),
        };
        Self(ApiError::new(ErrorCode::Forbidden).with_detail(detail))
    }
}

impl IntoResponse for BearerRejection {
    fn into_response(self) -> Response {
        let unauthorized = self.0.code() == ErrorCode::Unauthorized;
        let mut response = self.0.into_response();
        if unauthorized {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

impl From<ApiError> for BearerRejection {
    fn from(err: ApiError) -> Self {
        Self(err)
    }
}

impl<S: RequiredScope, St: Send + Sync> FromRequestParts<St> for TokenUser<S> {
    type Rejection = BearerRejection;

    async fn from_request_parts(parts: &mut Parts, _state: &St) -> Result<Self, BearerRejection> {
        let Some(token) = parts.extensions.get::<TokenPrincipal>() else {
            return Err(BearerRejection::unauthorized());
        };
        if !token.has(S::SCOPE) {
            return Err(BearerRejection::missing_scope(S::SCOPE));
        }
        Ok(Self {
            user_id: Arc::clone(&token.user_id),
            token_id: token.token_id.clone(),
            _scope: PhantomData,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_round_trip() {
        for scope in Scope::ALL {
            assert_eq!(Scope::parse(scope.as_str()), Some(scope));
            assert_eq!(serde_json::to_value(scope).unwrap(), scope.as_str());
        }
        assert_eq!(Scope::parse("admin"), None);
        assert_eq!(Scope::parse("Ingest"), None, "names are exact");
    }

    #[test]
    fn scope_sets_hold_each_scope_once() {
        let uploads = ScopeSet::of(&[Scope::Migrate, Scope::Uploads, Scope::Migrate]);
        assert!(uploads.contains(Scope::Uploads) && uploads.contains(Scope::Migrate));
        assert!(!uploads.contains(Scope::Ingest));
        assert_eq!(
            uploads.iter().collect::<Vec<_>>(),
            [Scope::Uploads, Scope::Migrate]
        );
        assert_eq!(uploads.to_string(), "uploads, migrate");
        assert_eq!(format!("{uploads:?}"), r#"{"uploads", "migrate"}"#);
        assert_eq!(ScopeSet::from(Scope::Lookup), ScopeSet::one(Scope::Lookup));
        assert_eq!(
            ScopeSet::one(Scope::Uploads).union(Scope::Migrate.into()),
            uploads
        );
        assert!(ScopeSet::NONE.is_empty() && !uploads.is_empty());
        assert_eq!(ScopeSet::of(&Scope::ALL).iter().count(), Scope::ALL.len());

        let token = TokenPrincipal::for_tests("U", &[Scope::Ingest, Scope::Uploads]);
        assert!(token.has_any(uploads));
        assert!(!token.has_any(ScopeSet::one(Scope::Migrate)));
        assert!(!token.has_any(ScopeSet::NONE));

        let one = BearerRejection::missing_scope(Scope::Lookup).0;
        assert_eq!(
            one.problem().detail.as_deref(),
            Some("the token lacks the lookup scope")
        );
        let several = BearerRejection::missing_scope(uploads).0;
        assert!(
            several
                .problem()
                .detail
                .unwrap()
                .ends_with("uploads, migrate")
        );
    }

    #[test]
    fn scope_lists_are_canonical() {
        assert_eq!(
            Scope::list(&[Scope::Lookup, Scope::Ingest, Scope::Lookup]),
            "ingest lookup"
        );
        assert_eq!(Scope::list(&[]), "");
        assert_eq!(
            Scope::parse_list("lookup  links:create bogus ingest"),
            [Scope::Lookup, Scope::LinksCreate, Scope::Ingest]
        );
    }

    #[test]
    fn only_well_formed_bearer_tokens_are_read() {
        let secret = crate::tokens::SecretToken::generate();
        let token = format!("shx_{}", secret.expose());
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value.parse().unwrap());
            headers
        };
        assert_eq!(
            bearer_token(&with(&format!("Bearer {token}"))),
            Some(token.as_str())
        );
        assert_eq!(
            bearer_token(&with(&format!("bearer  {token} "))),
            Some(token.as_str())
        );
        for bad in [
            format!("Basic {token}"),
            format!("Bearer {}", secret.expose()),
            "Bearer shx_short".to_owned(),
            "Bearer".to_owned(),
        ] {
            assert_eq!(bearer_token(&with(&bad)), None, "{bad}");
        }
        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }
}
