//! The router composition and the OpenAPI document (plan §2.9, D6).
//!
//! Routes are registered in groups by their limits ([`RouteLimits`]); each
//! handler carries a `#[utoipa::path]`, so the document is generated from the
//! code that serves it. A new route goes into its group here:
//!
//! | Group | Limits | Routes |
//! |---|---|---|
//! | `standard` | 64 KiB, 30 s | everything JSON: health, OpenAPI, auth, account; the read API (T11), library; notifications, client errors, version (P1-01) |
//! | `streams` | 64 KiB, no time limit | `GET /api/v1/events` (P1-01), `POST /api/v1/search/chat` (P3) |
//! | `media` | 64 KiB, 30 s until the headers | `GET /media/{file}`, outside `/api` and the document ([`media`]) |
//! | ingest, uploads, STT | [`RouteLimits::INGEST`], [`RouteLimits::UPLOAD_CHUNK`], [`RouteLimits::STT`] | added with their routes (P2, T9, P3) |
//!
//! The read API (T11): [`posts`] (`GET /posts`, `GET /posts/{key}`),
//! [`search`], [`stats`] and [`collections`], all behind
//! [`CurrentUser`](crate::current_user::CurrentUser) and conditional
//! ([`crate::conditional`]); their JSON shapes are in [`model`] and the shared
//! paging in [`listing`].
//!
//! The platform routes (P1-01): [`events`] (the SSE stream of
//! [`crate::events`]), [`notifications`], [`client_errors`] and [`version`],
//! all behind [`CurrentUser`](crate::current_user::CurrentUser).
//!
//! The committed copy of the document, `crates/server/openapi.json`, is what
//! the TypeScript client is generated from (T11). After changing a route,
//! regenerate it with
//! `UPDATE_OPENAPI=1 cargo test -p shelfy-server --test openapi`, then the
//! client with `pnpm exec tsx scripts/api-client/generate.ts`; both checks
//! fail while a committed copy is stale.

pub mod auth;
pub mod client_errors;
pub mod collections;
pub mod docs;
pub mod events;
pub mod health;
pub mod listing;
pub mod me;
pub mod media;
pub mod model;
pub mod notifications;
pub mod posts;
pub mod search;
pub mod stats;
pub mod version;

use utoipa::OpenApi;
use utoipa::openapi::path::Operation;
use utoipa::openapi::{
    Components, ContentBuilder, OpenApi as OpenApiDoc, Ref, RefOr, ResponseBuilder,
};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::openapi::SecuritySchemes;
use crate::error::{ErrorCode, FieldError, PROBLEM_JSON, Problem};
use crate::events::model as event;
use crate::limits::RouteLimits;
use crate::state::AppState;

/// Version of the API description. The URL prefix `/api/v1` carries the
/// major version; additive changes keep it.
pub const API_VERSION: &str = "1";

/// Name of the shared error response in `components.responses`.
const PROBLEM_RESPONSE: &str = "Problem";

/// The base of the document: metadata and the shared components.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Shelfy API",
        version = API_VERSION,
        description = "HTTP API of Shelfy Web. JSON in camelCase, timestamps in unix \
                       milliseconds. Errors are `application/problem+json` documents whose \
                       `code` is stable; clients map it to their own messages."
    ),
    // Responses register their schemas; types used only by parameters (the
    // filter enums) and the event payloads of the stream are listed here.
    components(schemas(
        Problem,
        ErrorCode,
        FieldError,
        event::ServerEvent,
        event::EventTopic,
        event::HelloEvent,
        event::ResyncEvent,
        event::ResyncReason,
        event::PostsChangedEvent,
        event::ChangeReason,
        event::StatsChangedEvent,
        event::JobUpdatedEvent,
        event::JobState,
        event::Notification,
        listing::MatchMode,
        listing::PostSort,
        listing::YesNo,
        posts::PostSource,
        search::SearchScope,
    )),
    modifiers(&SecuritySchemes),
    tags(
        (name = "platform", description = "Health, the API description, the realtime stream, \
                                           notifications, client error reports and the version."),
        (name = "auth", description = "Sign-in links, sessions and sign-out."),
        (name = "account", description = "The signed-in user."),
        (name = "library", description = "The signed-in user's posts, stats and collections."),
        (name = "search", description = "Ranked search over the signed-in user's library."),
    )
)]
pub struct ApiDoc;

/// Every route of the API, grouped by limits, with its OpenAPI paths.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    let standard = OpenApiRouter::default()
        .routes(routes!(health::health))
        .routes(routes!(docs::openapi_json))
        .routes(routes!(posts::list_posts))
        .routes(routes!(posts::get_post))
        .routes(routes!(search::search))
        .routes(routes!(stats::get_stats))
        .routes(routes!(collections::list_collections))
        .routes(routes!(notifications::list_notifications))
        .routes(routes!(notifications::mark_notifications_read))
        .routes(routes!(client_errors::report_client_error))
        .routes(routes!(version::get_version))
        .merge(auth::router())
        .merge(me::router());
    // Streams end when the shutdown token fires instead of on a timer.
    let streams = OpenApiRouter::default().routes(routes!(events::stream_events));
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(RouteLimits::STANDARD.apply(standard))
        .merge(RouteLimits::STREAM.apply(streams))
        .merge(RouteLimits::STANDARD.apply(media::router()))
}

/// The OpenAPI document of [`router`], with the shared error response added
/// as the `default` response of every operation.
#[must_use]
pub fn openapi() -> OpenApiDoc {
    let mut doc = router().into_openapi();
    add_problem_responses(&mut doc);
    doc
}

/// [`openapi()`] as JSON, in the served and committed form: the layout the
/// repository's prettier gives `.json` files, so the pre-commit hook leaves a
/// regenerated `openapi.json` byte for byte as written.
#[must_use]
pub fn openapi_json() -> String {
    let pretty = serde_json::to_string_pretty(&openapi()).expect("the document serializes");
    prettier_layout(&pretty)
}

/// Rewrites `serde_json`'s pretty output (2-space indent, every array item on
/// its own line) into prettier's JSON layout at print width 100: an array of
/// scalars that fits goes on one line, `["a", "b"]`. Objects stay expanded,
/// as prettier keeps them. Ends with a newline.
fn prettier_layout(pretty: &str) -> String {
    const PRINT_WIDTH: usize = 100;
    let lines: Vec<&str> = pretty.lines().collect();
    let indent_of = |line: &str| line.len() - line.trim_start().len();
    let mut out = String::with_capacity(pretty.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.ends_with('[') {
            let indent = indent_of(line);
            let mut items = Vec::new();
            let mut end = None;
            for (j, item) in lines.iter().enumerate().skip(i + 1) {
                let trimmed = item.trim_start();
                if indent_of(item) == indent && trimmed.starts_with(']') {
                    end = Some(j);
                    break;
                }
                let value = trimmed.strip_suffix(',').unwrap_or(trimmed);
                if indent_of(item) != indent + 2 || !is_scalar(value) {
                    break;
                }
                items.push(value);
            }
            if let Some(end) = end {
                let flat = format!("{line}{}{}", items.join(", "), lines[end].trim_start());
                if flat.chars().count() <= PRINT_WIDTH {
                    out.push_str(&flat);
                    out.push('\n');
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
        i += 1;
    }
    out
}

fn is_scalar(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text).is_ok_and(|v| !v.is_array() && !v.is_object())
}

fn add_problem_responses(doc: &mut OpenApiDoc) {
    let problem = ResponseBuilder::new()
        .description("An error, as RFC 9457 problem details with a stable `code`.")
        .content(
            PROBLEM_JSON,
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("Problem")))
                .build(),
        )
        .build();
    doc.components
        .get_or_insert_with(Components::new)
        .responses
        .insert(PROBLEM_RESPONSE.to_owned(), RefOr::T(problem));
    for item in doc.paths.paths.values_mut() {
        let operations = [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.options,
            &mut item.head,
            &mut item.patch,
            &mut item.trace,
        ];
        for operation in operations.into_iter().flatten() {
            add_default_response(operation);
        }
    }
}

fn add_default_response(operation: &mut Operation) {
    operation
        .responses
        .responses
        .entry("default".to_owned())
        .or_insert_with(|| RefOr::Ref(Ref::from_response_name(PROBLEM_RESPONSE)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_scalar_arrays_go_on_one_line_like_prettier() {
        let pretty = r#"{
  "tags": [
    "platform"
  ],
  "enum": [
    "ok",
    "fail"
  ],
  "empty": [],
  "objects": [
    {
      "a": 1
    }
  ],
  "long": [
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "cccccccccccc"
  ],
  "tricky": [
    "a,",
    "[",
    1.5,
    null
  ]
}"#;
        let expected = r#"{
  "tags": ["platform"],
  "enum": ["ok", "fail"],
  "empty": [],
  "objects": [
    {
      "a": 1
    }
  ],
  "long": [
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "cccccccccccc"
  ],
  "tricky": ["a,", "[", 1.5, null]
}
"#;
        let laid_out = prettier_layout(pretty);
        assert_eq!(laid_out, expected);
        let before: serde_json::Value = serde_json::from_str(pretty).unwrap();
        let after: serde_json::Value = serde_json::from_str(&laid_out).unwrap();
        assert_eq!(before, after);
    }
}
