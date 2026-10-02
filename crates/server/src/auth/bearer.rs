//! API tokens (plan §2.9 Auth, §2.11 device tokens): bearer requests from the
//! extension, the iOS Shortcut and the migration CLI.
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
//! ([`crate::routes::TOKEN_ROUTES`], with the scope it needs): the gate
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
use crate::control::api_tokens;
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
}

impl Scope {
    /// Every scope, in the order `api_tokens.scopes` lists them.
    pub const ALL: [Self; 6] = [
        Self::Ingest,
        Self::Tasks,
        Self::Uploads,
        Self::Lookup,
        Self::LinksCreate,
        Self::Migrate,
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
        }
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

    scope!(Ingest, Tasks, Uploads, Lookup, LinksCreate, Migrate);
}

/// A verified API token, inserted by the gate on token routes.
#[derive(Clone, Debug)]
pub struct TokenPrincipal {
    user_id: Arc<str>,
    token_id: String,
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

    /// Whether the token has `scope`.
    #[must_use]
    pub fn has(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
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

    /// 403: the token lacks `scope`.
    #[must_use]
    pub fn missing_scope(scope: Scope) -> Self {
        Self(
            ApiError::new(ErrorCode::Forbidden)
                .with_detail(format!("the token lacks the {} scope", scope.as_str())),
        )
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
