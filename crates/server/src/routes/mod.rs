//! The router composition and the OpenAPI document (plan §2.9, D6).
//!
//! Routes are registered in groups by their limits ([`RouteLimits`]); each
//! handler carries a `#[utoipa::path]`, so the document is generated from the
//! code that serves it. A new route goes into its group here:
//!
//! | Group | Limits | Routes |
//! |---|---|---|
//! | `standard` | 64 KiB, 30 s | everything JSON: health, OpenAPI, auth, account; the read API (T11), library; notifications, client errors, version (P1-01); jobs and queues (P1-07); library writes and collections (P1-03); passkeys and re-authentication (P1-13); the account, its sessions and tokens, and the device flow (P1-17); bulk actions and the trash (P1-11); the extension's pairing, configuration and status (P2-03); shared links (P2-11) |
//! | `streams` | 64 KiB, no time limit | `GET /api/v1/events` (P1-01), `POST /api/v1/search/chat` (P3), the extension's long poll `GET /api/v1/ingest/tasks` (P2-14) |
//! | `media` | 64 KiB, 30 s until the headers | `GET /media/{file}`, outside `/api` and the document ([`media`]) |
//! | `upload_chunks` | [`RouteLimits::UPLOAD_CHUNK`]: 16 MiB, no time limit | tus `PATCH /api/v1/uploads/{id}` (T9, P4-08, [`uploads`]) |
//! | ingest, STT | [`RouteLimits::INGEST`], [`RouteLimits::STT`] | added with their routes (P2, P3) |
//!
//! The uploads (T9, P1-19, P4-08): [`uploads`] (tus creation, `HEAD` to
//! resume, `PATCH` chunks, `DELETE` to terminate) take a session, an
//! `uploads` token or a `migrate` token ([`TOKEN_ROUTES`]), and the upload's
//! purpose decides which of them may upload what
//! ([`crate::control::uploads::UploadPurpose`]). The migration routes
//! [`migrations`] (preflight, missing objects, install, status) take a
//! `migrate` token and nothing else; the install is the `migrate` job
//! ([`crate::jobs::migrate`]).
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
//! The bulk actions and the trash (P1-11): `POST /posts/bulk` in [`bulk`]
//! (up to 500 posts in the request, more as a `bulk` job), and `GET
//! /trash`, `POST /trash/restore` and `POST /trash/empty` (a `purge` job) in
//! [`trash`]. `DELETE /collections/{id}?mode=withPosts` moves a collection's
//! posts to the trash.
//!
//! Library API clients (F21) use independent `library:read` and
//! `library:write` tokens on these routes and on media. Writes must be
//! granted explicitly; a library token defaults to read only. The account,
//! auth, provider, job and queue routes retain their existing cookie policy.
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
//! The links route (P2-11): [`links`] (`POST /links`, a session or a
//! `links:create` token), which saves a shared link as a post and enqueues
//! its hydration ([`crate::jobs::hydrate`]).
//!
//! The extension routes (P2-03): [`extension`] (`POST /extension/pair`,
//! public and CSRF-exempt; `GET /extension/config`, an `ingest` token;
//! `GET /extension/status`, a session) and `POST /me/tokens/pairing-code`
//! in [`me`], over [`crate::extension`]. Every token route an `extension`
//! token reaches also passes its version gate ([`crate::extension::admit`]).
//!
//! The extension's tasks (P2-14): [`ingest_tasks`] (`GET /ingest/tasks`, the
//! long poll, and `POST /ingest/tasks/{id}/complete`), a `tasks` token, over
//! [`crate::extension::tasks`]; its uploads are tus uploads of purpose
//! `archive-object` with the `uploads` scope.
//!
//! The committed copy of the document, `crates/server/openapi.json`, is what
//! the TypeScript client is generated from (T11). After changing a route,
//! regenerate it with
//! `UPDATE_OPENAPI=1 cargo test -p shelfy-server --test openapi`, then the
//! client with `pnpm exec tsx scripts/api-client/generate.ts`; both checks
//! fail while a committed copy is stale.

pub mod ai;
pub mod auth;
pub mod bulk;
pub mod client_errors;
pub mod collections;
pub mod device;
pub mod docs;
pub mod events;
pub mod extension;
pub mod health;
pub mod ingest;
pub mod ingest_tasks;
pub mod jobs;
pub mod links;
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
pub mod sync_runs;
pub mod tag_aliases;
pub mod tag_clusters;
pub mod trash;
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
        event::ExtensionStatusEvent,
        event::ProviderStatusEvent,
        event::ProviderState,
        event::SyncProgressEvent,
        event::SyncListing,
        event::AiStreamEvent,
        listing::MatchMode,
        listing::PostSort,
        listing::YesNo,
        posts::PostSource,
        search::SearchScope,
        collections::CollectionDeleteMode,
        tag_aliases::ReviewStatus,
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
        (name = "ai", description = "AI taxonomy: proposed and accepted clusters and aliases."),
        (name = "search", description = "Ranked search over the signed-in user's library."),
        (
            name = "uploads",
            description = "Resumable uploads (tus 1.0): bookmark files and imports from the web \
                           app, migration bundles from the CLI, archive objects from the browser \
                           extension."
        ),
        (
            name = "migration",
            description = "Moving a desktop library: what the server lacks, and the install."
        ),
        (name = "jobs", description = "The signed-in user's background jobs and their queues: \
                                       progress, cancel, retry, pause and resume."),
        (name = "extension", description = "The browser extension: pairing, its configuration \
                                            and kill switches, and whether it is connected."),
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
    (Method::POST, "/api/v1/extension/pair"),
];

/// Public routes that programs call without a cookie, and that read none:
/// the CSRF guard lets them through without `Origin` and `X-Shelfy-Client`
/// ([`crate::auth::csrf`]). The migration CLI signs in with the device flow
/// before it has a token; the browser extension exchanges its pairing code
/// for one. Each must also be in [`PUBLIC_ROUTES`].
pub const CSRF_EXEMPT_ROUTES: &[(Method, &str)] = &[
    (Method::POST, "/api/v1/auth/device/start"),
    (Method::POST, "/api/v1/auth/device/poll"),
    (Method::POST, "/api/v1/extension/pair"),
];

/// Routes that take a scoped API token: method, route template, the scopes
/// (a token needs one of them), and whether a signed-in session works too.
/// Such a route also declares one `("bearer" = ["<scope>"])` per scope in
/// the `security(…)` of its `#[utoipa::path]`, plus `("session" = [])` when
/// sessions work too. The migration routes (T9) take the CLI's `migrate`
/// token only; `POST /posts/lookup` takes a `lookup` token or a session
/// (P1-17); the tus uploads take a session, an `uploads` token or a
/// `migrate` token, and the purpose of each upload decides further (P4-08);
/// `GET /extension/config` takes the extension's `ingest` token (P2-03);
/// `POST /links` takes the iOS Shortcut's `links:create` token or a session
/// (P2-11); the extension's tasks take its `tasks` token (P2-14).
/// Library clients use independent `library:read` and `library:write`
/// scopes (F21); write does not imply read. Media uses `library:read`,
/// but stays outside the OpenAPI document as before.
pub const TOKEN_ROUTES: &[(Method, &str, &[Scope], bool)] = &[
    (Method::GET, "/api/v1/posts", &[Scope::LibraryRead], true),
    (
        Method::GET,
        "/api/v1/posts/{key}",
        &[Scope::LibraryRead],
        true,
    ),
    (
        Method::GET,
        "/api/v1/posts/count",
        &[Scope::LibraryRead],
        true,
    ),
    (
        Method::POST,
        "/api/v1/posts/batch-get",
        &[Scope::LibraryRead],
        true,
    ),
    (Method::GET, "/api/v1/search", &[Scope::LibraryRead], true),
    (Method::GET, "/api/v1/stats", &[Scope::LibraryRead], true),
    (
        Method::GET,
        "/api/v1/collections",
        &[Scope::LibraryRead],
        true,
    ),
    (Method::GET, "/api/v1/trash", &[Scope::LibraryRead], true),
    (Method::GET, "/media/{file}", &[Scope::LibraryRead], true),
    (
        Method::PATCH,
        "/api/v1/posts/{key}",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::POST,
        "/api/v1/posts/bulk",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::POST,
        "/api/v1/collections",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::PATCH,
        "/api/v1/collections/{id}",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::DELETE,
        "/api/v1/collections/{id}",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::POST,
        "/api/v1/collections/{id}/posts",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::DELETE,
        "/api/v1/collections/{id}/posts/{key}",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::POST,
        "/api/v1/collections/from-query",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::POST,
        "/api/v1/trash/restore",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::POST,
        "/api/v1/trash/empty",
        &[Scope::LibraryWrite],
        true,
    ),
    (
        Method::POST,
        "/api/v1/posts/lookup",
        &[Scope::Lookup, Scope::LibraryRead],
        true,
    ),
    (
        Method::POST,
        "/api/v1/links",
        &[Scope::LinksCreate, Scope::LibraryWrite],
        true,
    ),
    (
        Method::POST,
        "/api/v1/ingest/batches",
        &[Scope::Ingest],
        false,
    ),
    (Method::POST, "/api/v1/sync-runs", &[Scope::Ingest], false),
    (
        Method::PATCH,
        "/api/v1/sync-runs/{id}",
        &[Scope::Ingest],
        false,
    ),
    (
        Method::GET,
        "/api/v1/extension/config",
        &[Scope::Ingest],
        false,
    ),
    (
        Method::GET,
        "/api/v1/extension/sources",
        &[Scope::Ingest],
        false,
    ),
    (Method::GET, "/api/v1/ingest/tasks", &[Scope::Tasks], false),
    (
        Method::POST,
        "/api/v1/ingest/tasks/{id}/complete",
        &[Scope::Tasks],
        false,
    ),
    (
        Method::POST,
        "/api/v1/uploads",
        uploads::UPLOAD_SCOPES,
        true,
    ),
    (
        Method::HEAD,
        "/api/v1/uploads/{id}",
        uploads::UPLOAD_SCOPES,
        true,
    ),
    (
        Method::PATCH,
        "/api/v1/uploads/{id}",
        uploads::UPLOAD_SCOPES,
        true,
    ),
    (
        Method::DELETE,
        "/api/v1/uploads/{id}",
        uploads::UPLOAD_SCOPES,
        true,
    ),
    (
        Method::GET,
        "/api/v1/migrations/preflight",
        &[Scope::Migrate],
        false,
    ),
    (
        Method::POST,
        "/api/v1/migrations/missing-objects",
        &[Scope::Migrate],
        false,
    ),
    (Method::POST, "/api/v1/migrations", &[Scope::Migrate], false),
    (
        Method::GET,
        "/api/v1/migrations/{id}",
        &[Scope::Migrate],
        false,
    ),
];

/// Job-creating routes that take `Idempotency-Key` (plan §2.9): a repeat
/// with the same key gets the first response back
/// ([`crate::jobs::idempotency`]). Such a route also declares
/// `params(IdempotencyHeader)` in its `#[utoipa::path]`; a test checks that
/// this list and the document agree. `body_bytes` is the route's body limit.
pub const IDEMPOTENT_ROUTES: &[IdempotentRoute] = &[
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/tag-clusters/regenerate",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/tag-aliases/propose",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/ai/analyze",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/ai/queue/retry",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/exports",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/ingest/batches",
        body_bytes: RouteLimits::INGEST.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/jobs/{id}/retry",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/migrations",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/posts/bulk",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/trash/restore",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
    IdempotentRoute {
        method: Method::POST,
        path: "/api/v1/trash/empty",
        body_bytes: RouteLimits::STANDARD.body_bytes,
    },
];

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
        .fold(policy, |policy, (method, path, scopes, session)| {
            policy.token(method.clone(), *path, *scopes, *session)
        })
}

/// Every route of the API, grouped by limits, with its OpenAPI paths.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    let standard = OpenApiRouter::default()
        .routes(routes!(health::health))
        .routes(routes!(ai::analyze))
        .routes(routes!(ai::get_queue))
        .routes(routes!(ai::cancel_queue))
        .routes(routes!(ai::retry_queue))
        .routes(routes!(docs::openapi_json))
        .routes(routes!(posts::list_posts))
        .routes(routes!(posts::count_posts))
        .routes(routes!(posts::batch_get_posts))
        .routes(routes!(posts::lookup_posts))
        .routes(routes!(bulk::bulk_posts))
        .routes(routes!(posts::get_post, post_edit::update_post))
        .routes(routes!(search::search))
        .routes(routes!(tag_clusters::list_clusters))
        .routes(routes!(tag_clusters::regenerate_clusters))
        .routes(routes!(
            tag_clusters::update_cluster,
            tag_clusters::dismiss_cluster
        ))
        .routes(routes!(tag_clusters::remove_cluster_tag))
        .routes(routes!(tag_aliases::list_aliases))
        .routes(routes!(tag_aliases::propose_aliases))
        .routes(routes!(tag_aliases::accept_alias))
        .routes(routes!(tag_aliases::dismiss_alias))
        .routes(routes!(tag_aliases::accept_all_aliases))
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
        .routes(routes!(links::create_link))
        .routes(routes!(sync_runs::open_sync_run, sync_runs::list_sync_runs))
        .routes(routes!(sync_runs::update_sync_run))
        .routes(routes!(sync_runs::list_extension_sources))
        .routes(routes!(ingest_tasks::complete_extension_task))
        .routes(routes!(trash::list_trash))
        .routes(routes!(trash::restore_trash))
        .routes(routes!(trash::empty_trash))
        .routes(routes!(notifications::list_notifications))
        .routes(routes!(notifications::mark_notifications_read))
        .routes(routes!(client_errors::report_client_error))
        .routes(routes!(version::get_version))
        .routes(routes!(uploads::create_upload))
        .routes(routes!(uploads::upload_offset, uploads::delete_upload))
        .routes(routes!(migrations::migration_preflight))
        .routes(routes!(migrations::find_missing_objects))
        .routes(routes!(migrations::start_migration))
        .routes(routes!(migrations::get_migration))
        .routes(routes!(exports::start_export, exports::list_exports))
        .routes(routes!(exports::delete_export))
        .routes(routes!(exports::download_export))
        .merge(auth::router())
        .merge(passkeys::router())
        .merge(reauth::router())
        .merge(me::router())
        .merge(device::router())
        .merge(extension::router())
        .merge(jobs::router());
    // Streams end when the shutdown token fires instead of on a timer.
    let streams = OpenApiRouter::default()
        .routes(routes!(events::stream_events))
        .routes(routes!(ingest_tasks::list_extension_tasks));
    let upload_chunks = OpenApiRouter::default().routes(routes!(uploads::append_upload));
    // Capture batches are larger (up to 8 MiB, ≤ 500 items).
    let ingest = OpenApiRouter::default().routes(routes!(ingest::ingest_batch));
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(RouteLimits::STANDARD.apply(standard))
        .merge(RouteLimits::STREAM.apply(streams))
        .merge(RouteLimits::UPLOAD_CHUNK.apply(upload_chunks))
        .merge(RouteLimits::INGEST.apply(ingest))
        .merge(RouteLimits::STANDARD.apply(media::router()))
        .layer(Extension(Arc::new(uploads::UploadLocks::default())))
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
    fn csrf_exempt_routes_are_public_ones() {
        for (method, path) in CSRF_EXEMPT_ROUTES {
            assert!(
                PUBLIC_ROUTES.iter().any(|(m, p)| m == method && p == path),
                "{method} {path} skips the CSRF check but is not public"
            );
        }
    }

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

pub mod exports;
