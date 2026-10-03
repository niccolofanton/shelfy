//! A planted key appears in no `Debug` or `Display` of requests, endpoints,
//! providers or errors, and in no line of the stub's request log (plan §7.1,
//! P3 lane rule 6).

mod support;

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use http::{HeaderMap, StatusCode};
use secrecy::SecretString;
use shelfy_ai::stub::StubConfig;
use shelfy_ai::transport::{HttpRequest, HttpResponse, Transport, TransportError};
use shelfy_ai::{
    ChatRequest, EgressPolicy, ErrorKind, Message, Origin, Provider, ProviderConfig, ProviderKind,
    Source, StructuredMode,
};
use support::*;
use url::Url;

const PLANTED: &str = "sk-planted-SECRET-6f1e2d3c4b5a99";

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
