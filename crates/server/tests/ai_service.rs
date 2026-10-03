//! The AI service and the operator provider (P3-09), with the test stub as
//! the operator's node. Nothing leaves the machine; CI needs no network.
//!
//! The acceptance bullets: routing defaults and the operator limit; the node
//! offline then back, holding work without spending tries; consent; the pause
//! and the status on an invalid key; usage rows; the metrics; and the authz of
//! the new routes. The operator key never reaching a log is its own binary,
//! `ai_key_redaction.rs`.

mod support;

use std::time::Duration;

use axum::http::StatusCode;
use shelfy_ai::secrecy::SecretString;
use shelfy_ai::stub::{Fault, FaultRule, Stub, StubConfig};
use shelfy_ai::{ChatRequest, EmbedRequest, ErrorKind, JsonOutput, Message, TranscribeRequest};
use shelfy_server::ai::{AI_DRAIN_KIND, AiServiceError, CallHints, Caller, OperatorConfig, Task};
use shelfy_server::config::Config;
use shelfy_server::control::usage_daily;
use shelfy_server::events::Delivery;
use shelfy_server::events::model::ProviderState;
use shelfy_server::ids::now_ms;
use shelfy_server::jobs::Registry;
use shelfy_server::outbound::OriginAllowlist;
use shelfy_server::telemetry::metrics;
use support::auth::{owner, sign_in, with_session};
use support::jobs::{Probe, kind};
use support::{TestState, get, json, problem, send};

const KEY: &str = "operator-key-for-tests";

async fn stub() -> Stub {
    Stub::start(StubConfig {
        api_key: Some(SecretString::from(KEY)),
        ..StubConfig::default()
    })
    .await
    .unwrap()
}

/// Points a config's operator provider at `stub`, allowlisting its origin.
fn configure(config: &mut Config, stub: &Stub, concurrency: u8, timeout: Duration) {
    config.outbound.allow_origins =
        OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
    config.operator = OperatorConfig {
        url: Some(stub.openai_base()),
        key: Some(SecretString::from(KEY)),
        model: Some("stub-text".to_owned()),
        vision_model: Some("stub-vision".to_owned()),
        embed_model: Some("stub-embed".to_owned()),
        label: "Test node".to_owned(),
        concurrency,
        timeout,
        stt_url: Some(stub.whisper_url()),
        stt_key: Some(SecretString::from(KEY)),
    };
}

fn catalog() -> JsonOutput {
    JsonOutput::new(
        "catalog",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "tags": { "type": "array", "items": { "type": "string" } } },
            "required": ["tags"]
        }),
    )
    .unwrap()
}

fn catalog_request(model: &str) -> ChatRequest {
    ChatRequest::new(model, vec![Message::user_text("a brass lamp")]).with_json(catalog())
}

/// A 16 kHz mono 16-bit WAV of silence.
fn wav() -> Vec<u8> {
    let data = vec![0_u8; 3200];
    let len = u32::try_from(data.len()).unwrap();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&16_000_u32.to_le_bytes());
    out.extend_from_slice(&32_000_u32.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&data);
    out
}

#[tokio::test]
async fn routing_defaults_to_the_operator_for_the_owner() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, 1, Duration::from_secs(30)));
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let owner = Caller::new(&owner_id, true);
    let member = Caller::new(&owner_id, false);

    let catalog = svc.route(&t.state, owner, Task::Catalog).await.unwrap();
    assert_eq!(
        catalog.model, "stub-vision",
        "cataloging uses the vision model"
    );
    let chat = svc.route(&t.state, owner, Task::Chat).await.unwrap();
    assert_eq!(chat.model, "stub-text", "text tasks use the text model");
    let embed = svc.route(&t.state, owner, Task::Embed).await.unwrap();
    assert_eq!(embed.model, "stub-embed");
    svc.route(&t.state, owner, Task::Stt).await.unwrap();

    // The operator provider is owner-only (E4).
    assert!(matches!(
        svc.route(&t.state, member, Task::Catalog).await,
        Err(AiServiceError::NotConfigured)
    ));
    assert!(svc.is_configured());
}

#[tokio::test]
async fn without_a_vision_model_cataloging_has_no_route() {
    let stub = stub().await;
    let t = TestState::with_config(|c| {
        configure(c, &stub, 1, Duration::from_secs(30));
        c.operator.vision_model = None;
    });
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let caller = Caller::new(&owner_id, true);
    assert!(matches!(
        svc.route(&t.state, caller, Task::Catalog).await,
        Err(AiServiceError::NotConfigured)
    ));
    // Text still routes.
    assert!(svc.route(&t.state, caller, Task::Chat).await.is_ok());
}

#[tokio::test]
async fn a_catalog_and_a_transcription_go_to_the_node() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, 1, Duration::from_secs(30)));
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let caller = Caller::new(&owner_id, true);

    let route = svc.route(&t.state, caller, Task::Catalog).await.unwrap();
    let answer = svc
        .chat(
            &t.state,
            caller,
            Task::Catalog,
            &catalog_request(&route.model),
            CallHints::new(),
        )
        .await
        .unwrap();
    assert!(answer.json.unwrap()["tags"].is_array());
    assert!(answer.usage.is_some(), "the node reports usage");

    let transcript = svc
        .transcribe(
            &t.state,
            caller,
            &TranscribeRequest {
                wav: wav().into(),
                language: Some("it".into()),
                model: None,
            },
            CallHints::new(),
        )
        .await
        .unwrap();
    assert!(transcript.text.starts_with("stub transcript"));

    // An embeddings call too.
    let vectors = svc
        .embed(
            &t.state,
            caller,
            &EmbedRequest {
                model: "stub-embed".into(),
                input: vec!["lamp".into()],
                dimensions: None,
            },
            CallHints::new(),
        )
        .await
        .unwrap();
    assert_eq!(vectors.vectors.len(), 1);

    // The node saw the vision model for cataloging, and whisper for the clip.
    let models: Vec<String> = stub
        .requests()
        .iter()
        .filter_map(|r| r.model().map(str::to_owned))
        .collect();
    assert!(models.iter().any(|m| m == "stub-vision"), "{models:?}");
}

#[tokio::test]
async fn the_operator_limit_paces_calls_one_at_a_time() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, 1, Duration::from_secs(30)));
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let caller = Caller::new(&owner_id, true);
    let request = ChatRequest::new("stub-text", vec![Message::user_text("hi")]);

    // Three concurrent operator calls, concurrency 1 and a 500 ms gap: their
    // starts are at least 500 ms apart, so the run takes about 1 s. A test of
    // "never more than one in flight" without timing is in the gate's own unit
    // test (`ai/breaker.rs` and `ai/service.rs`).
    let started = std::time::Instant::now();
    let (a, b, c) = tokio::join!(
        svc.chat(&t.state, caller, Task::Chat, &request, CallHints::new()),
        svc.chat(&t.state, caller, Task::Chat, &request, CallHints::new()),
        svc.chat(&t.state, caller, Task::Chat, &request, CallHints::new()),
    );
    let elapsed = started.elapsed();
    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    assert!(
        elapsed >= Duration::from_millis(900),
        "three paced calls took {elapsed:?}, not serialized"
    );
    assert_eq!(stub.requests().len(), 3, "all three reached the node");
}

#[tokio::test]
async fn an_offline_node_holds_work_and_recovers_on_a_probe() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, 1, Duration::from_secs(5)));
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let caller = Caller::new(&owner_id, true);
    let request = ChatRequest::new("stub-text", vec![Message::user_text("hi")]);

    stub.set_offline(true).await.unwrap();
    let err = svc
        .chat(&t.state, caller, Task::Chat, &request, CallHints::new())
        .await
        .unwrap_err();
    assert!(matches!(&err, AiServiceError::Call(e) if e.kind() == ErrorKind::Offline));
    assert_eq!(svc.operator_state(), Some(ProviderState::Offline));
    stub.clear_requests();

    // Held: a second call returns at once, without reaching the node.
    let held = std::time::Instant::now();
    let err = svc
        .chat(&t.state, caller, Task::Chat, &request, CallHints::new())
        .await
        .unwrap_err();
    assert!(matches!(&err, AiServiceError::Call(e) if e.kind() == ErrorKind::Offline));
    assert!(
        held.elapsed() < Duration::from_secs(1),
        "a held call waited on the node"
    );
    assert!(
        stub.requests().is_empty(),
        "a held call did not reach the node"
    );

    // A probe while still offline keeps it offline.
    svc.operator_probe_once(&t.state).await;
    assert_eq!(svc.operator_state(), Some(ProviderState::Offline));

    // Back online: a probe recovers it, and calls work again.
    stub.set_offline(false).await.unwrap();
    svc.operator_probe_once(&t.state).await;
    assert_eq!(svc.operator_state(), Some(ProviderState::Ok));
    assert!(
        svc.chat(&t.state, caller, Task::Chat, &request, CallHints::new())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn an_invalid_key_pauses_the_drain_notifies_and_recovers() {
    let stub = stub().await;
    let t = TestState::with_config(|c| {
        configure(c, &stub, 1, Duration::from_secs(5));
        // A stand-in for P3-13's ai.drain, so pause and resume have a queue.
        c.jobs.registry = Registry::new().register(kind(AI_DRAIN_KIND, 1, 1, &Probe::new()));
    });
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let caller = Caller::new(&owner_id, true);
    let request = ChatRequest::new("stub-text", vec![Message::user_text("hi")]);

    let mut events = t.state.events().subscribe(&owner_id, None);
    stub.inject(FaultRule::new(Fault::Unauthorized).always());
    let err = svc
        .chat(&t.state, caller, Task::Chat, &request, CallHints::new())
        .await
        .unwrap_err();
    assert!(matches!(&err, AiServiceError::Call(e) if e.kind() == ErrorKind::InvalidKey));
    assert_eq!(svc.operator_state(), Some(ProviderState::InvalidKey));
    assert!(
        t.state.jobs().is_paused(&owner_id, AI_DRAIN_KIND),
        "the drain is paused"
    );

    // The notification and the status reached the owner's stream.
    let mut saw_notification = false;
    let mut saw_status = false;
    for _ in 0..6 {
        match tokio::time::timeout(Duration::from_secs(2), events.next()).await {
            Ok(Delivery::Event(event)) => {
                if event.data.contains("ai.provider_key_invalid") {
                    saw_notification = true;
                }
                if event.data.contains("invalid_key") {
                    saw_status = true;
                }
            }
            _ => break,
        }
        if saw_notification && saw_status {
            break;
        }
    }
    assert!(saw_notification, "no ai.provider_key_invalid notification");
    assert!(saw_status, "no provider.status invalid_key event");

    // A new key (here: the fault cleared) and a probe resume the drain.
    stub.clear_faults();
    svc.operator_probe_once(&t.state).await;
    assert_eq!(svc.operator_state(), Some(ProviderState::Ok));
    assert!(
        !t.state.jobs().is_paused(&owner_id, AI_DRAIN_KIND),
        "the drain resumed"
    );
}

#[tokio::test]
async fn consent_is_recorded_once_and_usage_accumulates() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, 1, Duration::from_secs(30)));
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let caller = Caller::new(&owner_id, true);
    let request = ChatRequest::new("stub-text", vec![Message::user_text("hi")]);

    svc.chat(&t.state, caller, Task::Chat, &request, CallHints::new())
        .await
        .unwrap();
    svc.chat(&t.state, caller, Task::Chat, &request, CallHints::new())
        .await
        .unwrap();

    let conn = t.control();
    // Exactly one consent row for the operator, at first use.
    let consent_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'ai.provider.consent' \
             AND actor_user_id = ?1 AND target = 'operator'",
            [&owner_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(consent_rows, 1, "consent is recorded once");

    // Two calls and their tokens are in usage_daily.
    let daily = usage_daily::of_day(&conn, &owner_id, now_ms()).unwrap();
    assert_eq!(daily.ai_calls, 2);
    assert!(
        daily.ai_in_tokens > 0 || daily.ai_out_tokens > 0,
        "tokens were recorded"
    );
}

#[tokio::test]
async fn the_metric_counts_operator_calls_by_task_and_tokens() {
    let handle = metrics::install();
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, 1, Duration::from_secs(30)));
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let caller = Caller::new(&owner_id, true);

    let route = svc.route(&t.state, caller, Task::Catalog).await.unwrap();
    svc.chat(
        &t.state,
        caller,
        Task::Catalog,
        &catalog_request(&route.model),
        CallHints::new(),
    )
    .await
    .unwrap();

    handle.run_upkeep();
    let text = handle.render();
    assert!(
        text.contains(
            "shelfy_ai_requests_total{provider_kind=\"operator\",task=\"catalog\",outcome=\"ok\"}"
        ),
        "no operator catalog ok series in\n{text}"
    );
    assert!(
        text.contains("shelfy_ai_tokens_total{direction=\"input\"}"),
        "no input token series in\n{text}"
    );
}

#[tokio::test]
async fn the_new_routes_need_a_session_and_list_the_operator() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, 1, Duration::from_secs(30)));
    let app = t.app();
    let _owner_id = owner(&t);

    // Deny by default: no session.
    problem(
        send(&app, get("/api/v1/me/providers")).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    problem(
        send(&app, get("/api/v1/me/usage/ai")).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;

    // Signed in: the owner sees the operator provider, with no key.
    let cookie = sign_in(&app, &t).await;
    let response = send(&app, with_session(get("/api/v1/me/providers"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let providers = json(response).await;
    let list = providers.as_array().expect("an array");
    let operator = list
        .iter()
        .find(|p| p["id"] == "operator")
        .expect("the operator provider is listed");
    assert_eq!(operator["managed"], true);
    assert_eq!(operator["kind"], "operator");
    assert!(
        operator.get("key").is_none() && operator.get("last4").is_none(),
        "no key shown"
    );

    let usage = send(&app, with_session(get("/api/v1/me/usage/ai"), &cookie)).await;
    assert_eq!(usage.status(), StatusCode::OK);
    assert!(json(usage).await["days"].is_array());
}
