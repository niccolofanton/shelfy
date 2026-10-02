//! The server on real sockets: the API and metrics listeners serve disjoint
//! routes, and a shutdown lets in-flight requests finish, stops accepting,
//! cancels the shutdown token, and checkpoints and closes the databases
//! (plan §2.3).

mod support;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::routing::get;
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_server::limits::RouteLimits;
use shelfy_server::serve::Server;
use shelfy_server::telemetry::metrics;
use shelfy_server::{app, routes};
use support::TestState;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use utoipa_axum::router::OpenApiRouter;

/// A minimal HTTP/1.1 GET: the status code and the raw response.
async fn http_get(addr: SocketAddr, path: &str) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    Ok((status, text))
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(400)).await;
    "finished"
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listeners_serve_disjoint_routes_and_shut_down_gracefully() {
    let TestState { dir: _dir, state } = TestState::new();
    let token = state.shutdown_token().clone();
    let data = state.config().data_dir.clone();

    // A user database with a committed write sitting in its WAL.
    let user = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let library = state.user_db(user).await.unwrap();
    library
        .write(|tx| {
            collections::create(
                tx,
                &NewCollection {
                    name: "Inbox".into(),
                    ..NewCollection::default()
                },
                1_790_899_200_000,
            )
        })
        .unwrap();
    drop(library);
    let library_wal = data.library_db(user).with_extension("sqlite-wal");
    assert!(std::fs::metadata(&library_wal).unwrap().len() > 0);

    let slow_routes = OpenApiRouter::new().route("/test/slow", get(slow));
    let application = app::build(
        state.clone(),
        routes::router().merge(RouteLimits::STANDARD.apply(slow_routes)),
    );
    let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = Server::new(
        state,
        application,
        api_listener,
        metrics_listener,
        metrics::install(),
    );
    let api = server.api_addr().unwrap();
    let metrics_addr = server.metrics_addr().unwrap();
    assert_ne!(api.port(), metrics_addr.port());

    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stop_rx.await;
    }));

    let (status, body) = http_get(api, "/health").await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""controlDb":"ok""#), "{body}");
    let (status, body) = http_get(metrics_addr, "/metrics").await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("shelfy_http_requests_total"), "{body}");
    let (status, _) = http_get(api, "/metrics").await.unwrap();
    assert_eq!(status, 404, "metrics stay off the API listener");
    let (status, _) = http_get(metrics_addr, "/health").await.unwrap();
    assert_eq!(status, 404, "the metrics listener serves metrics only");

    // A request in flight when the shutdown starts still completes.
    let in_flight = tokio::spawn(http_get(api, "/test/slow"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stopping = Instant::now();
    stop_tx.send(()).unwrap();

    let (status, body) = in_flight.await.unwrap().unwrap();
    assert_eq!(status, 200);
    assert!(body.ends_with("finished"), "{body}");
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("the server stops well within its grace period")
        .unwrap()
        .expect("a clean shutdown");
    assert!(stopping.elapsed() < Duration::from_secs(5));
    assert!(token.is_cancelled(), "jobs and streams were told to stop");

    // Stopped accepting.
    assert!(http_get(api, "/health").await.is_err());
    // Every database was checkpointed and closed: no WAL is left behind.
    for wal in [data.control_db().with_extension("sqlite-wal"), library_wal] {
        let len = std::fs::metadata(&wal).map_or(0, |m| m.len());
        assert_eq!(len, 0, "WAL left behind at {}", wal.display());
    }
}
