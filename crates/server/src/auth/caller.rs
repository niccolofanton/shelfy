//! The caller of a route that takes a session and API tokens (plan §2.9
//! Auth; P4-08): which credential the access gate admitted.
//!
//! Most routes take one kind of credential, and their handlers take
//! [`CurrentUser`] or [`TokenUser`](super::bearer::TokenUser). A route in
//! [`crate::routes::TOKEN_ROUTES`] with sessions too (the tus uploads, `POST
//! /posts/lookup`) may need to know which one came, because what the caller
//! may do depends on it: a session or an `uploads` token may upload a
//! bookmark, only a `migrate` token a migration bundle. [`Caller`] says so.
//!
//! The gate ([`super::access::gate`]) puts a [`TokenPrincipal`] into the
//! request when it admitted a token, and a [`CurrentUser`] in every case. So
//! a request with a [`CurrentUser`] and no [`TokenPrincipal`] came with a
//! session (or through a trusted test layer that stands in for one): a
//! request with an `Authorization` header never authenticates with its
//! cookie.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use super::bearer::{ScopeSet, TokenPrincipal};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};

/// Who calls a route, and with what: a signed-in session, or an API token
/// and its scopes. Without a user (a route the gate did not admit anyone
/// to), 401 `unauthorized`.
#[derive(Clone, Debug)]
pub struct Caller {
    user: CurrentUser,
    token: Option<TokenPrincipal>,
}

impl Caller {
    /// A caller signed in with a session.
    #[must_use]
    pub fn session(user: CurrentUser) -> Self {
        Self { user, token: None }
    }

    /// A caller with an API token.
    #[must_use]
    pub fn with_token(token: TokenPrincipal) -> Self {
        Self {
            user: CurrentUser::new(token.user_id()),
            token: Some(token),
        }
    }

    /// The user the request acts for.
    #[must_use]
    pub fn id(&self) -> &str {
        self.user.id()
    }

    /// The API token, when the caller sent one.
    #[must_use]
    pub fn api_token(&self) -> Option<&TokenPrincipal> {
        self.token.as_ref()
    }

    /// Whether the caller signed in with a session.
    #[must_use]
    pub fn is_session(&self) -> bool {
        self.token.is_none()
    }

    /// Whether the caller's credential is one of these: a session when
    /// `session` is set, or a token holding one of `scopes`.
    #[must_use]
    pub fn is_one_of(&self, session: bool, scopes: ScopeSet) -> bool {
        match &self.token {
            None => session,
            Some(token) => token.has_any(scopes),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        let Some(user) = parts.extensions.get::<CurrentUser>().cloned() else {
            return Err(ApiError::new(ErrorCode::Unauthorized));
        };
        let token = parts.extensions.get::<TokenPrincipal>().cloned();
        Ok(Self { user, token })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request;

    use super::*;
    use crate::auth::bearer::Scope;

    #[tokio::test]
    async fn the_credential_comes_from_what_the_gate_put_in() {
        let (mut parts, ()) = Request::get("/").body(()).unwrap().into_parts();
        let err = Caller::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unauthorized);

        parts.extensions.insert(CurrentUser::new("U1"));
        let session = Caller::from_request_parts(&mut parts, &()).await.unwrap();
        assert_eq!(session.id(), "U1");
        assert!(session.is_session() && session.api_token().is_none());

        TokenPrincipal::for_tests("U1", &[Scope::Uploads]).attach(&mut parts.extensions);
        let token = Caller::from_request_parts(&mut parts, &()).await.unwrap();
        assert_eq!(token.id(), "U1");
        assert!(!token.is_session());
        assert!(token.api_token().is_some_and(|t| t.has(Scope::Uploads)));
    }

    #[test]
    fn a_session_counts_where_sessions_do_and_a_token_by_its_scopes() {
        let migrate = ScopeSet::one(Scope::Migrate);
        let either = ScopeSet::of(&[Scope::Uploads, Scope::Migrate]);
        let session = Caller::session(CurrentUser::new("U1"));
        assert!(session.is_one_of(true, migrate));
        assert!(!session.is_one_of(false, either));

        let uploads = Caller::with_token(TokenPrincipal::for_tests("U1", &[Scope::Uploads]));
        assert_eq!(uploads.id(), "U1");
        assert!(uploads.is_one_of(false, either));
        assert!(!uploads.is_one_of(true, migrate), "a token is no session");
        assert!(!uploads.is_one_of(true, ScopeSet::NONE));
    }
}
