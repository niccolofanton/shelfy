//! API tokens (plan §2.9 Auth, §2.11 device tokens): the typed extractor of
//! bearer requests from the extension, the iOS Shortcut and the migration CLI.
//!
//! **Stub for P1-17.** The extractor reads tokens; nothing creates them yet.
//! P1-17 adds minting (shown once), the pairing and device-code flows,
//! `last_used_at`, revocation and the routes that accept tokens.
//!
//! A route states the scope it needs in its signature: `TokenUser<Ingest>`.
//! The request must send `Authorization: Bearer shx_<43 characters>`; the
//! SHA-256 of that whole value must match an unrevoked token of an active
//! user that has the scope. A token request never authenticates with
//! cookies and is not subject to the cookie CSRF check ([`super::csrf`]).
//!
//! The authentication layer ([`super::session::authenticate`]) inserts a
//! [`CurrentUser`](crate::current_user::CurrentUser) only for a cookie
//! session, and never for a request with an `Authorization` header, so a
//! token can never call a cookie route. A route that accepts both (P1-17:
//! `POST /posts/lookup` takes the cookie or a `lookup` token) takes an
//! extractor that tries the `CurrentUser` first, then `TokenUser<S>`.

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};

use crate::control::api_tokens;
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

/// The user of a bearer request whose token has scope `S`.
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

/// The rejection of [`TokenUser`]: a problem, plus `WWW-Authenticate:
/// Bearer` on a 401 (RFC 6750).
#[derive(Debug)]
pub struct BearerRejection(pub ApiError);

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

/// The token of `Authorization: Bearer shx_…`, if well formed.
fn bearer_token(parts: &Parts) -> Option<&str> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    let secret = token.strip_prefix(TOKEN_PREFIX)?;
    (scheme.eq_ignore_ascii_case("bearer") && is_token_shaped(secret)).then_some(token)
}

impl<S: RequiredScope> FromRequestParts<AppState> for TokenUser<S> {
    type Rejection = BearerRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, BearerRejection> {
        let Some(token) = bearer_token(parts) else {
            return Err(ApiError::new(ErrorCode::Unauthorized).into());
        };
        let token_hash = hash_token(token);
        let control = Arc::clone(state.control());
        let found =
            blocking(move || control.read(|conn| api_tokens::find_active(conn, &token_hash)))
                .await?;
        let Some(found) = found else {
            return Err(ApiError::new(ErrorCode::Unauthorized).into());
        };
        if !found
            .scopes
            .split_ascii_whitespace()
            .any(|scope| scope == S::SCOPE.as_str())
        {
            return Err(ApiError::new(ErrorCode::Forbidden)
                .with_detail(format!("the token lacks the {} scope", S::SCOPE.as_str()))
                .into());
        }
        tracing::Span::current().record("user_id", found.user_id.as_str());
        Ok(Self {
            user_id: found.user_id.into(),
            token_id: found.id,
            _scope: PhantomData,
        })
    }
}
