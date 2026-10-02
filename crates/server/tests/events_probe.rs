//! The §6.2 budget "write → SSE delivered: p95 ≤ 300 ms" (P1-01), probed
//! in-process on real sockets: the server on 127.0.0.1 with its 2 async
//! workers, SSE clients reading raw HTTP over TCP, and each write made the way
//! P1-03's `PATCH /posts/{key}` will make it: a note edit through the core's
//! write path on the blocking pool, then the `posts.changed` and
//! `stats.changed` publish calls.
//!
//! A sample runs from just before the write's transaction to the moment its
//! client reads the event. Writes are isolated, as interactive edits are: one
//! per user per round, rounds 2.25 s apart, so every write is the leading
//! edge of its throttle (G1). The users write spread over the first second of
//! each round, all streams open at once. The numbers are printed for the
//! record (run with `--nocapture`).

mod support;

use std::time::Duration;

use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use shelfy_core::repo::posts::{self, NewPost, UserContentPatch};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_server::current_user::CurrentUser;
use shelfy_server::events::POSTS_WINDOW;
use shelfy_server::events::model::ChangeReason;
use shelfy_server::serve::Server;
use shelfy_server::state::{AppState, blocking};
use shelfy_server::telemetry::metrics;
use shelfy_server::{app, routes};
use support::TestState;
use support::library::NOW;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::Instant;

/// §6.2: write → SSE delivered, p95.
const BUDGET: Duration = Duration::from_millis(300);
/// Users writing at the same time, each with an open stream.
const USERS: usize = 50;
/// Writes per user.
const ROUNDS: usize = 2;
/// Between two writes of one user: past the 2 s throttle window.
const SPACING: Duration = Duration::from_millis(2_250);
/// Header of the test-only stand-in for authentication.
const PROBE_USER: &str = "x-probe-user";

/// Test-only authentication: the user named by `x-probe-user`.
async fn probe_user(mut request: Request, next: Next) -> Response {
    let user = request
        .headers()
        .get(PROBE_USER)
        .and_then(|value| value.to_str().ok())
        .map(CurrentUser::new);
    if let Some(user) = user {
        request.extensions_mut().insert(user);
    }
    next.run(request).await
}

fn user_id(n: usize) -> String {
    format!("01J9Z3B8K4QW6TFX0V7G2N{n:04}")
}

fn post_key(user: usize, round: usize) -> String {
    format!("ig_{}", native_id(user, round))
}

fn native_id(user: usize, round: usize) -> String {
    format!("{}{round:02}", 1_000 + user)
}

/// An SSE client on a raw TCP connection.
struct Client {
    stream: TcpStream,
    received: Vec<u8>,
    /// Bytes of `received` already matched.
    consumed: usize,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr, user: &str) -> Self {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET /api/v1/events HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\
             {PROBE_USER}: {user}\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut client = Self {
            stream,
            received: Vec::new(),
            consumed: 0,
        };
        client.wait_for("HTTP/1.1 200 OK").await;
        client.wait_for("event: hello").await;
        client
    }

    /// Reads until `text` arrives after what was matched before.
    async fn wait_for(&mut self, text: &str) {
        loop {
            if let Some(at) = self.received[self.consumed..]
                .windows(text.len())
                .position(|window| window == text.as_bytes())
            {
                self.consumed += at + text.len();
                return;
            }
            let mut chunk = [0_u8; 4096];
            let n = self.stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "the stream closed while waiting for {text}");
            self.received.extend_from_slice(&chunk[..n]);
        }
    }
}

/// One user's writes; returns the time from each write to its event.
async fn user_writes(
    state: AppState,
    addr: std::net::SocketAddr,
    n: usize,
    start: Instant,
) -> Vec<Duration> {
    let user = user_id(n);
    let mut client = Client::connect(addr, &user).await;
    let offset = Duration::from_secs(1) * u32::try_from(n).unwrap() / u32::try_from(USERS).unwrap();
    let mut samples = Vec::new();
    for round in 0..ROUNDS {
        tokio::time::sleep_until(start + SPACING * u32::try_from(round).unwrap() + offset).await;
        let key = post_key(n, round);
        let started = Instant::now();
        let db = state.user_db(&user).await.unwrap();
        let edited = key.clone();
        blocking(move || {
            db.write(|tx| {
                let id = posts::id_for_key(tx, &edited)?.ok_or(RepoError::NotFound)?;
                let note = UserContentPatch {
                    note: Some(Some(format!("probe round {round}"))),
                    tags: None,
                };
                posts::update_user_content(tx, id, &note, NOW)
            })
        })
        .await
        .unwrap();
        state
            .events()
            .posts_changed(&user, ChangeReason::Edit, Some(vec![key.clone()]));
        state.events().stats_changed(&user);
        client.wait_for(&format!("\"keys\":[\"{key}\"]")).await;
        samples.push(started.elapsed());
    }
    samples
}

/// Nearest-rank percentile `p` (0–100) of sorted `samples`.
fn percentile(samples: &[Duration], p: usize) -> Duration {
    let rank = (samples.len() * p).div_ceil(100).max(1);
    samples[rank - 1]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_reach_an_sse_client_within_the_budget() {
    assert!(SPACING > POSTS_WINDOW, "every write must be a leading edge");
    let TestState { dir: _dir, state } = TestState::new();
    for n in 0..USERS {
        let library = state.user_db(&user_id(n)).await.unwrap();
        library
            .write(|tx| {
                for round in 0..ROUNDS {
                    let post = NewPost::new(
                        post_key(n, round),
                        Platform::Instagram,
                        native_id(n, round),
                        "image",
                        NOW,
                    );
                    posts::insert(tx, &post, NOW)?;
                }
                Ok::<_, RepoError>(())
            })
            .unwrap();
    }

    let application =
        app::build(state.clone(), routes::router()).layer(middleware::from_fn(probe_user));
    let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = Server::new(
        state.clone(),
        application,
        api_listener,
        metrics_listener,
        metrics::install(),
    );
    let addr = server.api_addr().unwrap();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stop_rx.await;
    }));

    let start = Instant::now() + Duration::from_millis(500);
    let users: Vec<_> = (0..USERS)
        .map(|n| tokio::spawn(user_writes(state.clone(), addr, n, start)))
        .collect();
    let mut samples = Vec::new();
    for user in users {
        samples.extend(user.await.unwrap());
    }
    stop_tx.send(()).unwrap();
    running.await.unwrap().unwrap();

    samples.sort_unstable();
    let (p50, p95, max) = (
        percentile(&samples, 50),
        percentile(&samples, 95),
        samples[samples.len() - 1],
    );
    eprintln!(
        "write → event: {} writes by {USERS} users, p50 {p50:.2?}, p95 {p95:.2?}, max {max:.2?}",
        samples.len()
    );
    assert_eq!(samples.len(), USERS * ROUNDS);
    assert!(p95 <= BUDGET, "p95 {p95:?} over the {BUDGET:?} budget");
}
