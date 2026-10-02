//! The router composition and the OpenAPI document (plan §2.9, D6).
//!
//! Routes are registered in groups by their limits ([`RouteLimits`]); each
//! handler carries a `#[utoipa::path]`, so the document is generated from the
//! code that serves it. A new route goes into its group here:
//!
//! | Group | Limits | Routes |
//! |---|---|---|
//! | `standard` | 64 KiB, 30 s | everything JSON: health, OpenAPI, auth, account; the read API (T11), library; notifications, client errors, version (P1-01); jobs and queues (P1-07); library writes and collections (P1-03); passkeys and re-authentication (P1-13); the account, its sessions and tokens, and the device flow (P1-17) |
//! | `streams` | 64 KiB, no time limit | `GET /api/v1/events` (P1-01), `POST /api/v1/search/chat` (P3) |
//! | `media` | 64 KiB, 30 s until the headers | `GET /media/{file}`, outside `/api` and the document ([`media`]) |
//! | `upload_chunks` | [`RouteLimits::UPLOAD_CHUNK`]: 16 MiB, no time limit | tus `PATCH /api/v1/uploads/{id}` (T9, [`uploads`]) |
//! | ingest, STT | [`RouteLimits::INGEST`], [`RouteLimits::STT`] | added with their routes (P2, P3) |
//!
//! The migration routes (T9): [`uploads`] (tus creation, `HEAD` to resume,
//! `PATCH` chunks) and [`migrations`] (missing objects, install, status).
//! They take a `migrate` token and nothing else ([`TOKEN_ROUTES`]), and
//! share the in-process state of [`crate::migrations`] through request
//! extensions.
//!
//! The read API (T11): [`posts`] (`GET /posts`, `GET /posts/{key}`),
//! [`search`], [`stats`] and [`collections`], all behind
//! [`CurrentUser`](crate::current_user::CurrentUser) and conditional
//! ([`crate::conditional`]); their JSON shapes are in [`model`] and the shared
//! paging in [`listing`].
//!
//! The library writes (P1-03): `GET /posts/count`, `POST /posts/batch-get`
//! and `POST /posts/lookup` in [`posts`]; `PATCH /posts/{key}` in
//! [`post_edit`]; the collection writes in [`collections`]; the selector of
//! the routes that act on many posts in [`selector`]. Every write goes
//! through [`crate::library`], which announces it on the event bus.
//!
//! The platform routes (P1-01): [`events`] (the SSE stream of
//! [`crate::events`]), [`notifications`], [`client_errors`] and [`version`],
//! all behind [`CurrentUser`](crate::current_user::CurrentUser).
//!
//! The job routes (P1-07): [`jobs`] (`/jobs` and `/queues/{kind}/…`) over
//! [`crate::jobs`]. A job-creating route that takes `Idempotency-Key` is
//! listed in [`IDEMPOTENT_ROUTES`] and declares the header with
//! `params(IdempotencyHeader)`.
//!
//! The passkey routes (P1-13): [`passkeys`] (username-less sign-in, public;
//! the account's passkeys under `/me/passkeys`) and [`reauth`]
//! (re-authentication of the session), over [`crate::auth::passkeys`] and
//! [`crate::auth::reauth`].
//!
//! The account routes (P1-17): [`me`] (profile and capabilities, settings,
//! consent, usage, sessions, API tokens) and [`device`] (the migration CLI's
//! sign-in, over [`crate::auth::device`]). `POST /posts/lookup` also takes a
//! `lookup` token ([`TOKEN_ROUTES`]).
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
pub mod device;
pub mod docs;
pub mod events;
pub mod health;
pub mod jobs;
pub mod listing;
pub mod me;
pub mod media;
pub mod migrations;
pub mod model;
pub mod notifications;
pub mod passkeys;
pub mod post_edit;
pub mod posts;
pub mod reauth;
pub mod search;
pub mod selector;
pub mod stats;
pub mod uploads;
pub mod version;

use std::sync::Arc;

use axum::Extension;
use axum::http::Method;
use utoipa::OpenApi;
use utoipa::openapi::path::Operation;
use utoipa::openapi::{
    Components, ContentBuilder, OpenApi as OpenApiDoc, Ref, RefOr, ResponseBuilder,
};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::access::AccessPolicy;
use crate::auth::bearer::Scope;
use crate::auth::openapi::SecuritySchemes;
use crate::error::{ErrorCode, FieldError, PROBLEM_JSON, Problem};
use crate::events::model as event;
use crate::jobs::idempotency::IdempotentRoute;
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
        collections::CollectionDeleteMode,
    )),
    modifiers(&SecuritySchemes),
    tags(
        (name = "platform", description = "Health, the API description, the realtime stream, \
                                           notifications, client error reports and the version."),
        (name = "auth", description = "Sign-in with passkeys and links, re-authentication, \
                                       sessions and sign-out, and the migration CLI's device \
                                       sign-in."),
        (name = "account", description = "The signed-in user: profile and capabilities, \
                                          settings, consent, storage use, sessions, passkeys and \
                                          API tokens."),
        (name = "library", description = "The signed-in user's posts, stats and collections, \
                                          and their edits."),
        (name = "search", description = "Ranked search over the signed-in user's library."),
        (
            name = "migration",
            description = "Moving a desktop library: resumable uploads (tus 1.0) and the install."
        ),
        (name = "jobs", description = "The signed-in user's background jobs and their queues: \
                                       progress, cancel, retry, pause and resume."),
    )
)]
pub struct ApiDoc;

/// Routes anyone may call, by method and route template. Every other route
/// needs a signed-in session or, for [`TOKEN_ROUTES`], a scoped API token:
/// access is denied by default ([`crate::auth::access`]). A public route also
/// declares `security(())` in its `#[utoipa::path]`; the authz test checks
/// that this list and the document agree.
pub const PUBLIC_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/health"),
    (Method::GET, "/api/v1/openapi.json"),
    (Method::GET, "/api/v1/auth/methods"),
    (Method::POST, "/api/v1/auth/magic-links"),
    (Method::POST, "/api/v1/auth/magic-links/redeem"),
    (Method::POST, "/api/v1/auth/passkeys/login/start"),
    (Method::POST, "/api/v1/auth/passkeys/login/finish"),
    (Method::POST, "/api/v1/auth/logout"),
    (Method::POST, "/api/v1/auth/device/start"),
    (Method::POST, "/api/v1/auth/device/poll"),
];

/// Public routes that programs call without a cookie, and that read none:
/// the CSRF guard lets them through without `Origin` and `X-Shelfy-Client`
/// ([`crate::auth::csrf`]). The migration CLI signs in with them before it
/// has a token. Each must also be in [`PUBLIC_ROUTES`].
pub const CSRF_EXEMPT_ROUTES: &[(Method, &str)] = &[
    (Method::POST, "/api/v1/auth/device/start"),
    (Method::POST, "/api/v1/auth/device/poll"),
];

/// Routes that take a scoped API token: method, route template, the scope,
/// and whether a signed-in session works too. Such a route also declares
/// `security(("bearer" = ["<scope>"]))` in its `#[utoipa::path]`, plus
/// `("session" = [])` when sessions work too. The migration routes (T9)
/// take the CLI's `migrate` token only; `POST /posts/lookup` takes a `lookup`
/// token or a session (P1-17); the extension routes join in P2.
pub const TOKEN_ROUTES: &[(Method, &str, Scope, bool)] = &[
    (Method::POST, "/api/v1/posts/lookup", Scope::Lookup, true),
    (Method::POST, "/api/v1/uploads", Scope::Migrate, false),
    (Method::HEAD, "/api/v1/uploads/{id}", Scope::Migrate, false),
    (Method::PATCH, "/api/v1/uploads/{id}", Scope::Migrate, false),
    (
        Method::POST,
        "/api/v1/migrations/missing-objects",
        Scope::Migrate,
        false,
    ),
    (Method::POST, "/api/v1/migrations", Scope::Migrate, false),
    (
        Method::GET,
        "/api/v1/migrations/{id}",
        Scope::Migrate,
        false,
    ),
];

/// Job-creating routes that take `Idempotency-Key` (plan §2.9): a repeat
/// with the same key gets the first response back
/// ([`crate::jobs::idempotency`]). Such a route also declares
/// `params(IdempotencyHeader)` in its `#[utoipa::path]`; a test checks that
/// this list and the document agree. `body_bytes` is the route's body limit.
pub const IDEMPOTENT_ROUTES: &[IdempotentRoute] = &[IdempotentRoute {
    method: Method::POST,
    path: "/api/v1/jobs/{id}/retry",
    body_bytes: RouteLimits::STANDARD.body_bytes,
}];

/// The access policy of [`router`]: [`PUBLIC_ROUTES`] and [`TOKEN_ROUTES`];
/// every other route needs a session.
#[must_use]
pub fn access() -> AccessPolicy {
    let policy = PUBLIC_ROUTES
        .iter()
        .fold(AccessPolicy::new(), |policy, (method, path)| {
            policy.public(method.clone(), *path)
        });
    TOKEN_ROUTES
        .iter()
        .fold(policy, |policy, (method, path, scope, session)| {
            policy.token(method.clone(), *path, *scope, *session)
        })
}

/// Every route of the API, grouped by limits, with its OpenAPI paths.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    let standard = OpenApiRouter::default()
        .routes(routes!(health::health))
        .routes(routes!(docs::openapi_json))
        .routes(routes!(posts::list_posts))
        .routes(routes!(posts::count_posts))
        .routes(routes!(posts::batch_get_posts))
        .routes(routes!(posts::lookup_posts))
        .routes(routes!(posts::get_post, post_edit::update_post))
        .routes(routes!(search::search))
        .routes(routes!(stats::get_stats))
        .routes(routes!(
            collections::list_collections,
            collections::create_collection
        ))
        .routes(routes!(
            collections::update_collection,
            collections::delete_collection
        ))
        .routes(routes!(collections::add_collection_posts))
        .routes(routes!(collections::remove_collection_post))
        .routes(routes!(collections::create_collection_from_query))
        .routes(routes!(notifications::list_notifications))
        .routes(routes!(notifications::mark_notifications_read))
        .routes(routes!(client_errors::report_client_error))
        .routes(routes!(version::get_version))
        .routes(routes!(uploads::create_upload))
        .routes(routes!(uploads::upload_offset))
        .routes(routes!(migrations::find_missing_objects))
        .routes(routes!(migrations::start_migration))
        .routes(routes!(migrations::get_migration))
        .merge(auth::router())
        .merge(passkeys::router())
        .merge(reauth::router())
        .merge(me::router())
        .merge(device::router())
        .merge(jobs::router());
    // Streams end when the shutdown token fires instead of on a timer.
    let streams = OpenApiRouter::default().routes(routes!(events::stream_events));
    let upload_chunks = OpenApiRouter::default().routes(routes!(uploads::append_upload));
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(RouteLimits::STANDARD.apply(standard))
        .merge(RouteLimits::STREAM.apply(streams))
        .merge(RouteLimits::UPLOAD_CHUNK.apply(upload_chunks))
        .merge(RouteLimits::STANDARD.apply(media::router()))
        .layer(Extension(Arc::new(uploads::UploadLocks::default())))
        .layer(Extension(Arc::new(crate::migrations::Installs::default())))
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
/// scalars (or of empty objects, such as the public routes' `security: [{}]`)
/// that fits goes on one line, `["a", "b"]`. Objects with members stay
/// expanded, as prettier keeps them. Ends with a newline.
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
    serde_json::from_str::<serde_json::Value>(text).is_ok_and(|v| match v {
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(members) => members.is_empty(),
        _ => true,
    })
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
  ],
  "security": [
    {}
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
  "tricky": ["a,", "[", 1.5, null],
  "security": [{}]
}
"#;
        let laid_out = prettier_layout(pretty);
        assert_eq!(laid_out, expected);
        let before: serde_json::Value = serde_json::from_str(pretty).unwrap();
        let after: serde_json::Value = serde_json::from_str(&laid_out).unwrap();
        assert_eq!(before, after);
    }
}
