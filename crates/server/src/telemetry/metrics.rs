//! Prometheus metrics (plan §3.6), served on their own listener (`:9464`),
//! which only the internal network reaches; the edge proxy never forwards it.
//!
//! The recorder is process-wide: [`install`] sets it up once, and every
//! `metrics::counter!`/`gauge!`/`histogram!` of the process reports to it,
//! the media crate's included. No label carries a user id or any other
//! per-user value: routes are templates, and every other label takes its
//! values from a fixed set (`tests/metrics.rs` checks both).
//!
//! | Metric | Type | Labels | What |
//! |---|---|---|---|
//! | `shelfy_http_requests_total` | counter | `route`, `method`, `status` | responses ([`super::http::observe`]) |
//! | `shelfy_http_request_duration_seconds` | histogram, [`DURATION_BUCKETS`] | `route` | time to the response headers |
//! | `shelfy_media_request_duration_seconds` | histogram, [`DURATION_BUCKETS`] | `variant` ([`media_variant`]) | time to the response headers of `GET /media/{file}`, by what the file name designates (F8) |
//! | `shelfy_sse_connections` | gauge | — | open `GET /api/v1/events` streams |
//! | `shelfy_jobs` | gauge | `kind`, `state` ([`job_state`]) | jobs the scheduler holds |
//! | `shelfy_job_oldest_queued_seconds` | gauge | `kind` | how long the oldest due job of an unpaused queue has waited; 0 when none |
//! | `shelfy_job_duration_seconds` | histogram, [`JOB_DURATION_BUCKETS`] | `kind`, `outcome` ([`job_outcome`]) | each ended attempt of a job |
//! | `shelfy_disk_bytes` | gauge | `area` ([`DISK_AREAS`], [`OTHER_AREA`]) | bytes of the files under each part of the data directory |
//! | `shelfy_open_user_dbs` | gauge | — | user libraries open in the handle cache |
//! | `shelfy_sqlite_busy_total` | counter | — | SQLite calls that gave up on a lock ([`shelfy_core::db::sqlite_busy_total`]) |
//! | `shelfy_rendition_bytes` | histogram, [`RENDITION_BUCKETS`] | `variant` (`g480`) | each rendition written ([`shelfy_media::store::RENDITION_BYTES`]) |
//! | `shelfy_egress_requests_total` | counter | `purpose` ([`crate::outbound::Purpose`]), `outcome` ([`egress_outcome`]) | outbound HTTP requests, once each, redirects included |
//! | `shelfy_ai_requests_total` | counter | `provider_kind` ([`ai_provider_kind`]), `task` ([`ai_task`]), `outcome` ([`ai_outcome`]) | AI calls through the service ([`crate::ai`]); no per-user label |
//! | `shelfy_ai_tokens_total` | counter | `direction` ([`ai_direction`]) | prompt and answer tokens providers reported |
//! | `shelfy_media_fetch_total` | counter | `host_group` (`instagram`, `x`, `pinterest`, [`fetch_outcome::NO_GROUP`]), `outcome` ([`fetch_outcome`]) | CDN fetches of the archive |
//! | `shelfy_breaker_open` | gauge, 0 or 1 | `host_group` ([`crate::outbound::HostGroup`]: the three CDNs and `instagram_web`, `x_web`, `pinterest_web`) | whether a host group's breaker is open (or half-open); 0 from the start |
//! | `shelfy_archive_cover_latency_seconds` | histogram, [`COVER_LATENCY_BUCKETS`] | `platform` (`instagram`, `twitter`, `pinterest`) | from a post's insert to its cover stored by the archive, for posts inserted in the last 7 days ([`crate::jobs::archive`]) |
//! | `shelfy_archive_backlog` | gauge | `platform` | items left to the server's archive, as the drains last counted them |
//! | `shelfy_ingest_items_total` | counter | `platform`, `outcome` ([`ingest_outcome`]) | items of ingest batches (P2-09) |
//! | `shelfy_sync_run_pages` | histogram, [`SYNC_RUN_PAGES_BUCKETS`] | `platform`, `trigger` | pages a finished sync run scanned (P2-09) |
//! | `shelfy_gc_objects_deleted_total` | counter | — | unreferenced objects collected |
//! | `shelfy_gc_bytes_freed_total` | counter | — | bytes of collected object rows |
//! | `shelfy_build_info` | gauge, always 1 | `version` | the build |
//!
//! `route` is a route template (`/api/v1/posts/{key}`), [`super::http::SPA_ROUTE`]
//! for the web app's files, or [`super::http::UNMATCHED_ROUTE`]. `method` is
//! a standard method or `OTHER`.
//!
//! Counters and histograms change as things happen. The gauges are sampled
//! by the server's maintenance task: [`sample`] every
//! [`UPKEEP_INTERVAL`] (5 s), [`sample_disk`] every [`DISK_INTERVAL`]
//! (5 minutes), so a scrape never waits on a database or a disk walk. The
//! disk sample of the `users` area is also kept in the state: the media
//! budget of [`crate::quota`] starts from it.

use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::http::{Method, StatusCode, header};
use axum::routing::get;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
pub use shelfy_media::store::RENDITION_BYTES;

use crate::control::jobs as job_rows;
use crate::jobs::Kind;
use crate::state::{AppState, blocking};

/// Histogram of the time from a post's insert to its cover stored, by
/// platform (§5 P2 exit: 98 % within 15 minutes).
pub const ARCHIVE_COVER_LATENCY_SECONDS: &str = "shelfy_archive_cover_latency_seconds";
/// Gauge of the items left to the archive, by platform.
pub const ARCHIVE_BACKLOG: &str = "shelfy_archive_backlog";
/// Unreferenced object rows collected.
pub const GC_OBJECTS_DELETED_TOTAL: &str = "shelfy_gc_objects_deleted_total";
/// Bytes recorded by those collected rows (renditions excluded).
pub const GC_BYTES_FREED_TOTAL: &str = "shelfy_gc_bytes_freed_total";
/// Counter of HTTP requests by route template, method and status.
pub const HTTP_REQUESTS_TOTAL: &str = "shelfy_http_requests_total";
/// Histogram of request handling time by route template, in seconds.
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "shelfy_http_request_duration_seconds";
/// Histogram of `GET /media/{file}` handling time by [`media_variant`], in
/// seconds: the §6.2 rendition budget (p95 ≤ 5 ms) reads its `g480` series.
/// Kept apart from [`HTTP_REQUEST_DURATION_SECONDS`] so no other route pays
/// for the label.
pub const MEDIA_REQUEST_DURATION_SECONDS: &str = "shelfy_media_request_duration_seconds";
/// Gauge of the open realtime streams.
pub const SSE_CONNECTIONS: &str = "shelfy_sse_connections";
/// Gauge of the jobs the scheduler holds, by kind and [`job_state`].
pub const JOBS: &str = "shelfy_jobs";
/// Gauge: how long the oldest due job of each kind has waited, in seconds.
pub const JOB_OLDEST_QUEUED_SECONDS: &str = "shelfy_job_oldest_queued_seconds";
/// Histogram of job attempts, by kind and [`job_outcome`], in seconds.
pub const JOB_DURATION_SECONDS: &str = "shelfy_job_duration_seconds";
/// Gauge of the bytes under each area of the data directory.
pub const DISK_BYTES: &str = "shelfy_disk_bytes";
/// Gauge of the user libraries open in the handle cache.
pub const OPEN_USER_DBS: &str = "shelfy_open_user_dbs";
/// Counter of SQLite calls that gave up on a lock.
pub const SQLITE_BUSY_TOTAL: &str = "shelfy_sqlite_busy_total";
/// Counter of outbound HTTP requests, by purpose and [`egress_outcome`].
pub const EGRESS_REQUESTS_TOTAL: &str = "shelfy_egress_requests_total";
/// Counter of AI calls, by `provider_kind`, `task` and [`ai_outcome`].
pub const AI_REQUESTS_TOTAL: &str = "shelfy_ai_requests_total";
/// Counter of AI tokens, by [`ai_direction`].
pub const AI_TOKENS_TOTAL: &str = "shelfy_ai_tokens_total";
/// Counter of the archive's CDN fetches, by host group and [`fetch_outcome`].
pub const MEDIA_FETCH_TOTAL: &str = "shelfy_media_fetch_total";
/// Gauge: 1 while a host group's breaker is open or half-open.
pub const BREAKER_OPEN: &str = "shelfy_breaker_open";
/// Counter of ingested items by platform and [`ingest_outcome`].
pub const INGEST_ITEMS_TOTAL: &str = "shelfy_ingest_items_total";
/// Histogram of the pages a finished sync run scanned, by platform and trigger.
pub const SYNC_RUN_PAGES: &str = "shelfy_sync_run_pages";
/// Constant 1, labelled with the build version.
pub const BUILD_INFO: &str = "shelfy_build_info";

/// Content type of the Prometheus text exposition format.
pub const PROMETHEUS_TEXT: &str = "text/plain; version=0.0.4; charset=utf-8";

/// How often [`PrometheusHandle::run_upkeep`] must run (the exporter's
/// default), and how often [`sample`] runs.
pub const UPKEEP_INTERVAL: Duration = Duration::from_secs(5);
/// How often [`sample_disk`] walks the data directory.
pub const DISK_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Histogram buckets of request durations, in seconds, with bounds at the
/// §6.2 budgets (5, 15, 40, 60, 100, 300 and 500 ms).
pub const DURATION_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.015, 0.025, 0.04, 0.06, 0.1, 0.25, 0.3, 0.5, 1.0, 2.5, 5.0, 10.0,
    30.0,
];

/// Histogram buckets of job attempts, in seconds: from a quick task to the
/// 60-minute lease of an import or a migration.
pub const JOB_DURATION_BUCKETS: &[f64] = &[
    0.01, 0.05, 0.25, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1200.0, 1800.0, 3600.0,
];

/// Histogram buckets of rendition sizes, in bytes (1 KB = 1,000 bytes), with
/// bounds at the §6.2 `g480` budget: p50 ≤ 35 KB, p95 ≤ 60 KB.
pub const RENDITION_BUCKETS: &[f64] = &[
    2_500.0, 5_000.0, 10_000.0, 15_000.0, 20_000.0, 25_000.0, 30_000.0, 35_000.0, 40_000.0,
    50_000.0, 60_000.0, 80_000.0, 100_000.0, 150_000.0, 250_000.0, 500_000.0,
];

/// Histogram buckets of the cover latency, in seconds, with a bound at the
/// P2 exit's 15 minutes (900 s).
pub const COVER_LATENCY_BUCKETS: &[f64] = &[
    5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1_800.0, 3_600.0, 7_200.0, 21_600.0,
    86_400.0, 604_800.0,
];

/// Histogram buckets of the pages a sync run scanned, with a bound at the P2
/// exit's "≤ 2 pages per incremental run".
pub const SYNC_RUN_PAGES_BUCKETS: &[f64] = &[1.0, 2.0, 3.0, 5.0, 10.0, 25.0, 50.0, 100.0];

/// The `state` values of [`JOBS`].
pub mod job_state {
    /// Due, waiting for a slot (paused queues included).
    pub const READY: &str = "ready";
    /// Waiting for its `run_at`: a retry's backoff, a drain's next item.
    pub const DELAYED: &str = "delayed";
    /// An attempt is running.
    pub const RUNNING: &str = "running";
}

/// The `outcome` values of [`JOB_DURATION_SECONDS`]: how an attempt ended.
pub mod job_outcome {
    /// The job is done.
    pub const SUCCEEDED: &str = "succeeded";
    /// The try failed and the job failed for good: a permanent error, or no
    /// try left.
    pub const FAILED: &str = "failed";
    /// The try failed transiently; the job runs again after a backoff.
    pub const RETRIED: &str = "retried";
    /// The job runs again later without using a try: the worker asked for it
    /// (a drain that re-arms itself, a yield at a pause), or the user's
    /// library was locked for maintenance.
    pub const REQUEUED: &str = "requeued";
    /// The server's shutdown stopped it; it runs again at the next start
    /// without using a try.
    pub const INTERRUPTED: &str = "interrupted";
    /// The user cancelled the job while it ran.
    pub const CANCELLED: &str = "cancelled";
    /// The lease watchdog stopped it: no sign of life for a whole lease.
    pub const LEASE_EXPIRED: &str = "lease_expired";
}

/// The `outcome` values of [`EGRESS_REQUESTS_TOTAL`].
pub mod egress_outcome {
    /// A final response below 400.
    pub const OK: &str = "ok";
    /// A final 4xx.
    pub const CLIENT_ERROR: &str = "client_error";
    /// A final 5xx.
    pub const SERVER_ERROR: &str = "server_error";
    /// The egress policy (or the proxy) refused a URL, or a redirect went
    /// past the limit: nothing reached the destination.
    pub const REFUSED: &str = "refused";
    /// The request took longer than its timeout.
    pub const TIMEOUT: &str = "timeout";
    /// The connection or the exchange failed.
    pub const FAILED: &str = "failed";
    /// Every value.
    pub const ALL: [&str; 6] = [OK, CLIENT_ERROR, SERVER_ERROR, REFUSED, TIMEOUT, FAILED];
}

/// The `outcome` values of [`MEDIA_FETCH_TOTAL`] (`crate::outbound::FetchOutcome`).
pub mod fetch_outcome {
    /// The image is staged in the user's store.
    pub const STORED: &str = "stored";
    /// The URL's signature expired (no request, or the CDN's expiry answer).
    pub const EXPIRED: &str = "expired";
    /// 404, 410 or 451.
    pub const GONE: &str = "gone";
    /// The CDN refused the server: a breaker sample.
    pub const BLOCKED: &str = "blocked";
    /// A 5xx, a timeout, a connection failure.
    pub const TRANSIENT: &str = "transient";
    /// The server cannot archive this URL.
    pub const REJECTED: &str = "rejected";
    /// The host group's breaker is open: nothing was sent.
    pub const BREAKER_OPEN: &str = "breaker_open";
    /// Every value.
    pub const ALL: [&str; 7] = [
        STORED,
        EXPIRED,
        GONE,
        BLOCKED,
        TRANSIENT,
        REJECTED,
        BREAKER_OPEN,
    ];
    /// The `host_group` of a URL outside the three CDN host groups.
    pub const NO_GROUP: &str = "none";
}

/// The `provider_kind` values of [`AI_REQUESTS_TOTAL`]: `operator` for the
/// operator's node, else the protocol of a user's provider. A fixed set, so
/// the label never carries a per-user value.
pub mod ai_provider_kind {
    /// The operator's own node (L15).
    pub const OPERATOR: &str = "operator";
    /// A user's OpenAI-compatible provider.
    pub const OPENAI_COMPATIBLE: &str = "openai_compatible";
    /// A user's Anthropic provider.
    pub const ANTHROPIC: &str = "anthropic";
    /// A whisper.cpp server (dictation).
    pub const WHISPER_CPP: &str = "whisper_cpp";
    /// Every value.
    pub const ALL: [&str; 4] = [OPERATOR, OPENAI_COMPATIBLE, ANTHROPIC, WHISPER_CPP];
}

/// The `task` values of [`AI_REQUESTS_TOTAL`] ([`crate::ai::Task`]).
pub mod ai_task {
    /// Cataloging a social post.
    pub const CATALOG: &str = "catalog";
    /// Screenshot quality control of a website capture.
    pub const QC: &str = "qc";
    /// Chat search.
    pub const CHAT: &str = "chat";
    /// Suggestion chips.
    pub const SUGGEST: &str = "suggest";
    /// A tag cluster refine run.
    pub const CLUSTER: &str = "cluster";
    /// A tag alias run.
    pub const ALIAS: &str = "alias";
    /// Embeddings.
    pub const EMBED: &str = "embed";
    /// Dictation (speech to text).
    pub const STT: &str = "stt";
    /// Every value.
    pub const ALL: [&str; 8] = [CATALOG, QC, CHAT, SUGGEST, CLUSTER, ALIAS, EMBED, STT];
}

/// The `outcome` values of [`AI_REQUESTS_TOTAL`]: `ok`, or the kind of the
/// [`shelfy_ai::ErrorKind`] that ended the call.
pub mod ai_outcome {
    /// The call succeeded.
    pub const OK: &str = "ok";
    /// Every value: `ok` and the error kinds callers act on.
    pub const ALL: [&str; 11] = [
        OK,
        "offline",
        "invalid_key",
        "rate_limited",
        "quota_exhausted",
        "transient",
        "bad_request",
        "refused",
        "schema_invalid",
        "unsupported",
        "cancelled",
    ];
}

/// The `direction` values of [`AI_TOKENS_TOTAL`].
pub mod ai_direction {
    /// Prompt tokens.
    pub const INPUT: &str = "input";
    /// Answer tokens (reasoning included).
    pub const OUTPUT: &str = "output";
    /// Every value.
    pub const ALL: [&str; 2] = [INPUT, OUTPUT];
}

/// The `outcome` values of [`INGEST_ITEMS_TOTAL`]: what became of each item
/// of an ingest batch (contract C5).
pub mod ingest_outcome {
    /// A new post.
    pub const INSERTED: &str = "inserted";
    /// A known post that changed.
    pub const UPDATED: &str = "updated";
    /// A known post.
    pub const KNOWN: &str = "known";
    /// An item the sanitizer rejected.
    pub const REJECTED: &str = "rejected";
    /// Every value.
    pub const ALL: [&str; 4] = [INSERTED, UPDATED, KNOWN, REJECTED];
}

/// The `area` values of [`DISK_BYTES`] with their directory: the top-level
/// directories of the data directory (plan §2.5).
pub const DISK_AREAS: &[(&str, &str)] = &[
    ("control", "control"),
    ("users", "users"),
    ("cache", "cache"),
    ("work", "work"),
    ("backup_staging", "backup-staging"),
];
/// The `area` of anything else at the top of the data directory (the dev
/// mailbox, a stray file).
pub const OTHER_AREA: &str = "other";

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Installs the process-wide recorder on the first call; every call returns
/// its handle.
pub fn install() -> PrometheusHandle {
    HANDLE
        .get_or_init(|| {
            let buckets = [
                (HTTP_REQUEST_DURATION_SECONDS, DURATION_BUCKETS),
                (MEDIA_REQUEST_DURATION_SECONDS, DURATION_BUCKETS),
                (JOB_DURATION_SECONDS, JOB_DURATION_BUCKETS),
                (RENDITION_BYTES, RENDITION_BUCKETS),
                (ARCHIVE_COVER_LATENCY_SECONDS, COVER_LATENCY_BUCKETS),
                (SYNC_RUN_PAGES, SYNC_RUN_PAGES_BUCKETS),
            ];
            let recorder = buckets
                .into_iter()
                .fold(PrometheusBuilder::new(), |builder, (name, buckets)| {
                    builder
                        .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
                        .expect("the bucket lists are not empty")
                })
                .build_recorder();
            let handle = recorder.handle();
            if metrics::set_global_recorder(recorder).is_err() {
                tracing::warn!("another metrics recorder is installed; /metrics stays empty");
            }
            describe();
            metrics::gauge!(BUILD_INFO, "version" => crate::VERSION).set(1.0);
            handle
        })
        .clone()
}

fn describe() {
    metrics::describe_counter!(GC_OBJECTS_DELETED_TOTAL, "Unreferenced objects collected.");
    metrics::describe_counter!(
        GC_BYTES_FREED_TOTAL,
        metrics::Unit::Bytes,
        "Bytes of collected object rows."
    );
    metrics::describe_histogram!(
        "shelfy_capture_duration_seconds",
        metrics::Unit::Seconds,
        "Capture service duration, by closed outcome."
    );
    metrics::describe_histogram!(
        "shelfy_capture_peak_rss_bytes",
        metrics::Unit::Bytes,
        "Capture service peak RSS, clamped to two GiB."
    );
    metrics::describe_histogram!(
        "shelfy_capture_bytes",
        metrics::Unit::Bytes,
        "Capture service artifact bytes, clamped to eighty MiB."
    );
    metrics::describe_counter!(
        HTTP_REQUESTS_TOTAL,
        "HTTP requests by route template, method and status."
    );
    metrics::describe_histogram!(
        HTTP_REQUEST_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Time from request to response headers, by route template."
    );
    metrics::describe_histogram!(
        MEDIA_REQUEST_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Time from request to response headers of GET /media/{file}, by variant."
    );
    metrics::describe_gauge!(SSE_CONNECTIONS, "Open realtime event streams.");
    metrics::describe_gauge!(
        JOBS,
        "Jobs the scheduler holds, by kind and state (ready, delayed, running)."
    );
    metrics::describe_gauge!(
        JOB_OLDEST_QUEUED_SECONDS,
        metrics::Unit::Seconds,
        "How long the oldest due job of an unpaused queue has waited, by kind."
    );
    metrics::describe_histogram!(
        JOB_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Duration of job attempts, by kind and outcome."
    );
    metrics::describe_gauge!(
        DISK_BYTES,
        metrics::Unit::Bytes,
        "Bytes of the files under each area of the data directory."
    );
    metrics::describe_gauge!(OPEN_USER_DBS, "User libraries open in the handle cache.");
    metrics::describe_counter!(
        SQLITE_BUSY_TOTAL,
        "SQLite calls that gave up on a lock (SQLITE_BUSY or SQLITE_LOCKED)."
    );
    metrics::describe_histogram!(
        RENDITION_BYTES,
        metrics::Unit::Bytes,
        "Size of each rendition written, by variant."
    );
    metrics::describe_counter!(
        EGRESS_REQUESTS_TOTAL,
        "Outbound HTTP requests, by purpose and outcome; a redirect chain counts once."
    );
    metrics::describe_counter!(
        MEDIA_FETCH_TOTAL,
        "CDN fetches of the archive, by host group and outcome."
    );
    metrics::describe_counter!(
        AI_REQUESTS_TOTAL,
        "AI calls through the service, by provider kind, task and outcome."
    );
    metrics::describe_counter!(
        AI_TOKENS_TOTAL,
        "Prompt and answer tokens providers reported, by direction."
    );
    metrics::describe_gauge!(
        BREAKER_OPEN,
        "1 while a host group's breaker is open or half-open, else 0."
    );
    metrics::describe_histogram!(
        ARCHIVE_COVER_LATENCY_SECONDS,
        metrics::Unit::Seconds,
        "Time from a post's insert to its cover stored by the archive, by platform."
    );
    metrics::describe_gauge!(
        ARCHIVE_BACKLOG,
        "Items left to the server's archive, by platform, as the drains last counted them."
    );
    metrics::describe_counter!(
        INGEST_ITEMS_TOTAL,
        "Items of ingest batches, by platform and outcome (inserted, updated, known, rejected)."
    );
    metrics::describe_histogram!(
        SYNC_RUN_PAGES,
        "Pages a finished sync run scanned, by platform and trigger."
    );
    metrics::describe_gauge!(BUILD_INFO, "Always 1; the label carries the version.");
}

/// The router of the metrics listener: `GET /metrics` only.
pub fn router(handle: PrometheusHandle) -> Router {
    Router::new().route(
        "/metrics",
        get(move || {
            let body = handle.render();
            async move { ([(header::CONTENT_TYPE, PROMETHEUS_TEXT)], body) }
        }),
    )
}

/// Records one handled request (called by [`super::http::observe`]).
pub fn record_http_request(route: &str, method: &Method, status: StatusCode, elapsed: Duration) {
    metrics::counter!(
        HTTP_REQUESTS_TOTAL,
        "route" => route.to_owned(),
        "method" => method_label(method),
        "status" => status.as_str().to_owned(),
    )
    .increment(1);
    metrics::histogram!(HTTP_REQUEST_DURATION_SECONDS, "route" => route.to_owned())
        .record(elapsed.as_secs_f64());
}

/// The route template of the media route, whose latency [`record_media_request`] splits.
pub const MEDIA_ROUTE: &str = "/media/{file}";
/// `variant` of a media file that is not a valid object name.
pub const OTHER_VARIANT: &str = "other";
/// `variant` of a stored object's master.
pub const ORIGINAL_VARIANT: &str = "original";

/// The `variant` label of a request for `file`: a rendition's suffix
/// (`g480`), [`ORIGINAL_VARIANT`] for a master, [`OTHER_VARIANT`] for any
/// other name. A fixed set, so the raw name never reaches a label.
#[must_use]
pub fn media_variant(file: &str) -> &'static str {
    use shelfy_media::ObjectName;
    use shelfy_media::name::Variant;
    match ObjectName::parse(file).map(|name| name.variant) {
        Some(Variant::Rendition(rendition)) => rendition.suffix(),
        Some(Variant::Original(_)) => ORIGINAL_VARIANT,
        None => OTHER_VARIANT,
    }
}

/// Records the latency of one `GET /media/{file}` request (called by
/// [`super::http::observe`] with the request path).
pub fn record_media_request(path: &str, elapsed: Duration) {
    let file = path.rsplit('/').next().unwrap_or_default();
    metrics::histogram!(MEDIA_REQUEST_DURATION_SECONDS, "variant" => media_variant(file))
        .record(elapsed.as_secs_f64());
}

/// Records one AI call (called by the AI service). `provider_kind` is one of
/// [`ai_provider_kind`], `task` one of [`ai_task`], `outcome` one of
/// [`ai_outcome`]; all are fixed sets, so no label carries a per-user value.
/// `input_tokens` and `output_tokens` are added when the provider reported
/// them (both 0 otherwise, which records nothing).
pub fn record_ai_request(
    provider_kind: &'static str,
    task: &'static str,
    outcome: &'static str,
    input_tokens: u64,
    output_tokens: u64,
) {
    metrics::counter!(
        AI_REQUESTS_TOTAL,
        "provider_kind" => provider_kind,
        "task" => task,
        "outcome" => outcome,
    )
    .increment(1);
    if input_tokens > 0 {
        metrics::counter!(AI_TOKENS_TOTAL, "direction" => ai_direction::INPUT)
            .increment(input_tokens);
    }
    if output_tokens > 0 {
        metrics::counter!(AI_TOKENS_TOTAL, "direction" => ai_direction::OUTPUT)
            .increment(output_tokens);
    }
}

/// Records one ended attempt of a job of `kind` (called by the scheduler,
/// which knows the [`job_outcome`]).
pub fn record_job_attempt(kind: &'static str, outcome: &'static str, elapsed: Duration) {
    metrics::histogram!(JOB_DURATION_SECONDS, "kind" => kind, "outcome" => outcome)
        .record(elapsed.as_secs_f64());
}

/// Samples the gauges of the server's state: the open streams and user
/// libraries, the lock failures so far, the scheduler's queues and the age
/// of the oldest due job of each kind (one indexed query per kind on the
/// control database). The blocking parts run on the blocking pool.
pub async fn sample(state: &AppState) {
    metrics::gauge!(SSE_CONNECTIONS).set(state.events().connections() as f64);
    metrics::counter!(SQLITE_BUSY_TOTAL).absolute(shelfy_core::db::sqlite_busy_total());
    let user_dbs = Arc::clone(state.user_dbs());
    match tokio::task::spawn_blocking(move || user_dbs.open_count()).await {
        Ok(open) => metrics::gauge!(OPEN_USER_DBS).set(open as f64),
        Err(err) => tracing::warn!(error = %err, "counting the open libraries failed"),
    }

    let jobs = state.jobs();
    for stats in jobs.stats() {
        for (job_state, count) in [
            (job_state::READY, stats.ready),
            (job_state::DELAYED, stats.delayed),
            (job_state::RUNNING, stats.running),
        ] {
            metrics::gauge!(JOBS, "kind" => stats.kind, "state" => job_state).set(count as f64);
        }
    }
    let kinds: Vec<&'static str> = jobs.registry().kinds().map(Kind::name).collect();
    if kinds.is_empty() {
        return;
    }
    let now = jobs.clock().now_ms();
    let control = Arc::clone(state.control());
    let oldest =
        blocking(move || control.read(|conn| job_rows::oldest_due(conn, &kinds, now))).await;
    match oldest {
        Ok(oldest) => {
            for (kind, run_at) in oldest {
                let waited_ms = run_at.map_or(0, |at| now.saturating_sub(at).max(0));
                metrics::gauge!(JOB_OLDEST_QUEUED_SECONDS, "kind" => kind)
                    .set(waited_ms as f64 / 1000.0);
            }
        }
        Err(err) => tracing::warn!(error = %err, "sampling the oldest queued jobs failed"),
    }
}

/// Measures the data directory ([`measure_disk`]) on the blocking pool and
/// sets [`DISK_BYTES`]. The [`USERS_AREA`] figure also goes to the media
/// budget ([`crate::quota::Quotas::record_users_sample`]).
pub async fn sample_disk(state: &AppState) {
    let root = state.config().data_dir.root().to_path_buf();
    // Taken before the walk: what is stored during it counts twice until the
    // next sample, never zero times.
    let mark = state.quota().sample_mark();
    match tokio::task::spawn_blocking(move || measure_disk(&root)).await {
        Ok(areas) => {
            for &(area, bytes) in &areas {
                metrics::gauge!(DISK_BYTES, "area" => area).set(bytes as f64);
                if area == USERS_AREA {
                    state.quota().record_users_sample(bytes, mark);
                }
            }
        }
        Err(err) => tracing::warn!(error = %err, "measuring the data directory failed"),
    }
}

/// The `area` of the user libraries and their media (`users/`).
pub const USERS_AREA: &str = "users";

/// The bytes of the files under `area` (one of [`DISK_AREAS`]) of the data
/// directory `root`, measured as [`measure_disk`] does; 0 for an unknown
/// area or a missing directory. Blocking.
#[must_use]
pub fn area_bytes(root: &Path, area: &str) -> u64 {
    DISK_AREAS
        .iter()
        .find(|&&(name, _)| name == area)
        .map_or(0, |&(_, dir)| tree_bytes(&root.join(dir)))
}

/// The bytes of the files under each area of the data directory `root`:
/// every [`DISK_AREAS`] entry and [`OTHER_AREA`], 0 when empty or missing.
/// Sizes are the files' lengths (`du --apparent-size`), as quotas count
/// them. Symbolic links inside an area are not followed; files that vanish
/// during the walk are skipped. Blocking.
#[must_use]
pub fn measure_disk(root: &Path) -> Vec<(&'static str, u64)> {
    let mut totals: Vec<(&'static str, u64)> = DISK_AREAS
        .iter()
        .map(|&(area, _)| (area, 0))
        .chain([(OTHER_AREA, 0)])
        .collect();
    let Ok(entries) = fs::read_dir(root) else {
        return totals;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let area = DISK_AREAS
            .iter()
            .find(|&&(_, dir)| name == dir)
            .map_or(OTHER_AREA, |&(area, _)| area);
        let bytes = tree_bytes(&entry.path());
        if let Some(total) = totals.iter_mut().find(|(a, _)| *a == area) {
            total.1 = total.1.saturating_add(bytes);
        }
    }
    totals
}

/// The bytes of the files at or under `path`. A link at `path` itself is
/// followed (an area may live on another volume); links below it are not.
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = fs::metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return if meta.is_file() { meta.len() } else { 0 };
    }
    let mut total = 0_u64;
    let mut dirs = vec![path.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // Not followed: `DirEntry::metadata` describes a link itself.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                dirs.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Standard methods keep their name; anything else is `OTHER`, so a client
/// cannot grow the label set.
fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

/// Capture metrics contain only aggregate, clamped service reports.
pub fn capture_report(outcome: &'static str, duration_ms: f64, peak_rss: f64, bytes: f64) {
    fn bounded(value: f64, max: f64) -> f64 {
        if value.is_finite() {
            value.clamp(0.0, max)
        } else {
            0.0
        }
    }
    metrics::histogram!("shelfy_capture_duration_seconds","outcome"=>outcome)
        .record(bounded(duration_ms, 660_000.0) / 1000.0);
    metrics::histogram!("shelfy_capture_peak_rss_bytes")
        .record(bounded(peak_rss, 2.0 * 1024.0 * 1024.0 * 1024.0));
    metrics::histogram!("shelfy_capture_bytes").record(bounded(bytes, 80.0 * 1024.0 * 1024.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_names_map_to_a_bounded_variant() {
        let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(media_variant(&format!("{hex}.g480.webp")), "g480");
        assert_eq!(media_variant(&format!("{hex}.jpg")), ORIGINAL_VARIANT);
        assert_eq!(media_variant(&format!("{hex}.g999.webp")), OTHER_VARIANT);
        assert_eq!(media_variant("../etc/passwd"), OTHER_VARIANT);
    }

    #[test]
    fn unknown_methods_share_one_label() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::from_bytes(b"BREW").unwrap()), "OTHER");
        assert_eq!(method_label(&Method::CONNECT), "OTHER");
    }

    #[test]
    fn the_disk_is_measured_by_area() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, bytes: usize| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![0_u8; bytes]).unwrap();
        };
        write("control/control.sqlite", 4_096);
        write("control/control.sqlite-wal", 1_000);
        write("users/01J9Z3B8K4QW6TFX0V7G2N5RCA/library.sqlite", 8_192);
        write(
            "users/01J9Z3B8K4QW6TFX0V7G2N5RCA/media/ab/x.g480.webp",
            25_000,
        );
        write("work/uploads/01J9.part", 300);
        write("backup-staging/db/control.sqlite", 4_096);
        write("dev-mailbox/1.eml", 50);
        write("stray.txt", 7);
        fs::create_dir_all(root.join("cache")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("users"), root.join("work/loop")).unwrap();

        let areas = measure_disk(root);
        let bytes = |area: &str| areas.iter().find(|(a, _)| *a == area).unwrap().1;
        assert_eq!(bytes("control"), 5_096);
        assert_eq!(bytes("users"), 33_192);
        assert_eq!(bytes("cache"), 0);
        assert_eq!(bytes("work"), 300, "links inside an area are not followed");
        assert_eq!(bytes("backup_staging"), 4_096);
        assert_eq!(bytes("other"), 57);
        assert_eq!(areas.len(), DISK_AREAS.len() + 1, "every area, always");

        let missing = measure_disk(&root.join("missing"));
        assert!(missing.iter().all(|&(_, bytes)| bytes == 0));
        assert_eq!(missing.len(), DISK_AREAS.len() + 1);

        // One area alone, as the media budget measures it.
        assert_eq!(area_bytes(root, USERS_AREA), 33_192);
        assert_eq!(area_bytes(root, "backup_staging"), 4_096);
        assert_eq!(area_bytes(root, "nope"), 0);
        assert_eq!(area_bytes(&root.join("missing"), USERS_AREA), 0);
    }
}
