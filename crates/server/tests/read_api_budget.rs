//! Latency of the read API on a synthetic library the size of the reference
//! one (6k posts, plan Appendix C), against the §6.2 budgets.
//!
//! Server time is measured in-process, request in to body out: routing, the
//! middleware stack, SQLite and JSON. §6.2 sets the `GET /posts` budget
//! (60-item page: p95 ≤ 40 ms, p99 ≤ 100 ms) for a 20k library; P1-05 runs that
//! size with `admin bench` on release builds. This test keeps the list path
//! inside the budget on every `cargo test`, debug builds included. The search
//! and detail numbers are printed for the record (run with `--nocapture`).

mod support;

use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use serde_json::Value;
use support::library::{ALICE, synthetic_library};
use support::{TestState, body, get, send};

/// §6.2: `GET /posts`, one 60-item page.
const LIST_P95: Duration = Duration::from_millis(40);
const LIST_P99: Duration = Duration::from_millis(100);
/// §6.2: `GET /posts/{key}`.
const DETAIL_P95: Duration = Duration::from_millis(15);

/// Library size: the reference library has 6,138 posts.
const POSTS: usize = 6_000;

/// Times one request; returns the time and the JSON body.
async fn timed(app: &Router, uri: &str) -> (Duration, Value) {
    let started = Instant::now();
    let response = send(app, get(uri)).await;
    let status = response.status();
    let bytes = body(response).await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK, "{uri}");
    (elapsed, serde_json::from_slice(&bytes).expect("JSON"))
}

/// Nearest-rank percentile `p` (0–100) of `samples`.
fn percentile(samples: &mut [Duration], p: usize) -> Duration {
    samples.sort_unstable();
    let rank = (samples.len() * p).div_ceil(100).max(1);
    samples[rank - 1]
}

fn report(name: &str, samples: &mut [Duration]) -> (Duration, Duration) {
    let (p50, p95, p99) = (
        percentile(samples, 50),
        percentile(samples, 95),
        percentile(samples, 99),
    );
    eprintln!(
        "{name}: {} requests, p50 {p50:.2?}, p95 {p95:.2?}, p99 {p99:.2?}, max {:.2?}",
        samples.len(),
        samples.last().copied().unwrap_or_default()
    );
    (p95, p99)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_read_api_meets_the_budgets_on_a_6k_library() {
    let t = TestState::new();
    let keys = t.write(ALICE, |tx| synthetic_library(tx, POSTS, 42)).await;
    assert_eq!(keys.len(), POSTS);
    let app = t.app_as(ALICE);
    // Warm-up: the first request opens a reader and prepares the statements.
    timed(&app, "/api/v1/posts?includeTotal=true").await;

    // Browsing: every page of the library, newest first, then the first pages
    // of each filter and of the oldest order.
    let mut list = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = 0;
    loop {
        let uri = match &cursor {
            Some(c) => format!("/api/v1/posts?cursor={c}"),
            None => "/api/v1/posts?includeTotal=true".to_owned(),
        };
        let (elapsed, page) = timed(&app, &uri).await;
        list.push(elapsed);
        seen += page["items"].as_array().unwrap().len();
        match page["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen, POSTS);
    for filters in [
        "sort=oldest",
        "platform=instagram",
        "platform=twitter&includeTotal=true",
        "source=web",
        "mediaType=video",
        "mediaType=carousel&mediaType=images",
        "stored=yes&includeTotal=true",
        "stored=no",
        "aiTagged=no",
        "tag=design",
        "trash=1",
    ] {
        let mut cursor: Option<String> = None;
        for _ in 0..3 {
            let uri = match &cursor {
                Some(c) => format!("/api/v1/posts?{filters}&cursor={c}"),
                None => format!("/api/v1/posts?{filters}"),
            };
            let (elapsed, page) = timed(&app, &uri).await;
            list.push(elapsed);
            match page["nextCursor"].as_str() {
                Some(c) => cursor = Some(c.to_owned()),
                None => break,
            }
        }
    }

    // Search (§6.2: p95 ≤ 60 ms in release; recorded here).
    let mut search = Vec::new();
    for q in [
        "lampada",
        "design studio",
        "the",
        "poster vintage",
        "a",
        "kitchen marble glass",
        "typography",
    ] {
        let q = q.replace(' ', "%20");
        let (elapsed, _) = timed(&app, &format!("/api/v1/search?q={q}")).await;
        search.push(elapsed);
        let (elapsed, _) = timed(&app, &format!("/api/v1/posts?q={q}&includeTotal=true")).await;
        search.push(elapsed);
    }

    // Detail.
    let mut detail = Vec::new();
    for key in keys.iter().step_by(POSTS / 100) {
        let (elapsed, post) = timed(&app, &format!("/api/v1/posts/{key}")).await;
        assert_eq!(post["key"], key.as_str());
        detail.push(elapsed);
    }

    let (list_p95, list_p99) = report("GET /posts", &mut list);
    report("GET /search and /posts?q=", &mut search);
    let (detail_p95, _) = report("GET /posts/{key}", &mut detail);
    assert!(
        list_p95 <= LIST_P95,
        "GET /posts p95 {list_p95:?} over the {LIST_P95:?} budget"
    );
    assert!(
        list_p99 <= LIST_P99,
        "GET /posts p99 {list_p99:?} over the {LIST_P99:?} budget"
    );
    assert!(
        detail_p95 <= DETAIL_P95,
        "GET /posts/{{key}} p95 {detail_p95:?} over the {DETAIL_P95:?} budget"
    );
}
