//! API tokens (plan §2.9 Auth, §2.11 device tokens): bearer requests from the
//! extension, the iOS Shortcut and the migration CLI.
//!
//! **P1-17 mints them.** This module verifies tokens; nothing creates them
//! yet. P1-17 adds minting (shown once), the pairing and device-code flows,
//! `last_used_at` and revocation.
//!
//! A request sends `Authorization: Bearer shx_<43 characters>`; the SHA-256
//! of that whole value must match an unrevoked token of an active user. A
//! route accepts tokens only when the access policy says so
//! ([`crate::routes::TOKEN_ROUTES`], with the scope it needs): the gate
//! ([`super::access::gate`]) then verifies the token, checks the scope and
//! inserts [`TokenPrincipal`] and [`CurrentUser`] into the request. On every
//! other route a token is refused, even next to a valid session cookie, and
//! no request with an `Authorization` header authenticates with its cookies.
//! Token requests skip the cookie CSRF check ([`super::csrf`]).
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

use crate::control::api_tokens;
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::state::{AppState, blocking};
use crate::tokens::{hash_token, is_token_shaped};

/// Prefix of every API token, so leaked tokens are easy to recognize.
pub const TOKEN_PREFIX: &str = "shx_";

/// What a token may do (§2.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `POST /ingest/batches`, sync runs.
    Ingest,
    /// `GET /ingest/tasks`, task completion.
    Tasks,
    /// tus uploads.
    Uploads,
    /// `POST /posts/lookup`.
    Lookup,
    /// `POST /links` (the iOS Shortcut).
    LinksCreate,
    /// The migration routes (the CLI).
    Migrate,
}

impl Scope {
    /// The scope named `name` in `api_tokens.scopes`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Ingest,
            Self::Tasks,
            Self::Uploads,
            Self::Lookup,
            Self::LinksCreate,
            Self::Migrate,
        ]
        .into_iter()
        .find(|scope| scope.as_str() == name)
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
/// bearer token of an active user and not revoked.
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
    let found =
        blocking(move || control.read(|conn| api_tokens::find_active(conn, &token_hash))).await?;
    Ok(found.map(|token| TokenPrincipal {
        user_id: token.user_id.into(),
        scopes: token
            .scopes
            .split_ascii_whitespace()
            .filter_map(Scope::parse)
            .collect(),
        token_id: token.id,
    }))
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
        for scope in [
            Scope::Ingest,
            Scope::Tasks,
            Scope::Uploads,
            Scope::Lookup,
            Scope::LinksCreate,
            Scope::Migrate,
        ] {
            assert_eq!(Scope::parse(scope.as_str()), Some(scope));
        }
        assert_eq!(Scope::parse("admin"), None);
        assert_eq!(Scope::parse("Ingest"), None, "names are exact");
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
