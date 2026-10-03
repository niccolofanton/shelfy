//! The stub itself: deterministic answers, canned answers by key, replay
//! from a recordings directory, latency, the admin routes, offline spells.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use shelfy_ai::direct::DirectTransport;
use shelfy_ai::stub::{Recording, StubConfig, key_of, request_key, write_recording};
use shelfy_ai::{
    CallOptions, ChatRequest, ErrorKind, Message, Part, Provider, ProviderConfig, ProviderKind,
    RetryPolicy, Source, Timeouts, TransportError,
};
use support::*;

fn caption(text: &str) -> ChatRequest {
    ChatRequest::new(
        "m",
        vec![Message::user(vec![Part::text(text), Part::webp(webp())])],
    )
    .with_system("Catalog the post.")
    .with_json(catalog())
}

#[tokio::test]
async fn answers_are_deterministic_and_keyed_by_the_text() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let first = provider.chat(&caption("a lamp"), &options()).await.unwrap();
    let again = provider.chat(&caption("a lamp"), &options()).await.unwrap();
    let other = provider
        .chat(&caption("a chair"), &options())
        .await
        .unwrap();
    assert_eq!(first.text, again.text);
    assert_ne!(first.text, other.text);
    let keys: Vec<String> = stub
        .requests()
        .into_iter()
        .map(|request| request.key.unwrap())
        .collect();
    assert_eq!(keys[0], key_of(&caption("a lamp")));
    assert_eq!(keys[0], request_key(Some("Catalog the post."), ["a lamp"]));
    // One key across protocols and modes.
    let anthropic = operator(&stub, ProviderKind::Anthropic);
    anthropic
        .chat(
            &caption("a lamp"),
            &options().with_structured(shelfy_ai::StructuredMode::JsonObject),
        )
        .await
        .unwrap();
    assert_eq!(
        stub.requests().last().unwrap().key.as_deref(),
        Some(keys[0].as_str())
    );
}

#[tokio::test]
async fn recordings_are_replayed_by_key() {
    let dir = std::env::temp_dir().join(format!("shelfy-ai-recordings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let request = caption("a recorded lamp");
    let recorded =
        r#"{"description":"recorded","general_tags":["a"],"specific_tags":["b"],"language":"it"}"#;
    write_recording(
        &dir,
        &key_of(&request),
        &Recording {
            text: recorded.into(),
        },
    )
    .unwrap();
    let stub = stub_with(StubConfig {
        recordings: Some(dir.clone()),
        ..StubConfig::default()
    })
    .await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let answer = provider
        .chat(&request.clone().streamed(true), &options())
        .await
        .unwrap();
    assert_eq!(answer.json.unwrap()["description"], "recorded");
    let anthropic = operator(&stub, ProviderKind::Anthropic);
    let answer = anthropic
        .chat(
            &request,
            &options().with_structured(shelfy_ai::StructuredMode::Tool),
        )
        .await
        .unwrap();
    assert_eq!(answer.json.unwrap()["language"], "it");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        write_recording(
            &std::env::temp_dir(),
            "../escape",
            &Recording {
                text: String::new()
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn latency_delays_every_answer() {
    let stub = stub_with(StubConfig {
        latency: Duration::from_millis(200),
        ..StubConfig::default()
    })
    .await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let started = Instant::now();
    provider.chat(&caption("x"), &options()).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200));
    stub.set_latency(Duration::ZERO);
    let started = Instant::now();
    provider.chat(&caption("x"), &options()).await.unwrap();
    assert!(started.elapsed() < Duration::from_millis(200));
}

#[tokio::test]
async fn many_calls_in_flight_are_served_together() {
    let stub = stub_with(StubConfig {
        latency: Duration::from_millis(300),
        ..StubConfig::default()
    })
    .await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let started = Instant::now();
    let calls: Vec<_> = (0..16)
        .map(|n| {
            let provider = provider.clone();
            tokio::spawn(async move {
                provider
                    .chat(&caption(&format!("post {n}")), &options())
                    .await
            })
        })
        .collect();
    for call in calls {
        call.await.unwrap().unwrap();
    }
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
}

/// Sends one admin request through the direct transport.
async fn admin(
    stub: &shelfy_ai::stub::Stub,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> u16 {
    use shelfy_ai::transport::{HttpRequest, Transport};
    let request = HttpRequest {
        method: method.parse().unwrap(),
        url: stub.url().join(path).unwrap(),
        headers: http::HeaderMap::new(),
        body: body.to_string().into(),
        egress: shelfy_ai::Egress::Allowlisted,
        connect_timeout: Duration::from_secs(1),
        timeout: Duration::from_secs(5),
    };
    DirectTransport::new()
        .send(request)
        .await
        .unwrap()
        .status
        .as_u16()
}

#[tokio::test]
async fn the_admin_routes_drive_faults_canned_answers_and_offline_spells() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let no_retry = CallOptions::new(Timeouts::new(
        Duration::from_secs(1),
        Duration::from_secs(5),
    ))
    .with_retry(RetryPolicy::NONE);

    let fault = json!({"fault": {"kind": "server_error"}, "times": 1, "endpoint": "chat"});
    assert_eq!(admin(&stub, "POST", "/_stub/faults", fault).await, 204);
    assert_eq!(
        provider
            .chat(&caption("x"), &no_retry)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Transient
    );

    let canned = json!({"key": key_of(&caption("canned")), "text": "{\"description\":\"c\",\"general_tags\":[],\"specific_tags\":[],\"language\":\"en\"}"});
    assert_eq!(admin(&stub, "POST", "/_stub/canned", canned).await, 204);
    assert_eq!(
        provider
            .chat(&caption("canned"), &no_retry)
            .await
            .unwrap()
            .json
            .unwrap()["description"],
        "c"
    );

    assert_eq!(
        admin(&stub, "PUT", "/_stub/latency", json!({"ms": 0})).await,
        204
    );
    assert_eq!(
        admin(&stub, "DELETE", "/_stub/requests", json!(null)).await,
        204
    );
    assert_eq!(
        admin(&stub, "POST", "/_stub/faults", json!({"nope": 1})).await,
        400
    );

    assert_eq!(
        admin(&stub, "POST", "/_stub/offline", json!({"seconds": 0.5})).await,
        202
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        provider
            .chat(&caption("x"), &no_retry)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Offline
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    provider.chat(&caption("x"), &no_retry).await.unwrap();
    assert!(
        stub.requests()
            .iter()
            .all(|request| !request.path.starts_with("/_stub"))
    );
}

#[tokio::test]
async fn a_stub_that_is_shut_down_refuses_connections() {
    let stub = stub().await;
    let base = stub.openai_base();
    let policy = policy(&stub);
    stub.shutdown().await;
    let provider = Provider::new(
        ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            Source::Operator,
            base.clone(),
        ),
        &policy,
        Arc::new(DirectTransport::new()),
    )
    .unwrap();
    let error = provider.chat(&caption("x"), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Offline);
    // The transport reports the refusal itself.
    use shelfy_ai::transport::{ConnectFailure, HttpRequest, Transport};
    let request = HttpRequest {
        method: http::Method::GET,
        url: base,
        headers: http::HeaderMap::new(),
        body: bytes::Bytes::new(),
        egress: shelfy_ai::Egress::Allowlisted,
        connect_timeout: Duration::from_secs(1),
        timeout: Duration::from_secs(5),
    };
    assert_eq!(
        DirectTransport::new().send(request).await.unwrap_err(),
        TransportError::Connect(ConnectFailure::Refused)
    );
}
