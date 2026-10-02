//! `shelfy-server healthcheck`, the binary the compose healthcheck runs: exit
//! 0 for a healthy server, 1 for an unhealthy one or none at all.

mod support;

use std::net::SocketAddr;
use std::process::Output;

use axum::Json;
use axum::http::StatusCode;
use axum::routing::get;
use serde_json::json;
use shelfy_server::serve::Server;
use shelfy_server::telemetry::metrics;
use support::TestState;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Runs the binary with `args` and `SHELFY_LISTEN_ADDR` set to `listen`.
async fn healthcheck(listen: String, args: &[&str]) -> Output {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_shelfy-server"))
            .arg("healthcheck")
            .args(args)
            .env("SHELFY_LISTEN_ADDR", listen)
            .output()
            .expect("run shelfy-server healthcheck")
    })
    .await
    .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_server_is_healthy_until_it_stops() {
    let TestState { dir: _dir, state } = TestState::new();
    let app = shelfy_server::app::app(state.clone());
    let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = Server::new(state, app, api, metrics_listener, metrics::install());
    let addr = server.api_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    // As in the container: the listener comes from SHELFY_LISTEN_ADDR, and an
    // unspecified address is probed on the loopback.
    for listen in [addr.to_string(), format!("0.0.0.0:{}", addr.port())] {
        let output = healthcheck(listen.clone(), &[]).await;
        assert!(
            output.status.success(),
            "{listen}: {}",
            text(&output.stderr)
        );
        let stdout = text(&output.stdout);
        assert!(stdout.starts_with("ok: shelfy-server "), "{stdout}");
    }

    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    let output = healthcheck(addr.to_string(), &["--timeout", "2"]).await;
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("cannot connect"), "{stderr}");
}

/// A stand-in server whose `/health` answers `status` with a failed check.
async fn failing_server(status: StatusCode) -> SocketAddr {
    let body = json!({ "status": "fail", "version": "0.1.0", "checks": { "controlDb": "fail" } });
    let router = axum::Router::new().route(
        "/health",
        get(move || {
            let body = body.clone();
            async move { (status, Json(body)) }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_check_exits_one() {
    for status in [StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK] {
        let addr = failing_server(status).await;
        let output = healthcheck(addr.to_string(), &[]).await;
        assert_eq!(output.status.code(), Some(1), "{status}");
        let stderr = text(&output.stderr);
        assert!(
            stderr.contains(&format!("unhealthy: HTTP {}", status.as_u16())),
            "{stderr}"
        );
    }
}
