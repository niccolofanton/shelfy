//! Deny-by-default access control (plan §2.9 Auth, §7.1).
//!
//! Every route needs a signed-in session unless the router's access policy
//! says otherwise. The policy is keyed by method and route template (the
//! `MatchedPath`, such as `/api/v1/posts/{key}`):
//!
//! | Access | Who gets through | Declared in |
//! |---|---|---|
//! | [`Access::Session`] | a signed-in session: the default of every route | nothing to declare |
//! | [`Access::Public`] | anyone | [`crate::routes::PUBLIC_ROUTES`], plus `security(())` in the route's `#[utoipa::path]` |
//! | [`Access::Token`] | an API token with one of the route's scopes, and a session too when `session` is set | [`crate::routes::TOKEN_ROUTES`], plus one `("bearer" = ["<scope>"])` per scope in `security(…)`, and `("session" = [])` when sessions work too |
//!
//! A route may take several scopes (P4-08): the tus upload routes take the
//! web app's session, `uploads` tokens and the migration CLI's `migrate`
//! tokens, and the handler decides per upload purpose who may do what
//! ([`super::caller::Caller`], [`crate::control::uploads::UploadPurpose`]).
//! In the document each scope is a requirement of its own, because the
//! scopes of one requirement must all be held.
//!
//! The gate ([`gate`]) is a route layer: it runs for every route of the
//! router, after routing and before the handler and the route's limits,
//! including routes outside the OpenAPI document (`/media/{file}`). It does
//! not run for the router's fallback, which is not a route: an unknown path
//! answers 404 (and P1-09's SPA files, served as the fallback, stay public).
//! A `HEAD` request is checked as the `GET` that serves it, unless the route
//! has an explicit `HEAD` rule (tus `HEAD /api/v1/uploads/{id}`, which has no
//! `GET`). A method the route does not serve is checked like any other: an
//! anonymous `DELETE /health` answers 401, a signed-in one 405.
//!
//! On a session route the gate resolves the session cookie and inserts
//! [`CurrentUser`] and [`SessionUser`](super::SessionUser); on a token route
//! it verifies the token and its scope, records the token's use
//! ([`bearer::record_use`]) and inserts
//! [`TokenPrincipal`](super::bearer::TokenPrincipal) and [`CurrentUser`]. It
//! answers 401 (403 for a token without the scope) before the handler runs;
//! on a route that takes tokens the 401 carries `WWW-Authenticate: Bearer`,
//! sessions or not.
//! Once it knows the user, it answers 423 `user_locked` while the user's
//! library is locked for maintenance (`admin user lock`, plan §3.5), so a
//! restore never races the user's own requests.
//!
//! A [`CurrentUser`] already in the request extensions counts as a session:
//! extensions are server-side only, and a test layer that stands in for a
//! signed-in session wraps the whole application to put one there
//! (`TestState::app_as`). A layer inside the router runs after the gate and
//! cannot.
//!
//! **Adding a route.** A route that needs a session declares nothing. A public
//! route adds `(method, template)` to [`crate::routes::PUBLIC_ROUTES`] and
//! `security(())` to its `#[utoipa::path]`. A route that takes API tokens adds
//! `(method, template, scopes, sessions too)` to
//! [`crate::routes::TOKEN_ROUTES`] and its `bearer` requirements to the
//! document. The authz test in `tests/auth.rs` fails until the policy and the
//! document agree. Test-only routes get their access through
//! [`crate::app::build_with_access`].

use std::sync::Arc;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::bearer::{self, BearerRejection, ScopeSet};
use super::session;
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::ids::now_ms;
use crate::state::AppState;

/// Who may call a route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Anyone.
    Public,
    /// A signed-in session (the default).
    Session,
    /// An API token with one of `scopes`; with `session`, a signed-in
    /// session too.
    Token {
        /// The scopes the route takes: the token needs one of them.
        scopes: ScopeSet,
        /// Whether a signed-in session works too.
        session: bool,
    },
}

/// One route's access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The method (`GET` also covers `HEAD`).
    pub method: Method,
    /// The route template, as in `MatchedPath`.
    pub path: String,
    /// Who may call it.
    pub access: Access,
}

/// The access of every route that does not need a plain session.
#[derive(Clone, Debug, Default)]
pub struct AccessPolicy {
    rules: Vec<Rule>,
}

impl AccessPolicy {
    /// A policy where every route needs a session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `method path` public.
    #[must_use]
    pub fn public(self, method: Method, path: impl Into<String>) -> Self {
        self.with(method, path.into(), Access::Public)
    }

    /// Opens `method path` to API tokens with one of `scopes` (a
    /// [`Scope`](super::bearer::Scope) or a [`ScopeSet`]), and to sessions
    /// too when `session` is set.
    #[must_use]
    pub fn token(
        self,
        method: Method,
        path: impl Into<String>,
        scopes: impl Into<ScopeSet>,
        session: bool,
    ) -> Self {
        let scopes = scopes.into();
        self.with(method, path.into(), Access::Token { scopes, session })
    }

    /// Sets the access of one route; a later rule for it replaces an earlier
    /// one.
    fn with(mut self, method: Method, path: String, access: Access) -> Self {
        self.rules
            .retain(|rule| !(rule.method == method && rule.path == path));
        self.rules.push(Rule {
            method,
            path,
            access,
        });
        self
    }

    /// Who may call `method path` (a route template): its rule, or
    /// [`Access::Session`].
    #[must_use]
    pub fn access(&self, method: &Method, path: &str) -> Access {
        let rule = |method: &Method| {
            self.rules
                .iter()
                .find(|rule| rule.method == *method && rule.path == path)
                .map(|rule| rule.access)
        };
        rule(method)
            .or_else(|| {
                (*method == Method::HEAD)
                    .then(|| rule(&Method::GET))
                    .flatten()
            })
            .unwrap_or(Access::Session)
    }

    /// The rules, in the order they were added.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }
}

/// The state of [`gate`]: the application state and the policy.
#[derive(Clone)]
pub struct Gate {
    state: AppState,
    policy: Arc<AccessPolicy>,
}

impl Gate {
    /// A gate enforcing `policy`.
    #[must_use]
    pub fn new(state: AppState, policy: AccessPolicy) -> Self {
        Self {
            state,
            policy: Arc::new(policy),
        }
    }
}

/// Route layer: lets a request through only with the access its route needs
/// (see the module docs), after putting the user into the request. A user
/// whose library is locked for maintenance (`admin user lock`) gets 423
/// `user_locked` on every route that is not public.
pub async fn gate(State(gate): State<Gate>, mut request: Request, next: Next) -> Response {
    let access = match request.extensions().get::<MatchedPath>() {
        Some(path) => gate.policy.access(request.method(), path.as_str()),
        // A route layer always runs on a matched route; refuse if not.
        None => return ApiError::new(ErrorCode::Unauthorized).into_response(),
    };
    if let Err(refused) = admit(&gate.state, access, &mut request).await {
        return refused.into_response();
    }
    if let Some(user) = request.extensions().get::<CurrentUser>()
        && let Err(refused) = refuse_locked(&gate.state, user.id()).await
    {
        return refused.into_response();
    }
    next.run(request).await
}

/// 423 `user_locked` while `user_id`'s library is locked for maintenance
/// (plan §3.5): the operator is restoring it. Its open handle is released at
/// once, so the restore does not wait for the next maintenance pass.
///
/// One `stat` of the lock marker per request; the dentry is hot in the page
/// cache, so it does not need the blocking pool. Releasing the handle does:
/// it checkpoints the library and may wait for its locks (`busy_timeout`),
/// so it runs there. The 423 waits for it, so the handle is released and
/// its generation retired before the client hears back.
async fn refuse_locked(state: &AppState, user_id: &str) -> Result<(), ApiError> {
    let user_dbs = state.user_dbs();
    match user_dbs.is_locked(user_id) {
        Ok(false) => Ok(()),
        Ok(true) => {
            let (user_dbs, user) = (Arc::clone(user_dbs), user_id.to_owned());
            if let Err(err) = tokio::task::spawn_blocking(move || user_dbs.evict(&user)).await {
                tracing::warn!(error = %err, "releasing a locked library failed");
            }
            Err(ApiError::user_locked())
        }
        Err(err) => Err(ApiError::from(err)),
    }
}

/// Why the gate refused a request.
enum Refused {
    /// A problem (401 for a missing session, or a database failure).
    Problem(ApiError),
    /// A token problem: a 401 also carries `WWW-Authenticate: Bearer`.
    Token(BearerRejection),
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        match self {
            Self::Problem(err) => err.into_response(),
            Self::Token(rejection) => rejection.into_response(),
        }
    }
}

async fn admit(state: &AppState, access: Access, request: &mut Request) -> Result<(), Refused> {
    match access {
        Access::Public => Ok(()),
        Access::Session => signed_in(state, request).await,
        Access::Token { scopes, session } => {
            if request.headers().contains_key(header::AUTHORIZATION) {
                match bearer::verify(state, request.headers()).await {
                    Ok(Some(token)) if token.has_any(scopes) => {
                        bearer::record_use(state, &token).await;
                        token.attach(request.extensions_mut());
                        Ok(())
                    }
                    Ok(Some(_)) => Err(Refused::Token(BearerRejection::missing_scope(scopes))),
                    Ok(None) => Err(Refused::Token(BearerRejection::unauthorized())),
                    Err(err) => Err(Refused::Problem(err)),
                }
            } else if session {
                // No session either: the 401 names the scheme a program
                // would use, as on a token-only route.
                signed_in(state, request)
                    .await
                    .map_err(|refused| match refused {
                        Refused::Problem(err) if err.code() == ErrorCode::Unauthorized => {
                            Refused::Token(BearerRejection::unauthorized())
                        }
                        other => other,
                    })
            } else {
                Err(Refused::Token(BearerRejection::unauthorized()))
            }
        }
    }
}

/// Admits a request with a signed-in session (or a user a trusted outer
/// layer already put in).
async fn signed_in(state: &AppState, request: &mut Request) -> Result<(), Refused> {
    if request.extensions().get::<CurrentUser>().is_some() {
        return Ok(());
    }
    match session::from_cookie(state, request.headers(), now_ms()).await {
        Ok(Some(user)) => {
            session::attach(request.extensions_mut(), user);
            Ok(())
        }
        Ok(None) => Err(Refused::Problem(ApiError::new(ErrorCode::Unauthorized))),
        Err(err) => Err(Refused::Problem(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::bearer::Scope;

    #[test]
    fn routes_need_a_session_unless_the_policy_says_otherwise() {
        let uploads = [Scope::Uploads, Scope::Migrate];
        let policy = AccessPolicy::new()
            .public(Method::GET, "/health")
            .public(Method::POST, "/api/v1/auth/logout")
            .token(Method::POST, "/api/v1/migrations", Scope::Migrate, false)
            .token(Method::POST, "/api/v1/posts/lookup", Scope::Lookup, true)
            .token(Method::POST, "/api/v1/uploads", &uploads[..], true);
        assert_eq!(policy.access(&Method::GET, "/health"), Access::Public);
        assert_eq!(policy.access(&Method::HEAD, "/health"), Access::Public);
        assert_eq!(policy.access(&Method::DELETE, "/health"), Access::Session);
        assert_eq!(
            policy.access(&Method::GET, "/api/v1/auth/logout"),
            Access::Session
        );
        assert_eq!(
            policy.access(&Method::POST, "/api/v1/migrations"),
            Access::Token {
                scopes: ScopeSet::one(Scope::Migrate),
                session: false
            }
        );
        assert_eq!(
            policy.access(&Method::POST, "/api/v1/posts/lookup"),
            Access::Token {
                scopes: ScopeSet::one(Scope::Lookup),
                session: true
            }
        );
        assert_eq!(
            policy.access(&Method::POST, "/api/v1/uploads"),
            Access::Token {
                scopes: ScopeSet::of(&uploads),
                session: true
            },
            "a route may take several scopes and the session"
        );
        assert_eq!(
            policy.access(&Method::GET, "/media/{file}"),
            Access::Session
        );
        assert_eq!(
            policy.access(&Method::GET, "/health/"),
            Access::Session,
            "templates match exactly"
        );

        // A later rule replaces an earlier one for the same route.
        let policy = policy.token(Method::GET, "/health", Scope::Tasks, false);
        assert_eq!(policy.rules().len(), 5);
        assert!(matches!(
            policy.access(&Method::GET, "/health"),
            Access::Token { .. }
        ));
    }

    #[test]
    fn an_explicit_head_rule_wins_over_the_get_one() {
        let migrate = Access::Token {
            scopes: ScopeSet::one(Scope::Migrate),
            session: false,
        };
        let policy = AccessPolicy::new()
            .token(Method::HEAD, "/api/v1/uploads/{id}", Scope::Migrate, false)
            .public(Method::GET, "/health");
        assert_eq!(
            policy.access(&Method::HEAD, "/api/v1/uploads/{id}"),
            migrate
        );
        assert_eq!(
            policy.access(&Method::GET, "/api/v1/uploads/{id}"),
            Access::Session,
            "HEAD does not open GET"
        );
        assert_eq!(policy.access(&Method::HEAD, "/health"), Access::Public);
    }
}
