//! Errors and retries against the stub: each kind, the retries on transient
//! and rate-limited errors with `Retry-After`, none on the others, timeouts,
//! cancellation, redirects.

mod support;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use shelfy_ai::stub::{Endpoint, Fault, FaultRule};
use shelfy_ai::transport::{ConnectFailure, HttpRequest, HttpResponse, Transport, TransportError};
use shelfy_ai::{
    CallOptions, ChatRequest, EgressPolicy, ErrorKind, Message, Origin, Provider, ProviderConfig,
    ProviderKind, RetryPolicy, Source, Timeouts,
};
use support::*;
use tokio_util::sync::CancellationToken;

fn hello() -> ChatRequest {
    ChatRequest::new("m", vec![Message::user_text("hello")])
}

#[tokio::test]
async fn offline_is_connection_refused_and_never_retried() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.set_offline(true).await;
    let error = provider.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Offline, "{error}");
    assert!(error.message().contains("refused"), "{error}");
    stub.set_offline(false).await;
    provider.chat(&hello(), &options()).await.unwrap();
    assert_eq!(
        stub.requests().len(),
        1,
        "only the call after coming back arrived"
    );
    let health = support::provider(
        &stub,
        ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            Source::Operator,
            stub.openai_base(),
        )
        .with_llama_health(),
    );
    stub.set_offline(true).await;
    assert_eq!(
        health.health(&options()).await.unwrap_err().kind(),
        ErrorKind::Offline
    );
    stub.set_offline(false).await;
    health.health(&options()).await.unwrap();
}

/// Fails every request with one connect failure, counting them.
struct Unreachable {
    failure: ConnectFailure,
    calls: AtomicU32,
    connect_timeouts: Mutex<Vec<Duration>>,
}

impl Transport for Unreachable {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.connect_timeouts
            .lock()
            .unwrap()
            .push(request.connect_timeout);
        let failure = self.failure.clone();
        Box::pin(async move { Err(TransportError::Connect(failure)) })
    }
}

#[tokio::test]
async fn every_connect_failure_is_offline_and_never_retried() {
    // No network: a transport stands for the refused, unroutable, unknown or
    // silent host. The stub's offline spell covers a real refusal.
    let base = url::Url::parse("http://100.94.10.20:8080/v1").unwrap();
    let policy = EgressPolicy::new().allow(Origin::of(&base).unwrap());
    for failure in [
        ConnectFailure::Refused,
        ConnectFailure::Unreachable,
        ConnectFailure::Dns,
        ConnectFailure::Timeout,
    ] {
        let transport = Arc::new(Unreachable {
            failure: failure.clone(),
            calls: AtomicU32::new(0),
            connect_timeouts: Mutex::new(Vec::new()),
        });
        let provider = Provider::new(
            ProviderConfig::new(
                ProviderKind::OpenAiCompatible,
                Source::Operator,
                base.clone(),
            ),
            &policy,
            transport.clone(),
        )
        .unwrap();
        let options = CallOptions::new(Timeouts::new(
            Duration::from_millis(3000),
            Duration::from_secs(60),
        ))
        .with_retry(fast_retry());
        let error = provider.chat(&hello(), &options).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Offline, "{failure:?}: {error}");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1, "{failure:?}");
        assert_eq!(
            *transport.connect_timeouts.lock().unwrap(),
            [Duration::from_millis(3000)],
            "the caller's connect timeout reaches the transport"
        );
    }
}

#[tokio::test]
async fn statuses_map_to_kinds_and_only_transient_ones_are_retried() {
    let cases = [
        (Fault::Unauthorized, ErrorKind::InvalidKey, 401, 1),
        (Fault::Forbidden, ErrorKind::InvalidKey, 403, 1),
        (Fault::QuotaExhausted, ErrorKind::QuotaExhausted, 429, 1),
        (Fault::BadRequest, ErrorKind::BadRequest, 400, 1),
        (Fault::ServerError, ErrorKind::Transient, 500, 4),
        (Fault::Overloaded, ErrorKind::Transient, 503, 4),
    ];
    for (fault, kind, status, requests) in cases {
        let stub = stub().await;
        let provider = operator(&stub, ProviderKind::OpenAiCompatible);
        stub.inject(FaultRule::new(fault.clone()).always());
        let error = provider.chat(&hello(), &options()).await.unwrap_err();
        assert_eq!(error.kind(), kind, "{fault:?}: {error}");
        assert_eq!(error.status(), Some(status), "{fault:?}");
        assert_eq!(stub.requests().len(), requests, "{fault:?}");
    }
}

#[tokio::test]
async fn anthropic_overload_and_billing_map_too() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::Anthropic);
    stub.inject(FaultRule::new(Fault::Overloaded));
    let answer = provider.chat(&hello(), &options()).await.unwrap();
    assert_eq!(answer.requests, 2, "a 529 is retried");
    stub.inject(FaultRule::new(Fault::QuotaExhausted));
    let error = provider.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::QuotaExhausted);
    assert_eq!(
        (error.status(), error.code()),
        (Some(402), Some("billing_error"))
    );
}

#[tokio::test]
async fn a_transient_error_is_retried_until_it_passes() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::ServerError).times(2));
    let answer = provider.chat(&hello(), &options()).await.unwrap();
    assert_eq!(answer.requests, 3);
}

#[tokio::test]
async fn rate_limits_wait_the_retry_after() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::RateLimited {
        retry_after_ms: 300,
    }));
    let started = Instant::now();
    let answer = provider.chat(&hello(), &options()).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(answer.requests, 2);
}

#[tokio::test]
async fn a_retry_after_past_the_cap_goes_back_to_the_caller() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::RateLimited {
        retry_after_ms: 60_000,
    }));
    let started = Instant::now();
    let error = provider.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::RateLimited);
    assert_eq!(error.retry_after(), Some(Duration::from_secs(60)));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(stub.requests().len(), 1);
}

#[tokio::test]
async fn rate_limits_run_out_of_retries() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::Anthropic);
    stub.inject(FaultRule::new(Fault::RateLimited { retry_after_ms: 10 }).always());
    let error = provider.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::RateLimited);
    assert_eq!(error.retry_after(), Some(Duration::from_millis(10)));
    assert_eq!(stub.requests().len(), 4, "the first try and 3 retries");
}

#[tokio::test]
async fn a_timeout_is_transient() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::Timeout));
    let options = CallOptions::new(Timeouts::new(
        Duration::from_secs(1),
        Duration::from_millis(300),
    ))
    .with_retry(RetryPolicy::NONE);
    let started = Instant::now();
    let error = provider.chat(&hello(), &options).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Transient);
    assert!(error.message().contains("longer than 300 ms"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_malformed_answer_is_transient_and_retried() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::MalformedJson).on(Endpoint::Chat));
    let answer = provider.chat(&hello(), &options()).await.unwrap();
    assert_eq!(answer.requests, 2);
    stub.inject(FaultRule::new(Fault::Reset).on(Endpoint::Chat));
    assert_eq!(
        provider.chat(&hello(), &options()).await.unwrap().requests,
        2
    );
    stub.inject(FaultRule::new(Fault::MalformedJson).on(Endpoint::Models));
    assert_eq!(provider.models(&options()).await.unwrap().len(), 3);
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::Redirect));
    let error = provider.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::BadRequest);
    assert_eq!(error.status(), Some(302));
    let requests = stub.requests();
    assert_eq!(requests.len(), 1, "the Location was never requested");
    assert_eq!(requests[0].path, "/v1/chat/completions");
}

#[tokio::test]
async fn cancelling_ends_the_call() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::Timeout));
    let token = CancellationToken::new();
    let options = options().with_cancel(token.clone());
    let call = tokio::spawn({
        let provider = provider.clone();
        async move { provider.chat(&hello(), &options).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    token.cancel();
    let error = call.await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled);
}
