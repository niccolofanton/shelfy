//! Full redaction surfaces for synthetic BYOK credentials.
mod support;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use shelfy_ai::{
    ChatRequest, Message,
    secrecy::SecretString,
    stub::{Fault, FaultRule, Stub, StubConfig},
};
use shelfy_server::{
    ai::{
        CallHints, Caller, Task,
        providers::{self, CONSENT_VERSION},
    },
    error::ApiError,
    events::Delivery,
    telemetry::{json_layer, metrics},
};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use support::auth::{owner, sign_in, with_session};
use support::{TestState, body, get, send};
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt as _};
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
#[tokio::test]
async fn byok_key_never_leaves_redacted_surfaces() {
    const KEY: &str = "Planted_BYOK_secret_8442_never_log";
    let capture = Capture::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(json_layer(capture.clone())),
    )
    .unwrap();
    let metrics = metrics::install();
    let stub = Stub::start(StubConfig {
        api_key: Some(SecretString::from(KEY)),
        ..Default::default()
    })
    .await
    .unwrap();
    let t = TestState::with_config(|c| {
        c.ai_allow_loopback = true;
        c.vault = shelfy_server::ai::vault::KeyVault::new(
            Some(SecretString::from(STANDARD.encode([35; 32]))),
            None,
        )
        .unwrap();
    });
    let id = owner(&t);
    let session = sign_in(&t.app(), &t).await;
    let input=serde_json::from_value(serde_json::json!({"kind":"openai_compatible","label":"Synthetic","baseUrl":stub.openai_base(),"models":{"chat":"stub-text"},"key":KEY})).unwrap();
    providers::put(&t.state, &id, "custom", input)
        .await
        .unwrap();
    providers::consent(&t.state, &id, "custom", CONSENT_VERSION)
        .await
        .unwrap();
    let request = ChatRequest::new(
        "stub-text",
        vec![Message::user_text("synthetic connectivity text")],
    );
    let response = t
        .state
        .ai()
        .chat(
            &t.state,
            Caller::new(&id, false),
            Task::Chat,
            &request,
            CallHints::new(),
        )
        .await
        .unwrap();
    assert!(!format!("{response:?}").contains(KEY));
    let mut events = t.state.events().subscribe(&id, None);
    stub.inject(FaultRule::new(Fault::Unauthorized).always());
    let error = t
        .state
        .ai()
        .chat(
            &t.state,
            Caller::new(&id, false),
            Task::Chat,
            &request,
            CallHints::new(),
        )
        .await
        .unwrap_err();
    assert!(
        !serde_json::to_string(&ApiError::from(error).problem())
            .unwrap()
            .contains(KEY)
    );
    let probes = providers::test(&t.state, &id, "custom").await.unwrap();
    assert!(!serde_json::to_string(&probes).unwrap().contains(KEY));
    let summaries = body(
        send(
            &t.app(),
            with_session(get("/api/v1/me/providers"), &session),
        )
        .await,
    )
    .await;
    assert!(!String::from_utf8_lossy(&summaries).contains(KEY));
    let mut seen = String::new();
    while let Ok(Delivery::Event(event)) =
        tokio::time::timeout(Duration::from_millis(50), events.next()).await
    {
        seen.push_str(&event.data);
    }
    assert!(seen.contains("invalid_key"));
    assert!(!seen.contains(KEY));
    metrics.run_upkeep();
    assert!(!metrics.render().contains(KEY));
    let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    assert!(!logs.is_empty());
    assert!(!logs.contains(KEY));
}
