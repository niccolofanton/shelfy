//! Capture peers cannot reach public, authenticated, fallback or metrics routes.
mod support;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use shelfy_server::{net::IpNet, serve::Server, telemetry::metrics};
use std::net::SocketAddr;
use support::{TestState, get, send};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

fn peer_request(path: &str, peer: &str, forwarded: &str) -> Request<Body> {
    let mut request = get(path);
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    request
        .headers_mut()
        .insert("cf-connecting-ip", forwarded.parse().unwrap());
    request.headers_mut().insert(
        "authorization",
        "Bearer synthetic-capture-token".parse().unwrap(),
    );
    request
}

#[tokio::test]
async fn capture_socket_peers_are_denied_before_routing_and_forwarded_headers() {
    let t = TestState::with_config(|config| {
        config.capture_subnet = Some(IpNet::parse("10.134.0.0/24").unwrap());
        config.trusted_proxies =
            shelfy_server::net::TrustedProxies::parse("10.134.0.0/24").unwrap();
    });
    for peer in ["10.134.0.4:1234", "[::ffff:10.134.0.4]:1234"] {
        for path in ["/health", "/api/v1/me", "/metrics", "/unknown.js"] {
            assert_eq!(
                send(&t.app(), peer_request(path, peer, "8.8.8.8"))
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
    }
    assert_eq!(
        send(
            &t.app(),
            peer_request("/health", "10.135.0.4:1234", "10.134.0.4")
        )
        .await
        .status(),
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_real_listeners_attach_the_peer_and_refuse_capture() {
    let TestState { dir: _dir, state } = TestState::with_config(|config| {
        config.capture_subnet = Some(IpNet::parse("127.0.0.0/8").unwrap());
    });
    let server = Server::new(
        state.clone(),
        shelfy_server::app::app(state),
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        metrics::install(),
    );
    let addresses = [
        (server.api_addr().unwrap(), "/health"),
        (server.metrics_addr().unwrap(), "/metrics"),
    ];
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    for (address, path) in addresses {
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nCF-Connecting-IP: 8.8.8.8\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(response.contains("forbidden"));
    }
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
}
