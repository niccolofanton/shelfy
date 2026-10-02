//! The security of the OpenAPI document (plan §2.9 Auth).
//!
//! Secure by default: the document's top-level `security` is the session
//! cookie, so every operation needs a signed-in session unless it opts out.
//! A public route declares `security(())` in its `#[utoipa::path]`; a route
//! that also accepts tokens (P1-17) declares
//! `security(("session" = []), ("bearer" = ["lookup"]))`. The authz test in
//! `tests/auth.rs` checks the document against the server's behavior.

use utoipa::Modify;
use utoipa::openapi::security::{
    ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme,
};
use utoipa::openapi::{Components, OpenApi};

use super::cookie::SESSION_COOKIE;

/// Name of the session-cookie scheme.
pub const SESSION_SCHEME: &str = "session";
/// Name of the API-token scheme.
pub const BEARER_SCHEME: &str = "bearer";

/// Adds the `session` and `bearer` schemes to `components.securitySchemes`
/// and makes the session the default requirement.
#[derive(Clone, Copy, Debug)]
pub struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut OpenApi) {
        let components = openapi.components.get_or_insert_with(Components::new);
        components.add_security_scheme(
            SESSION_SCHEME,
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                SESSION_COOKIE,
                "The web app's session, set by signing in. Requests that change state \
                 (POST, PUT, PATCH, DELETE) with this cookie must also send \
                 `X-Shelfy-Client: web` and an `Origin` equal to the public URL; otherwise \
                 they fail with 403 `csrf_failed`. A request with an `Authorization` header \
                 is never authenticated by this cookie.",
            ))),
        );
        components.add_security_scheme(
            BEARER_SCHEME,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("shx_")
                    .description(Some(
                        "A scoped API token (`shx_…`) of the extension, the iOS Shortcut or \
                         the migration CLI. Tokens cannot call routes that only accept the \
                         session.",
                    ))
                    .build(),
            ),
        );
        openapi.security = Some(vec![SecurityRequirement::new(
            SESSION_SCHEME,
            Vec::<String>::new(),
        )]);
    }
}
