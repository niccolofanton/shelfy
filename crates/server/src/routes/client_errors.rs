//! `POST /api/v1/client-errors`: crash reports from the web app's error
//! boundaries (plan §2.19 Robustness, §1.2 #11).
//!
//! A report becomes one `warn` log line, in the request's span (request id,
//! user). It has technical fields only: unknown fields are refused, so a
//! report cannot carry a post, a caption or a note, and `route` is the route
//! pattern, never the URL with its query string. Free text is scrubbed
//! ([`ClientText`]: URLs other than the web app's assets, email addresses,
//! query strings and tokens go) and clipped before it reaches the log. The
//! body is capped at the default 64 KiB, and a user may send 10 reports a
//! minute ([`crate::rate_limit`]).
//!
//! Like every unsafe request, a report passes the CSRF guard
//! ([`crate::auth::csrf`]): the web app sends it with `fetch` (`keepalive:
//! true`, so it survives a page that is closing), `Origin` and
//! `X-Shelfy-Client: web`. `navigator.sendBeacon` cannot set that header.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::Json;
use crate::telemetry::redact::ClientText;

/// Characters of `message` kept in the log.
pub const MESSAGE_CHARS: usize = 1_000;
/// Characters of each stack kept in the log.
pub const STACK_CHARS: usize = 8_000;
/// Characters of `name` kept in the log.
pub const NAME_CHARS: usize = 100;
/// Longest `view`, in characters.
const MAX_VIEW_CHARS: usize = 64;
/// Longest `route`, in characters.
const MAX_ROUTE_CHARS: usize = 200;
/// Longest `clientVersion`, in characters.
const MAX_VERSION_CHARS: usize = 64;

/// An error caught by an error boundary of the web app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ClientErrorReport {
    /// The view whose boundary caught the error (`gallery`, `postModal`,
    /// `settings`): 1–64 characters of letters, digits, `.`, `_`, `-`, `:`.
    pub view: String,
    /// The error's name (`TypeError`); the first 100 characters are kept.
    #[schema(nullable = false)]
    pub name: Option<String>,
    /// The error's message; the first 1,000 characters are kept. Never put
    /// post content in it.
    pub message: String,
    /// The JavaScript stack; the first 8,000 characters are kept.
    #[schema(nullable = false)]
    pub stack: Option<String>,
    /// React's component stack; the first 8,000 characters are kept.
    #[schema(nullable = false)]
    pub component_stack: Option<String>,
    /// The route pattern of the view (`/p/:key`), never the URL: at most 200
    /// characters, starting with `/`, without `?` or `#`.
    #[schema(nullable = false)]
    pub route: Option<String>,
    /// The web app's build version: at most 64 characters of letters,
    /// digits, `.`, `+`, `-`, `_`.
    #[schema(nullable = false)]
    pub client_version: Option<String>,
    /// When it happened, by the client's clock (unix ms).
    #[schema(nullable = false)]
    pub occurred_at: Option<i64>,
}

impl ClientErrorReport {
    /// Refuses malformed identifiers (422 `validation_failed`). Free text is
    /// clipped instead, so a long stack never loses the report.
    fn validate(&self) -> Result<(), ApiError> {
        let view_ok = (1..=MAX_VIEW_CHARS).contains(&self.view.len())
            && self
                .view
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b));
        if !view_ok {
            return Err(ApiError::invalid_field(
                "view",
                "must be 1-64 characters of letters, digits, '.', '_', '-' and ':'",
            ));
        }
        if let Some(route) = &self.route {
            let route_ok = route.starts_with('/')
                && route.chars().count() <= MAX_ROUTE_CHARS
                && !route.contains(['?', '#'])
                && !route.chars().any(char::is_control);
            if !route_ok {
                return Err(ApiError::invalid_field(
                    "route",
                    "must be a route pattern: '/…', at most 200 characters, no query or fragment",
                ));
            }
        }
        if let Some(version) = &self.client_version {
            let version_ok = (1..=MAX_VERSION_CHARS).contains(&version.len())
                && version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".+-_".contains(&b));
            if !version_ok {
                return Err(ApiError::invalid_field(
                    "clientVersion",
                    "must be 1-64 characters of letters, digits, '.', '+', '-' and '_'",
                ));
            }
        }
        Ok(())
    }
}

/// Reports an error caught by the web app.
///
/// The report is logged for the operator; nothing is stored. Unknown fields
/// are refused: a report carries no post content.
#[utoipa::path(
    post,
    path = "/api/v1/client-errors",
    tag = "platform",
    operation_id = "reportClientError",
    request_body = ClientErrorReport,
    responses(
        (status = NO_CONTENT, description = "The report was logged."),
    )
)]
pub async fn report_client_error(
    _user: CurrentUser,
    Json(report): Json<ClientErrorReport>,
) -> Result<StatusCode, ApiError> {
    report.validate()?;
    let name = report.name.as_deref().map(scrubbed);
    let message = scrubbed(&report.message);
    let stack = report.stack.as_deref().map(scrubbed);
    let component_stack = report.component_stack.as_deref().map(scrubbed);
    tracing::warn!(
        view = %report.view,
        error_name = name.as_deref().map(|n| clip(n, NAME_CHARS)),
        error_message = clip(&message, MESSAGE_CHARS),
        stack = stack.as_deref().map(|s| clip(s, STACK_CHARS)),
        component_stack = component_stack.as_deref().map(|s| clip(s, STACK_CHARS)),
        client_route = report.route.as_deref(),
        client_version = report.client_version.as_deref(),
        occurred_at = report.occurred_at,
        "client error"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Free text as it may be logged ([`ClientText`]): scrubbed first, so the
/// clip never leaves part of a URL or a token behind.
fn scrubbed(text: &str) -> String {
    ClientText(text).scrubbed()
}

/// The first `max` characters of `text`.
fn clip(text: &str, max: usize) -> &str {
    text.char_indices()
        .nth(max)
        .map_or(text, |(end, _)| &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn report() -> ClientErrorReport {
        ClientErrorReport {
            view: "postModal".into(),
            message: "Cannot read properties of undefined".into(),
            ..ClientErrorReport::default()
        }
    }

    #[test]
    fn identifiers_are_checked() {
        assert!(report().validate().is_ok());
        let good = ClientErrorReport {
            route: Some("/p/:key".into()),
            client_version: Some("0.1.0-rc.1+abc".into()),
            view: "settings:account".into(),
            ..report()
        };
        assert!(good.validate().is_ok());
        let bad = [
            ClientErrorReport {
                view: String::new(),
                ..report()
            },
            ClientErrorReport {
                view: "post modal".into(),
                ..report()
            },
            ClientErrorReport {
                view: "v".repeat(65),
                ..report()
            },
            ClientErrorReport {
                route: Some("/search?q=lamp".into()),
                ..report()
            },
            ClientErrorReport {
                route: Some("p/:key".into()),
                ..report()
            },
            ClientErrorReport {
                route: Some("/p/:key#slide".into()),
                ..report()
            },
            ClientErrorReport {
                client_version: Some("1.0 beta".into()),
                ..report()
            },
        ];
        for report in bad {
            let err = report.validate().unwrap_err();
            assert_eq!(err.code(), ErrorCode::ValidationFailed, "{report:?}");
        }
    }

    #[test]
    fn clipping_respects_characters() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abcdef", 3), "abc");
        assert_eq!(clip("èèè", 2), "èè");
        assert_eq!(clip("", 0), "");
    }
}
