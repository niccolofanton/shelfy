//! The AI adapters (`shelfy-ai`, P3-01) through the outbound client (P2-04,
//! L11): the operator's node reached at its allowlisted origin, a user's
//! provider held to the strict rules, faults mapped to the adapters' error
//! kinds, and the metric. The test provider (`shelfy_ai::stub`) stands for
//! every provider; nothing leaves the machine.
//!
//! This file never spells the HTTP client crate's name, so that the
//! single-construction rule can scan it like any other.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelfy_ai::secrecy::SecretString;
use shelfy_ai::stub::{AuthSeen, Endpoint, Fault, FaultRule, Stub, StubConfig};
use shelfy_ai::{
    CallOptions, ChatRequest, EgressPolicy, ErrorKind, JsonOutput, Message, Provider,
    ProviderConfig, ProviderKind, RetryPolicy, Source, Timeouts, TranscribeRequest,
};
use shelfy_server::outbound::ai::AiTransport;
use shelfy_server::outbound::{Lookup, OriginAllowlist, Outbound, OutboundConfig};
use shelfy_server::telemetry::metrics;
use url::Url;

const KEY: &str = "operator-key-for-tests";

async fn stub() -> Stub {
    Stub::start(StubConfig {
        api_key: Some(SecretString::from(KEY)),
        ..StubConfig::default()
    })
    .await
    .unwrap()
}

/// The outbound client with the stub as the operator's node, and a name that
/// answers a private address.
fn outbound(stub: &Stub) -> Outbound {
    let origin = format!("http://{}", stub.addr());
    let private: IpAddr = "127.0.0.1".parse().unwrap();
    Outbound::new(&OutboundConfig {
        allow_origins: OriginAllowlist::parse(&origin).unwrap(),
        lookup: Lookup::fixed([("rebind.example.test", vec![private])]),
        ..OutboundConfig::default()
    })
    .unwrap()
}

/// `shelfy_ai`'s own allowlist: the same origin.
fn policy(stub: &Stub) -> EgressPolicy {
    EgressPolicy::new().allow(shelfy_ai::Origin::of(&stub.url()).unwrap())
}

fn node(stub: &Stub, transport: Arc<AiTransport>, kind: ProviderKind, base: Url) -> Provider {
    let config = ProviderConfig::new(kind, Source::Operator, base)
        .with_key(SecretString::from(KEY))
        .with_llama_health();
    Provider::new(config, &policy(stub), transport).unwrap()
}

fn options() -> CallOptions {
    CallOptions::new(Timeouts::new(
        Duration::from_secs(2),
        Duration::from_secs(10),
    ))
    .with_retry(RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        retry_after_cap: Duration::from_secs(2),
    })
}

fn catalog() -> JsonOutput {
    JsonOutput::new(
        "catalog",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {"tags": {"type": "array", "items": {"type": "string"}}},
            "required": ["tags"]
        }),
    )
    .unwrap()
}

/// A 16 kHz mono 16-bit WAV of silence.
fn wav() -> Vec<u8> {
    let data = vec![0_u8; 3200];
    let data_len = u32::try_from(data.len()).unwrap();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&16_000_u32.to_le_bytes());
    out.extend_from_slice(&32_000_u32.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&data);
    out
}

#[tokio::test]
async fn the_operators_node_is_reached_at_its_allowlisted_origin() {
    let handle = metrics::install();
    let stub = stub().await;
    let transport = Arc::new(AiTransport::new(&outbound(&stub)));
    let chat = node(
        &stub,
        transport.clone(),
        ProviderKind::OpenAiCompatible,
        stub.openai_base(),
    );

    chat.health(&options()).await.unwrap();
    let models = chat.models(&options()).await.unwrap();
    assert_eq!(models.len(), 3);

    let request =
        ChatRequest::new("ornith", vec![Message::user_text("a lamp")]).with_json(catalog());
    let answer = chat.chat(&request, &options()).await.unwrap();
    assert!(answer.json.unwrap()["tags"].is_array());

    let seen = Arc::new(Mutex::new(0_usize));
    let counter = Arc::clone(&seen);
    let streamed = options().with_text_callback(move |_| *counter.lock().unwrap() += 1);
    let answer = chat
        .chat(&request.clone().streamed(true), &streamed)
        .await
        .unwrap();
    assert!(answer.json.is_some());
    assert!(*seen.lock().unwrap() > 2, "the stream arrived in pieces");
    assert!(!answer.stream_fallback);
    assert!(answer.usage.is_some());

    let whisper = node(
        &stub,
        transport,
        ProviderKind::WhisperCpp,
        stub.whisper_url(),
    );
    let transcript = whisper
        .transcribe(
            &TranscribeRequest {
                wav: wav().into(),
                language: Some("it".into()),
                model: None,
            },
            &options(),
        )
        .await
        .unwrap();
    assert!(transcript.text.starts_with("stub transcript"));

    let requests = stub.requests();
    let endpoints: Vec<Endpoint> = requests.iter().map(|request| request.endpoint).collect();
    assert_eq!(
        endpoints,
        [
            Endpoint::Health,
            Endpoint::Models,
            Endpoint::Chat,
            Endpoint::Chat,
            Endpoint::Inference
        ]
    );
    assert!(
        requests
            .iter()
            .all(|request| request.auth == AuthSeen::Valid),
        "the key reached the node"
    );

    handle.run_upkeep();
    let text = handle.render();
    let series = "shelfy_egress_requests_total{purpose=\"ai_operator\",outcome=\"ok\"}";
    let count: f64 = text
        .lines()
        .find_map(|line| line.strip_prefix(series))
        .and_then(|rest| rest.trim().parse().ok())
        .unwrap_or_else(|| panic!("no {series} in\n{text}"));
    assert!(count >= 5.0, "{count}");
}

#[tokio::test]
async fn faults_reach_the_adapters_as_their_error_kinds() {
    let stub = stub().await;
    let transport = Arc::new(AiTransport::new(&outbound(&stub)));
    let chat = node(
        &stub,
        transport,
        ProviderKind::OpenAiCompatible,
        stub.openai_base(),
    );
    let hello = || ChatRequest::new("m", vec![Message::user_text("hello")]);

    // A node that is asleep: connection refused.
    stub.set_offline(true).await.unwrap();
    let error = chat.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Offline, "{error}");
    stub.set_offline(false).await.unwrap();

    // A redirect is never followed.
    stub.inject(FaultRule::new(Fault::Redirect));
    let error = chat.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(
        (error.kind(), error.status()),
        (ErrorKind::BadRequest, Some(302))
    );

    // A rate limit is waited out; a broken stream falls back.
    stub.inject(FaultRule::new(Fault::RateLimited { retry_after_ms: 50 }));
    assert_eq!(chat.chat(&hello(), &options()).await.unwrap().requests, 2);
    stub.inject(FaultRule::new(Fault::Reset));
    let answer = chat
        .chat(&hello().streamed(true), &options())
        .await
        .unwrap();
    assert!(answer.stream_fallback);

    // The adapters' own deadline ends a node that never answers.
    stub.inject(FaultRule::new(Fault::Timeout));
    let short = CallOptions::new(Timeouts::new(
        Duration::from_secs(1),
        Duration::from_millis(300),
    ))
    .with_retry(RetryPolicy::NONE);
    let error = chat.chat(&hello(), &short).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Transient);
    assert!(error.message().contains("longer than 300 ms"), "{error}");

    let paths: Vec<String> = stub
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert!(
        paths.iter().all(|path| path == "/v1/chat/completions"),
        "the redirect's target was never requested: {paths:?}"
    );
}

#[tokio::test]
async fn a_users_provider_is_held_to_the_strict_rules() {
    let stub = stub().await;
    let transport = Arc::new(AiTransport::new(&outbound(&stub)));
    let user = |base: &str, policy: &EgressPolicy| {
        let config = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            Source::User,
            Url::parse(base).unwrap(),
        )
        .with_key(SecretString::from("user-key"));
        Provider::new(config, policy, transport.clone())
    };
    let hello = ChatRequest::new("m", vec![Message::user_text("hello")]);

    // A name that answers a private address: refused by the outbound client.
    let rebind = user("https://rebind.example.test/v1", &policy(&stub)).unwrap();
    let error = rebind.chat(&hello, &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Refused, "{error}");

    // The operator's origin is not a user's.
    let operator = format!("{}v1", stub.url());
    let error = user(&operator, &policy(&stub)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Refused);

    // The loopback switch passes the guard, but the outbound client never
    // reaches a loopback address.
    let loopback = EgressPolicy::new().allow_loopback(true);
    let local = user(stub.openai_base().as_str(), &loopback).unwrap();
    let error = local.chat(&hello, &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Refused, "{error}");

    assert!(stub.requests().is_empty(), "nothing reached the stub");
}
