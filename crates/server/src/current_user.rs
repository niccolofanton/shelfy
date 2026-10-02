//! The signed-in user of a request: the seam between authentication (T10) and
//! the routes that act on a user's data.
//!
//! The contract:
//!
//! - the access gate ([`crate::auth::access`]) runs before the handler and,
//!   once it has verified the credential the route accepts (the session
//!   cookie, or a scoped API token on a token route), inserts a
//!   [`CurrentUser`] into the request extensions. Without one, a route that is
//!   not public answers 401 `unauthorized` at the gate, before the handler
//!   runs and before any database is opened;
//! - a route that needs a user takes [`CurrentUser`] as an extractor, which
//!   answers 401 too when the extensions hold none.
//!
//! Request extensions are server-side only: a client cannot set one through a
//! header, so the gate and the extractor trust what they find. Tests that
//! stand in for a signed-in session insert one with a layer of their own
//! (`TestState::app_as` in `tests/support/library.rs`); such a layer must wrap
//! the built application, because a layer inside the router runs after the
//! gate. It is not part of a build.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::error::{ApiError, ErrorCode};

/// The user a request acts for. Routes address only this user's library, so
/// another user's resource is indistinguishable from a missing one (404).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentUser {
    id: String,
}

impl CurrentUser {
    /// The user with this id (a ULID, `users.id`). Only the authentication
    /// layer creates one, after verifying the credential.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    /// The user's id; it names the user's directory under `users/`.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request;

    use super::*;

    #[tokio::test]
    async fn the_user_comes_from_the_extensions_only() {
        let (mut parts, ()) = Request::get("/").body(()).unwrap().into_parts();
        let err = CurrentUser::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unauthorized);

        parts
            .extensions
            .insert(CurrentUser::new("01J9Z3B8K4QW6TFX0V7G2N5RCE"));
        let user = CurrentUser::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(user.id(), "01J9Z3B8K4QW6TFX0V7G2N5RCE");
    }
}
