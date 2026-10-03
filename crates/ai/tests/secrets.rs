//! A planted key appears in no `Debug` or `Display` of requests, endpoints,
//! providers or errors, in no log line, and in no line of the stub's request
//! log (plan §7.1, P3 lane rule 6).

mod support;

use std::io;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use http::{HeaderMap, StatusCode};
use secrecy::SecretString;
use shelfy_ai::stub::{Fault, FaultRule, StubConfig};
use shelfy_ai::transport::{HttpRequest, HttpResponse, Transport, TransportError};
use shelfy_ai::{
    ChatRequest, EgressPolicy, ErrorKind, Message, Origin, Provider, ProviderConfig, ProviderKind,
    Source, StructuredMode,
};
use support::*;
use url::Url;

const PLANTED: &str = "sk-planted-SECRET-6f1e2d3c4b5a99";

/// Collects every log line.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn the_logs_never_hold_the_key() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let stub = stub_with(StubConfig {
        api_key: Some(SecretString::from(PLANTED)),
        ..StubConfig::default()
    })
    .await;
    let provider = support::provider(
        &stub,
        ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            Source::Operator,
            stub.openai_base(),
        )
        .with_key(SecretString::from(PLANTED)),
    );
    // A retry, a broken stream with its fallback, and a repair: every path
    // that logs.
    stub.inject(FaultRule::new(Fault::ServerError));
    stub.inject(FaultRule::new(Fault::MalformedJson));
    stub.inject(FaultRule::new(Fault::NonConformingJson));
    let request = ChatRequest::new("m", vec![Message::user_text("x")])
        .with_json(catalog())
        .streamed(true);
    let answer = provider.chat(&request, &options()).await.unwrap();
    assert!(answer.stream_fallback && answer.repaired);

    // A 401 that echoes the key.
    let echoed = format!(r#"{{"error":{{"message":"bad key {PLANTED}"}}}}"#);
    let echo = provider_on(
        Echo::new(401, "application/json", &echoed),
        config(
            ProviderKind::OpenAiCompatible,
            "http://100.94.10.20:8080/v1",
        ),
    );
    let error = echo
        .chat(
            &ChatRequest::new("m", vec![Message::user_text("x")]),
            &options(),
        )
        .await
        .unwrap_err();
    tracing::info!(error = %error, debug = ?error, "the caller logs the error");

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("retrying an AI call"), "{logs}");
    assert!(logs.contains("retrying without streaming"), "{logs}");
    assert!(logs.contains("repairing a JSON answer"), "{logs}");
    assert_clean(&logs);
}

/// Records each request's `Debug` and answers with a fixed status and body.
struct Echo {
    status: StatusCode,
    content_type: &'static str,
    body: String,
    seen: Mutex<Vec<String>>,
}

impl Echo {
    fn new(status: u16, content_type: &'static str, body: &str) -> Arc<Self> {
        Arc::new(Self {
            status: StatusCode::from_u16(status).unwrap(),
            content_type,
            body: body.to_owned(),
            seen: Mutex::new(Vec::new()),
        })
    }
}

impl Transport for Echo {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        self.seen.lock().unwrap().push(format!("{request:?}"));
        let key_header = request
            .headers
            .get("authorization")
            .or_else(|| request.headers.get("x-api-key"))
            .expect("the key is sent");
        assert!(
            key_header.is_sensitive(),
            "the key header is marked sensitive"
        );
        let mut headers = HeaderMap::new();
        headers.insert("content-type", self.content_type.parse().unwrap());
        let body = Bytes::from(self.body.clone());
        let status = self.status;
        Box::pin(async move {
            Ok(HttpResponse {
                status,
                headers,
                body: Box::pin(futures_util::stream::iter([Ok(body)])),
            })
        })
    }
}

fn config(kind: ProviderKind, base: &str) -> ProviderConfig {
    ProviderConfig::new(kind, Source::Operator, Url::parse(base).unwrap())
        .with_key(SecretString::from(PLANTED))
        .with_llama_health()
}

fn provider_on(transport: Arc<dyn Transport>, config: ProviderConfig) -> Provider {
    let policy = EgressPolicy::new().allow(Origin::of(&config.base_url).unwrap());
    Provider::new(config, &policy, transport).unwrap()
}

fn assert_clean(text: &str) {
    assert!(!text.contains(PLANTED), "the key leaked: {text}");
    assert!(
        !text.contains("6f1e2d3c4b5a99"),
        "part of the key leaked: {text}"
    );
}

#[tokio::test]
async fn configs_providers_and_requests_never_print_the_key() {
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        let config = config(kind, "http://100.94.10.20:8080/v1");
        assert_clean(&format!("{config:?}"));
        let answer = match kind {
            ProviderKind::Anthropic => {
                r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#
            }
            _ => r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#,
        };
        let echo = Echo::new(200, "application/json", answer);
        let provider = provider_on(echo.clone(), config);
        assert_clean(&format!("{provider:?}"));
        let answer = provider
            .chat(
                &ChatRequest::new("m", vec![Message::user_text("x")]),
                &options(),
            )
            .await
            .unwrap();
        assert_eq!(answer.text, "ok");
        let seen = echo.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains("Sensitive"), "{}", seen[0]);
        assert_clean(&seen[0]);
    }
}

#[tokio::test]
async fn errors_that_echo_the_key_never_print_it() {
    let echoed = format!(
        r#"{{"error":{{"message":"Incorrect API key provided: {PLANTED}.","type":"invalid_request_error","code":"invalid_api_key"}}}}"#
    );
    let provider = provider_on(
        Echo::new(401, "application/json", &echoed),
        config(
            ProviderKind::OpenAiCompatible,
            "http://100.94.10.20:8080/v1",
        ),
    );
    let error = provider
        .chat(
            &ChatRequest::new("m", vec![Message::user_text("x")]),
            &options(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidKey);
    assert_clean(&error.to_string());
    assert_clean(&format!("{error:?}"));
    assert!(error.message().contains("[redacted]"));

    // An error event inside a stream, and an error in a 200 body, too.
    let stream = format!(
        "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"authentication_error\",\"message\":\"bad key {PLANTED}\"}}}}\n\n"
    );
    let provider = provider_on(
        Echo::new(200, "text/event-stream", &stream),
        config(ProviderKind::Anthropic, "http://100.94.10.20:8080"),
    );
    let request = ChatRequest::new("m", vec![Message::user_text("x")]).streamed(true);
    let error = provider.chat(&request, &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidKey);
    assert_clean(&format!("{error} {error:?}"));

    let body = format!(r#"{{"error":{{"code":403,"message":"key {PLANTED} is disabled"}}}}"#);
    let provider = provider_on(
        Echo::new(200, "application/json", &body),
        config(
            ProviderKind::OpenAiCompatible,
            "http://100.94.10.20:8080/v1",
        ),
    );
    let error = provider
        .chat(
            &ChatRequest::new("m", vec![Message::user_text("x")]),
            &options(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidKey);
    assert_clean(&format!("{error} {error:?}"));

    let provider = provider_on(
        Echo::new(503, "application/json", &body),
        config(
            ProviderKind::OpenAiCompatible,
            "http://100.94.10.20:8080/v1",
        ),
    );
    let error = provider
        .health(&options().with_retry(shelfy_ai::RetryPolicy::NONE))
        .await
        .unwrap_err();
    assert_clean(&format!("{error} {error:?}"));
}

#[tokio::test]
async fn the_stub_log_never_holds_the_key() {
    let stub = stub_with(StubConfig {
        api_key: Some(SecretString::from(PLANTED)),
        ..StubConfig::default()
    })
    .await;
    let stub_config = format!(
        "{:?}",
        StubConfig {
            api_key: Some(SecretString::from(PLANTED)),
            ..StubConfig::default()
        }
    );
    assert_clean(&stub_config);
    for kind in [
        ProviderKind::OpenAiCompatible,
        ProviderKind::Anthropic,
        ProviderKind::WhisperCpp,
    ] {
        let provider = support::provider(
            &stub,
            ProviderConfig::new(kind, Source::Operator, base(&stub, kind))
                .with_key(SecretString::from(PLANTED))
                .with_structured(StructuredMode::JsonSchema),
        );
        if kind == ProviderKind::WhisperCpp {
            let request = shelfy_ai::TranscribeRequest {
                wav: wav(160).into(),
                language: None,
                model: None,
            };
            provider.transcribe(&request, &options()).await.unwrap();
        } else {
            let request = ChatRequest::new("m", vec![Message::user_text("x")])
                .with_json(catalog())
                .streamed(true);
            provider.chat(&request, &options()).await.unwrap();
            provider.models(&options()).await.unwrap();
        }
    }
    let requests = stub.requests();
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .all(|request| request.auth == shelfy_ai::stub::AuthSeen::Valid)
    );
    assert_clean(&serde_json::to_string(&requests).unwrap());
    assert_clean(&format!("{requests:?}"));
}
