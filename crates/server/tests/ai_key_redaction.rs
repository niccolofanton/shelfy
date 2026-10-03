//! The operator key never reaches a log, a metric, an answer, a problem, an
//! event or a notification (P3-09, plan §7.1). Its own test binary, which
//! installs the global log subscriber so background work is captured too, as
//! `log_redaction.rs` does for the other secrets.

mod support;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelfy_ai::secrecy::SecretString;
use shelfy_ai::stub::{Fault, FaultRule, Stub, StubConfig};
use shelfy_ai::{ChatRequest, ErrorKind, Message};
use shelfy_server::ai::{AiServiceError, CallHints, Caller, OperatorConfig, Task};
use shelfy_server::config::Config;
use shelfy_server::error::ApiError;
use shelfy_server::events::Delivery;
use shelfy_server::outbound::OriginAllowlist;
use shelfy_server::telemetry::{json_layer, metrics};
use support::TestState;
use support::auth::owner;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

/// A distinctive operator key that must appear nowhere but the wire.
const PLANTED_KEY: &str = "ig_PlantedOperatorKey_4242_zzz";
const PLANTED_CAPTION: &str = "Planted catalog caption 7331 never log this lamp";

/// Collects everything the log layer writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn the_operator_key_never_leaks() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(json_layer(capture.clone()));
    tracing::subscriber::set_global_default(subscriber).expect("the only subscriber");
    let handle = metrics::install();

    let stub = Stub::start(StubConfig {
        api_key: Some(SecretString::from(PLANTED_KEY)),
        ..StubConfig::default()
    })
    .await
    .unwrap();

    let t = TestState::with_config(|config: &mut Config| {
        config.outbound.allow_origins =
            OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
        config.operator = OperatorConfig {
            url: Some(stub.openai_base()),
            key: Some(SecretString::from(PLANTED_KEY)),
            model: Some("stub-text".to_owned()),
            vision_model: Some("stub-vision".to_owned()),
            embed_model: None,
            label: "Node".to_owned(),
            concurrency: 1,
            timeout: Duration::from_secs(5),
            stt_url: Some(stub.whisper_url()),
            stt_key: Some(SecretString::from(PLANTED_KEY)),
        };
    });
    let owner_id = owner(&t);
    let svc = t.state.ai();
    let caller = Caller::new(&owner_id, true);
    let request = ChatRequest::new("stub-text", vec![Message::user_text("hello")]);

    // A successful call: its answer must not carry the key.
    let answer = svc
        .chat(&t.state, caller, Task::Chat, &request, CallHints::new())
        .await
        .unwrap();
    assert!(!format!("{answer:?}").contains(PLANTED_KEY));
    assert!(!answer.text.contains(PLANTED_KEY));

    // The real drain: its caption and rendered prompt are library content.
    t.write(&owner_id, |tx| {
        let mut post = shelfy_core::repo::posts::NewPost::new(
            "x_7331",
            shelfy_core::repo::Platform::Twitter,
            "7331",
            "text",
            1,
        );
        post.caption = Some(PLANTED_CAPTION.into());
        shelfy_core::repo::posts::insert(tx, &post, 1)
    })
    .await;
    shelfy_server::ai::queue::analyze(
        &t.state,
        &owner_id,
        shelfy_core::selector::Selector::Keys(vec!["x_7331".into()]),
        shelfy_core::ai::queue::Mode::Missing,
        None,
        false,
    )
    .await
    .unwrap();
    let job = shelfy_server::jobs::ai_drain::enqueue(t.state.jobs(), &owner_id)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    t.wait_job(&owner_id, job, |j| {
        j.state == shelfy_server::events::model::JobState::Succeeded
    })
    .await;
    scheduler.abort().await;

    let mut events = t.state.events().subscribe(&owner_id, None);

    // A refused key: its error, as the problem a route would send.
    stub.inject(FaultRule::new(Fault::Unauthorized).always());
    let err = svc
        .chat(&t.state, caller, Task::Chat, &request, CallHints::new())
        .await
        .unwrap_err();
    assert!(matches!(&err, AiServiceError::Call(e) if e.kind() == ErrorKind::InvalidKey));
    let problem = ApiError::from(err).problem();
    let problem_json = serde_json::to_string(&problem).unwrap();
    assert!(
        !problem_json.contains(PLANTED_KEY),
        "the problem leaked the key: {problem_json}"
    );

    // The events that reached the owner (provider.status, the notification).
    let mut seen = String::new();
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_millis(500), events.next()).await {
            Ok(Delivery::Event(event)) => seen.push_str(&event.data),
            _ => break,
        }
    }
    assert!(
        seen.contains("invalid_key"),
        "the status event was expected: {seen:?}"
    );
    assert!(
        !seen.contains(PLANTED_KEY),
        "an event leaked the key: {seen}"
    );

    // The metrics.
    handle.run_upkeep();
    let rendered = handle.render();
    assert!(!rendered.contains(PLANTED_KEY), "a metric leaked the key");

    // Let the usage write and any background logging settle, then the logs.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    assert!(!logs.is_empty(), "no logs were captured");
    assert!(!logs.contains(PLANTED_KEY), "the logs leaked the key");
    assert!(
        !logs.contains(PLANTED_CAPTION),
        "the logs leaked library content"
    );
    assert!(
        !logs.contains("You are an assistant that catalogs"),
        "the logs leaked the prompt"
    );
}
