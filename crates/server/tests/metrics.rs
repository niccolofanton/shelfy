//! The Prometheus metrics of P1-15 (plan §3.6) after real traffic: every
//! metric is exported with sane values, and no label carries a per-user
//! value.
//!
//! The recorder is process-wide, and the gauges hold whatever the last
//! sample set: the tests of this binary take turns ([`TURN`]).

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use metrics_exporter_prometheus::PrometheusHandle;
use rusqlite::Connection;
use serde_json::json;
use shelfy_media::store::MediaStore;
use shelfy_media::{Digest, Rendition};
use shelfy_server::config::Config;
use shelfy_server::control::jobs::JobRow;
use shelfy_server::events::model::JobState;
use shelfy_server::ids::now_ms;
use shelfy_server::jobs::{NewJob, Registry};
use shelfy_server::outbound::{HostGroup, Purpose};
use shelfy_server::routes;
use shelfy_server::static_files::WebApp;
use shelfy_server::telemetry::metrics::{
    self, DISK_AREAS, OTHER_AREA, egress_outcome, fetch_outcome, job_outcome, job_state, sample,
    sample_disk,
};
use support::auth::{OWNER_EMAIL, owner, post, sign_in, spa, with_session};
use support::jobs::{Probe, kind, mode, modes};
use support::library::ALICE;
use support::{TestState, body, get, post_json, send};
use tokio_util::sync::CancellationToken;

/// The kind of job these tests run.
const KIND: &str = "test.metrics";

const MINUTE: Duration = Duration::from_secs(60);

/// One test at a time samples, renders and reads the recorder.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One series of the exposition: name, labels in order, value.
#[derive(Clone, Debug, PartialEq)]
struct Series {
    name: String,
    labels: Vec<(String, String)>,
    value: f64,
}

/// Parses the Prometheus text format as the exporter writes it.
fn parse(text: &str) -> Vec<Series> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (series, value) = line.rsplit_once(' ').expect("a value");
            let (name, labels) = match series.split_once('{') {
                Some((name, labels)) => {
                    let labels = labels.strip_suffix('}').expect("closed labels");
                    let labels = split_labels(labels);
                    (name.to_owned(), labels)
                }
                None => (series.to_owned(), Vec::new()),
            };
            let value = match value {
                "+Inf" => f64::INFINITY,
                value => value.parse().unwrap_or_else(|_| panic!("{line}")),
            };
            Series {
                name,
                labels,
                value,
            }
        })
        .collect()
}

/// `a="x",b="y"` (values never contain a quote here).
fn split_labels(labels: &str) -> Vec<(String, String)> {
    labels
        .split("\",")
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once("=\"").expect("name=\"value\"");
            (name.to_owned(), value.trim_end_matches('"').to_owned())
        })
        .collect()
}

/// The value of the series `name` whose labels include `labels`.
fn value(all: &[Series], name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    all.iter()
        .find(|s| {
            s.name == name
                && labels
                    .iter()
                    .all(|(k, v)| s.labels.iter().any(|(lk, lv)| lk == k && lv == v))
        })
        .map(|s| s.value)
}

fn rendered(handle: &PrometheusHandle) -> (String, Vec<Series>) {
    handle.run_upkeep();
    let text = handle.render();
    let series = parse(&text);
    (text, series)
}

/// A built web app: just an `index.html`.
fn web_app(dir: &std::path::Path) -> WebApp {
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><title>Shelfy</title>",
    )
    .unwrap();
    WebApp::load(dir).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_metric_is_exported_with_sane_values() {
    let _turn = TURN.lock().await;
    let handle = metrics::install();
    let web = tempfile::tempdir().unwrap();
    let probe = Probe::new();
    let t = TestState::with_config(|config: &mut Config| {
        config.web = Some(web_app(web.path()));
        config.jobs.registry = Registry::new().register(kind(KIND, 1, 1, &probe));
        // A write that meets a held lock fails fast.
        config.user_db.pragmas.busy_timeout = Duration::from_millis(50);
    });
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;

    // HTTP: an API route, the web app's page, an unknown API path.
    let version = with_session(get("/api/v1/version"), &cookie);
    assert_eq!(send(&app, version).await.status(), StatusCode::OK);
    assert_eq!(send(&app, get("/")).await.status(), StatusCode::OK);
    let unknown = send(&app, get("/api/v1/nope")).await.status();
    assert_eq!(unknown, StatusCode::NOT_FOUND);

    // A write that finds the library locked by another connection.
    let stats = with_session(get("/api/v1/stats"), &cookie);
    assert_eq!(send(&app, stats).await.status(), StatusCode::OK);
    let holder = Connection::open(t.data_dir().library_db(&owner_id)).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let create = post_json(
        "/api/v1/collections",
        json!({ "name": "Lamps" }).to_string(),
    );
    let busy = send(&app, spa(&t, create, &cookie)).await.status();
    assert_eq!(busy, StatusCode::SERVICE_UNAVAILABLE);
    holder.execute_batch("ROLLBACK").unwrap();

    // A rendition of 30 KB.
    let media = MediaStore::new(t.data_dir().users_dir())
        .user(&owner_id)
        .unwrap();
    let digest = Digest::of(b"a cover");
    media
        .store_rendition(&digest, Rendition::G480, &[7_u8; 30_000])
        .unwrap();

    // Jobs: one done, one running, one due a minute ago behind it, one later.
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let done = t.enqueue(&owner_id, KIND, mode("ok")).await;
    t.wait_job(&owner_id, done, |job| job.finished_at.is_some())
        .await;
    t.enqueue(&owner_id, KIND, mode("gate")).await;
    probe.wait_starts(2).await;
    let now = now_ms();
    for run_at in [now - 60_000, now + 3_600_000] {
        let job = NewJob::new(owner_id.as_str(), KIND)
            .payload(mode("ok"))
            .run_at(run_at);
        t.state.jobs().enqueue(job).await.unwrap();
    }

    // A realtime stream, open while the gauges are sampled.
    let stream = send(&app, with_session(get("/api/v1/events"), &cookie)).await;
    assert_eq!(stream.status(), StatusCode::OK);
    sample(&t.state).await;
    sample_disk(&t.state).await;
    let (text, all) = rendered(&handle);
    drop(stream);

    let get_value = |name: &str, labels: &[(&str, &str)]| {
        value(&all, name, labels).unwrap_or_else(|| panic!("no {name} {labels:?} in\n{text}"))
    };
    for (name, kind) in [
        ("shelfy_http_requests_total", "counter"),
        ("shelfy_http_request_duration_seconds", "histogram"),
        ("shelfy_sse_connections", "gauge"),
        ("shelfy_jobs", "gauge"),
        ("shelfy_job_oldest_queued_seconds", "gauge"),
        ("shelfy_job_duration_seconds", "histogram"),
        ("shelfy_disk_bytes", "gauge"),
        ("shelfy_open_user_dbs", "gauge"),
        ("shelfy_sqlite_busy_total", "counter"),
        ("shelfy_rendition_bytes", "histogram"),
        ("shelfy_breaker_open", "gauge"),
        ("shelfy_build_info", "gauge"),
    ] {
        assert!(
            text.contains(&format!("# TYPE {name} {kind}\n")),
            "{name} is not a {kind} in\n{text}"
        );
    }

    let route = |route: &str, status: &str| {
        get_value(
            "shelfy_http_requests_total",
            &[("route", route), ("method", "GET"), ("status", status)],
        )
    };
    assert!(route("/api/v1/version", "200") >= 1.0);
    assert!(route("spa", "200") >= 1.0, "the web app's page");
    assert!(route("unmatched", "404") >= 1.0);
    assert!(
        get_value(
            "shelfy_http_request_duration_seconds_bucket",
            &[("route", "/api/v1/version"), ("le", "+Inf")],
        ) >= 1.0
    );

    assert_eq!(get_value("shelfy_sse_connections", &[]), 1.0);
    let jobs = |state: &str| get_value("shelfy_jobs", &[("kind", KIND), ("state", state)]);
    assert_eq!(jobs(job_state::RUNNING), 1.0);
    assert_eq!(jobs(job_state::READY), 1.0);
    assert_eq!(jobs(job_state::DELAYED), 1.0);
    let oldest = get_value("shelfy_job_oldest_queued_seconds", &[("kind", KIND)]);
    assert!((59.0..120.0).contains(&oldest), "{oldest}");
    let succeeded = [("kind", KIND), ("outcome", job_outcome::SUCCEEDED)];
    assert!(get_value("shelfy_job_duration_seconds_count", &succeeded) >= 1.0);

    for area in ["users", "control"] {
        assert!(
            get_value("shelfy_disk_bytes", &[("area", area)]) > 0.0,
            "{area}"
        );
    }
    let areas: BTreeSet<&str> = all
        .iter()
        .filter(|s| s.name == "shelfy_disk_bytes")
        .flat_map(|s| s.labels.iter().map(|(_, v)| v.as_str()))
        .collect();
    assert_eq!(areas.len(), DISK_AREAS.len() + 1, "{areas:?}");
    assert_eq!(
        get_value("shelfy_open_user_dbs", &[]),
        1.0,
        "the owner's library"
    );
    assert!(get_value("shelfy_sqlite_busy_total", &[]) >= 1.0);
    let g480 = [("variant", "g480")];
    assert!(get_value("shelfy_rendition_bytes_count", &g480) >= 1.0);
    let at_35_kb = [("variant", "g480"), ("le", "35000")];
    assert!(get_value("shelfy_rendition_bytes_bucket", &at_35_kb) >= 1.0);
    let at_25_kb = [("variant", "g480"), ("le", "25000")];
    let below = value(&all, "shelfy_rendition_bytes_bucket", &at_25_kb).unwrap_or(0.0);
    assert!(below < get_value("shelfy_rendition_bytes_bucket", &at_35_kb));
    // Every host group's breaker has a series from the start (P2-04).
    for group in HostGroup::ALL {
        let open = get_value("shelfy_breaker_open", &[("host_group", group.label())]);
        assert_eq!(open, 0.0, "{group:?}");
    }

    probe.open(4);
    assert!(
        scheduler
            .stop(tokio::time::Instant::now() + Duration::from_secs(10))
            .await
    );
}

/// Every `outcome` of `shelfy_job_duration_seconds`, on paused time.
#[tokio::test(start_paused = true)]
async fn every_job_outcome_is_recorded() {
    const OUTCOMES: &str = "test.outcomes";
    let _turn = TURN.lock().await;
    let handle = metrics::install();
    let probe = Probe::new();
    let t = TestState::with_jobs(Registry::new().register(kind(OUTCOMES, 8, 8, &probe)));
    t.add_user(ALICE);
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let done = |job: &JobRow| job.finished_at.is_some();

    let ok = t.enqueue(ALICE, OUTCOMES, mode("ok")).await;
    let failed = t.enqueue(ALICE, OUTCOMES, mode("permanent")).await;
    let retried = t
        .enqueue(ALICE, OUTCOMES, modes(&["transient", "ok"]))
        .await;
    let cancelled = t.enqueue(ALICE, OUTCOMES, mode("wait")).await;
    let hung = t.enqueue(ALICE, OUTCOMES, modes(&["hang", "ok"])).await;
    probe.wait_starts(5).await;
    t.state.jobs().cancel(ALICE, cancelled).await.unwrap();
    for id in [ok, failed, retried, hung] {
        t.wait_job(ALICE, id, done).await;
    }
    // A drain yields when its queue is paused; a running job is interrupted
    // by the shutdown.
    let drained = t.enqueue(ALICE, OUTCOMES, mode("drain")).await;
    probe.wait_starts(8).await;
    t.state.jobs().pause(ALICE, OUTCOMES).await.unwrap();
    t.wait_job(ALICE, drained, |job| job.state == JobState::Queued)
        .await;
    t.state.jobs().resume(ALICE, OUTCOMES).await.unwrap();
    t.wait_job(ALICE, drained, done).await;
    t.enqueue(ALICE, OUTCOMES, mode("wait")).await;
    probe.wait_starts(10).await;
    assert!(scheduler.stop(tokio::time::Instant::now() + MINUTE).await);

    let (text, all) = rendered(&handle);
    for outcome in [
        job_outcome::SUCCEEDED,
        job_outcome::FAILED,
        job_outcome::RETRIED,
        job_outcome::REQUEUED,
        job_outcome::INTERRUPTED,
        job_outcome::CANCELLED,
        job_outcome::LEASE_EXPIRED,
    ] {
        let labels = [("kind", OUTCOMES), ("outcome", outcome)];
        let count = value(&all, "shelfy_job_duration_seconds_count", &labels);
        assert!(count >= Some(1.0), "no {outcome} attempt in\n{text}");
    }
    let succeeded = [("kind", OUTCOMES), ("outcome", job_outcome::SUCCEEDED)];
    let count = value(&all, "shelfy_job_duration_seconds_count", &succeeded);
    assert_eq!(
        count,
        Some(4.0),
        "ok, the retry, the hung job's retry, the drain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_label_carries_a_per_user_value() {
    let _turn = TURN.lock().await;
    let handle = metrics::install();
    let probe = Probe::new();
    let t = TestState::with_config(|config: &mut Config| {
        config.jobs.registry = Registry::new().register(kind(KIND, 1, 1, &probe));
    });
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;

    // Requests full of per-user values: ids, keys, an address, search text.
    let planted_key = "ig_PlantedKey4242";
    let planted_query = "plantedquerytext";
    let job = t.enqueue(&owner_id, KIND, mode("ok")).await;
    let paths = [
        format!("/api/v1/posts/{planted_key}"),
        format!("/api/v1/posts?q={planted_query}"),
        format!("/api/v1/search?q={planted_query}"),
        format!("/api/v1/jobs/{job}"),
        format!("/api/v1/nope/{owner_id}?email={OWNER_EMAIL}"),
        format!("/media/{}.g480.webp", "cd".repeat(32)),
        format!("/{owner_id}/{planted_key}"),
    ];
    for path in &paths {
        let response = send(&app, with_session(get(path), &cookie)).await;
        assert_ne!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{path}"
        );
    }
    let cancel = spa(&t, post(&format!("/api/v1/jobs/{job}/cancel")), &cookie);
    send(&app, cancel).await;
    let email = json!({ "email": OWNER_EMAIL }).to_string();
    send(
        &app,
        support::auth::from_spa(&t, post_json("/api/v1/auth/magic-links", email)),
    )
    .await;
    let report = Request::post("/api/v1/client-errors")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "view": "gallery", "message": planted_key }).to_string(),
        ))
        .unwrap();
    let response = send(&app, spa(&t, report, &cookie)).await;
    body(response).await;
    sample(&t.state).await;
    sample_disk(&t.state).await;
    let (text, all) = rendered(&handle);

    for planted in [
        owner_id.as_str(),
        OWNER_EMAIL,
        planted_key,
        planted_query,
        &cookie,
    ] {
        assert!(!text.contains(planted), "{planted} is in a metric:\n{text}");
    }

    let mut templates: BTreeSet<String> = routes::openapi().paths.paths.into_keys().collect();
    templates.extend(["/media/{file}", "spa", "unmatched"].map(str::to_owned));
    let methods = [
        "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "OTHER",
    ];
    let job_states = [job_state::READY, job_state::DELAYED, job_state::RUNNING];
    let outcomes = [
        job_outcome::SUCCEEDED,
        job_outcome::FAILED,
        job_outcome::RETRIED,
        job_outcome::REQUEUED,
        job_outcome::INTERRUPTED,
        job_outcome::CANCELLED,
        job_outcome::LEASE_EXPIRED,
    ];
    let areas: Vec<&str> = DISK_AREAS
        .iter()
        .map(|&(area, _)| area)
        .chain([OTHER_AREA])
        .collect();
    let host_groups: Vec<&str> = HostGroup::ALL
        .map(HostGroup::label)
        .into_iter()
        .chain([fetch_outcome::NO_GROUP])
        .collect();
    let purposes = Purpose::ALL.map(Purpose::label);
    let mut seen = BTreeSet::new();
    for series in &all {
        for (name, value) in &series.labels {
            seen.insert(name.as_str());
            let fine = match name.as_str() {
                "route" => templates.contains(value),
                "method" => methods.contains(&value.as_str()),
                "status" => value.len() == 3 && value.bytes().all(|b| b.is_ascii_digit()),
                "kind" => value.starts_with("test."),
                "state" => job_states.contains(&value.as_str()),
                "outcome" => {
                    outcomes.contains(&value.as_str())
                        || egress_outcome::ALL.contains(&value.as_str())
                        || fetch_outcome::ALL.contains(&value.as_str())
                }
                "area" => areas.contains(&value.as_str()),
                "host_group" => host_groups.contains(&value.as_str()),
                "purpose" => purposes.contains(&value.as_str()),
                "variant" => value == "g480",
                "version" => value == shelfy_server::VERSION,
                "le" | "quantile" => value == "+Inf" || value.parse::<f64>().is_ok(),
                _ => false,
            };
            assert!(fine, "label {name}={value:?} on {}", series.name);
        }
    }
    assert!(seen.contains("route") && seen.contains("kind"), "{seen:?}");
}
