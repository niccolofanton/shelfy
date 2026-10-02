//! The HTTP surface through the real middleware stack, driven in-process with
//! `tower::ServiceExt`: health, problem errors, request ids, body limits,
//! timeouts, compression, metrics and the OpenAPI endpoint.

mod support;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use shelfy_core::db::{ControlDbConfig, DbError};
use shelfy_server::error::{ApiError, ErrorCode};
use shelfy_server::extract::{Json, Query};
use shelfy_server::limits::RouteLimits;
use shelfy_server::state::AppState;
use shelfy_server::telemetry::http::REQUEST_ID_HEADER;
use shelfy_server::telemetry::metrics;
use shelfy_server::{app, routes};
use support::{TestState, body, get as get_req, is_ulid, json, post_json, problem, send};
use utoipa_axum::router::OpenApiRouter;

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

#[derive(Debug, Deserialize, Serialize)]
struct Named {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Paging {
    limit: u32,
}

async fn echo(Json(named): Json<Named>) -> Json<Named> {
    Json(named)
}

async fn length(body: Bytes) -> String {
    body.len().to_string()
}

async fn paging(Query(paging): Query<Paging>) -> String {
    paging.limit.to_string()
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(10)).await;
    "late"
}

async fn boom() -> &'static str {
    panic!("secret panic message")
}

async fn busy() -> Result<(), ApiError> {
    Err(DbError::ReaderTimeout.into())
}

/// The real application plus test routes in the real limit groups.
fn app_with_test_routes(state: &AppState) -> Router {
    let standard = OpenApiRouter::new()
        .route("/test/echo", post(echo))
        .route("/test/query", get(paging))
        .route("/test/panic", get(boom))
        .route("/test/busy", get(busy));
    let ingest = OpenApiRouter::new().route("/test/ingest", post(length));
    let short = OpenApiRouter::new().route("/test/slow", get(slow));
    let routes = routes::router()
        .merge(RouteLimits::STANDARD.apply(standard))
        .merge(RouteLimits::INGEST.apply(ingest))
        .merge(
            RouteLimits {
                body_bytes: KIB,
                timeout: Some(Duration::from_millis(100)),
            }
            .apply(short),
        );
    app::build(state.clone(), routes)
}

/// A JSON body `{"name": "aaa…"}` of exactly `len` bytes.
fn named_body(len: usize) -> Vec<u8> {
    let overhead = r#"{"name":""}"#.len();
    let body = format!(r#"{{"name":"{}"}}"#, "a".repeat(len - overhead));
    assert_eq!(body.len(), len);
    body.into_bytes()
}

#[tokio::test]
async fn health_reports_ok_with_a_database_check() {
    let t = TestState::new();
    let response = send(&t.app(), get_req("/health")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let health = json(response).await;
    assert_eq!(
        health,
        serde_json::json!({
            "status": "ok",
            "version": shelfy_server::VERSION,
            "checks": { "controlDb": "ok" }
        })
    );
}

#[tokio::test]
async fn health_fails_while_the_database_does_not_answer() {
    let t = TestState::with_config(|config| {
        config.control_db = ControlDbConfig {
            readers: 1,
            reader_wait_timeout: Duration::from_millis(100),
            ..ControlDbConfig::default()
        };
    });
    let app = t.app();

    // Hold the only reader, so the check times out waiting for one.
    let control = Arc::clone(t.state.control());
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        control
            .read(|_| {
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok::<_, DbError>(())
            })
            .unwrap();
    });
    held_rx.recv().unwrap();

    let response = send(&app, get_req("/health")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let health = json(response).await;
    assert_eq!(health["status"], "fail");
    assert_eq!(health["checks"]["controlDb"], "fail");

    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let response = send(&app, get_req("/health")).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn unknown_routes_answer_a_not_found_problem() {
    let t = TestState::new();
    for uri in ["/nope", "/api/v1/nope", "/api/v1/nope/ig_1?cursor=x"] {
        let response = send(&t.app(), get_req(uri)).await;
        let problem = problem(response, StatusCode::NOT_FOUND).await;
        assert_eq!(problem.code, ErrorCode::NotFound);
        assert_eq!(problem.title, "Not Found");
        assert_eq!(problem.detail, None);
    }
}

#[tokio::test]
async fn wrong_methods_answer_a_problem_with_allow() {
    let t = TestState::new();
    let response = send(
        &t.app(),
        Request::delete("/health").body(Body::empty()).unwrap(),
    )
    .await;
    let allow = response.headers()[header::ALLOW]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(allow.contains("GET"), "Allow: {allow}");
    let problem = problem(response, StatusCode::METHOD_NOT_ALLOWED).await;
    assert_eq!(problem.code, ErrorCode::MethodNotAllowed);
}

#[tokio::test]
async fn every_response_gets_a_fresh_server_request_id() {
    let t = TestState::new();
    let app = t.app();
    let first = send(&app, get_req("/health")).await;
    let second = send(
        &app,
        Request::get("/nope")
            .header(REQUEST_ID_HEADER, "spoofed-id")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let first = first.headers()[&REQUEST_ID_HEADER].to_str().unwrap();
    let second = second.headers()[&REQUEST_ID_HEADER].to_str().unwrap();
    assert!(is_ulid(first), "{first}");
    assert!(is_ulid(second), "{second}");
    assert_ne!(first, second);
}

#[tokio::test]
async fn bodies_over_the_route_limit_are_refused() {
    let t = TestState::new();
    let app = app_with_test_routes(&t.state);
    let limit = RouteLimits::STANDARD.body_bytes;

    // Announced by Content-Length: refused before the handler runs.
    let oversized = named_body(limit + 1);
    let request = Request::post("/test/echo")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, oversized.len())
        .body(Body::from(oversized.clone()))
        .unwrap();
    let problem_body = problem(send(&app, request).await, StatusCode::PAYLOAD_TOO_LARGE).await;
    assert_eq!(problem_body.code, ErrorCode::PayloadTooLarge);

    // Not announced: the body stops at the limit while being read.
    let response = send(&app, post_json("/test/echo", oversized)).await;
    let problem_body = problem(response, StatusCode::PAYLOAD_TOO_LARGE).await;
    assert_eq!(problem_body.code, ErrorCode::PayloadTooLarge);

    // At the limit: accepted.
    let response = send(&app, post_json("/test/echo", named_body(limit))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json(response).await["name"].as_str().unwrap().len(),
        limit - 11
    );

    // The ingest group allows 8 MiB.
    let request = Request::post("/test/ingest")
        .body(Body::from(vec![b'x'; MIB]))
        .unwrap();
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, MIB.to_string());
    let request = Request::post("/test/ingest")
        .body(Body::from(vec![b'x'; RouteLimits::INGEST.body_bytes + 1]))
        .unwrap();
    problem(send(&app, request).await, StatusCode::PAYLOAD_TOO_LARGE).await;
}

#[tokio::test]
async fn handlers_over_their_time_limit_answer_timeout() {
    let t = TestState::new();
    let app = app_with_test_routes(&t.state);
    let started = std::time::Instant::now();
    let response = send(&app, get_req("/test/slow")).await;
    let problem = problem(response, StatusCode::GATEWAY_TIMEOUT).await;
    assert_eq!(problem.code, ErrorCode::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn rejected_extractors_answer_problems() {
    let t = TestState::new();
    let app = app_with_test_routes(&t.state);

    let response = send(&app, post_json("/test/echo", r#"{"name":"#)).await;
    let syntax = problem(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(syntax.code, ErrorCode::BadRequest);
    assert!(syntax.detail.is_some());

    let request = Request::post("/test/echo")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(r#"{"name":"a"}"#))
        .unwrap();
    let media = problem(
        send(&app, request).await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
    )
    .await;
    assert_eq!(media.code, ErrorCode::UnsupportedMediaType);

    let response = send(&app, post_json("/test/echo", r#"{"nom":"a"}"#)).await;
    let shape = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(shape.code, ErrorCode::ValidationFailed);

    let response = send(&app, get_req("/test/query?limit=lots")).await;
    let query = problem(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(query.code, ErrorCode::BadRequest);

    let response = send(&app, get_req("/test/query?limit=5")).await;
    assert_eq!(body(response).await, "5");
}

#[tokio::test]
async fn panics_and_database_contention_answer_problems() {
    let t = TestState::new();
    let app = app_with_test_routes(&t.state);

    let response = send(&app, get_req("/test/panic")).await;
    let panic = problem(response, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert_eq!(panic.code, ErrorCode::Internal);
    assert_eq!(
        panic.detail, None,
        "a panic message never reaches the client"
    );

    let response = send(&app, get_req("/test/busy")).await;
    assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    let busy = problem(response, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(busy.code, ErrorCode::Unavailable);
}

#[tokio::test]
async fn large_responses_are_compressed() {
    let t = TestState::new();
    let app = t.app();
    let request = Request::get("/api/v1/openapi.json")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");

    let request = Request::get("/health")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let response = send(&app, request).await;
    assert!(
        response.headers().get(header::CONTENT_ENCODING).is_none(),
        "small bodies stay uncompressed"
    );
}

#[tokio::test]
async fn the_openapi_document_is_served() {
    let t = TestState::new();
    let response = send(&t.app(), get_req("/api/v1/openapi.json")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let served = json(response).await;
    assert_eq!(served, serde_json::to_value(routes::openapi()).unwrap());
    assert_eq!(served["openapi"], "3.1.0");
}

#[tokio::test]
async fn metrics_are_served_only_by_the_metrics_router() {
    let handle = metrics::install();
    let t = TestState::new();
    let app = t.app();
    assert_eq!(
        send(&app, get_req("/health")).await.status(),
        StatusCode::OK
    );

    problem(send(&app, get_req("/metrics")).await, StatusCode::NOT_FOUND).await;

    let metrics_app = metrics::router(handle);
    let response = send(&metrics_app, get_req("/metrics")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        metrics::PROMETHEUS_TEXT
    );
    let text = String::from_utf8(body(response).await.to_vec()).unwrap();
    for series in [
        r#"shelfy_http_requests_total{route="/health",method="GET",status="200"}"#,
        r#"shelfy_http_requests_total{route="unmatched",method="GET",status="404"}"#,
        r#"shelfy_http_request_duration_seconds_bucket{route="/health",le="0.005"}"#,
        r#"shelfy_build_info{version=""#,
    ] {
        assert!(text.contains(series), "missing {series} in\n{text}");
    }
    let other = send(&metrics_app, get_req("/health")).await;
    assert_eq!(other.status(), StatusCode::NOT_FOUND);
}
