//! Streaming against the stub: an empty, malformed or broken stream gets one
//! non-streaming retry (desktop AI-13); a slow first token ends the call.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelfy_ai::stub::{Fault, FaultRule};
use shelfy_ai::{CallOptions, ChatRequest, ErrorKind, Message, ProviderKind, Timeouts};
use support::*;

fn streamed(kind: ProviderKind) -> ChatRequest {
    let request = ChatRequest::new("m", vec![Message::user_text("a lamp")]).streamed(true);
    match kind {
        ProviderKind::Anthropic => request,
        _ => request.with_json(catalog()),
    }
}

#[tokio::test]
async fn an_empty_malformed_or_broken_stream_gets_one_non_streaming_retry() {
    for fault in [Fault::EmptyStream, Fault::MalformedJson, Fault::Reset] {
        for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
            let stub = stub().await;
            let provider = operator(&stub, kind);
            stub.inject(FaultRule::new(fault.clone()));
            let seen = Arc::new(Mutex::new(Vec::<String>::new()));
            let sink = Arc::clone(&seen);
            let options = options()
                .with_text_callback(move |text| sink.lock().unwrap().push(text.to_owned()));
            let answer = provider
                .chat(&streamed(kind), &options)
                .await
                .unwrap_or_else(|error| panic!("{fault:?} {kind:?}: {error}"));
            assert!(answer.stream_fallback, "{fault:?} {kind:?}");
            assert_eq!(answer.requests, 2, "{fault:?} {kind:?}");
            let requests = stub.requests();
            assert!(requests[0].streamed());
            assert!(!requests[1].streamed(), "the retry does not stream");
            assert_eq!(
                seen.lock().unwrap().last(),
                Some(&answer.text),
                "the callback ends on the final text"
            );
        }
    }
}

#[tokio::test]
async fn a_slow_first_token_past_its_deadline_ends_the_call() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::SlowFirstToken { delay_ms: 1500 }));
    let options = CallOptions::new(
        Timeouts::new(Duration::from_secs(2), Duration::from_secs(10))
            .with_first_token(Duration::from_millis(200)),
    )
    .with_retry(fast_retry());
    let started = std::time::Instant::now();
    let error = provider
        .chat(&streamed(ProviderKind::OpenAiCompatible), &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Transient);
    assert!(error.message().contains("no token"), "{error}");
    assert!(
        started.elapsed() < Duration::from_millis(1200),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        stub.requests().len(),
        1,
        "a first-token timeout is not retried"
    );
}

#[tokio::test]
async fn a_slow_first_token_within_its_deadline_is_fine() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::Anthropic);
    stub.inject(FaultRule::new(Fault::SlowFirstToken { delay_ms: 150 }));
    let options = CallOptions::new(
        Timeouts::new(Duration::from_secs(2), Duration::from_secs(10))
            .with_first_token(Duration::from_secs(2)),
    );
    let answer = provider
        .chat(&streamed(ProviderKind::Anthropic), &options)
        .await
        .unwrap();
    assert!(answer.timings.first_token.unwrap() >= Duration::from_millis(150));
    assert!(!answer.stream_fallback);
}

#[tokio::test]
async fn the_whole_answer_has_its_own_deadline() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.set_chunk_delay(Duration::from_millis(100));
    let options = CallOptions::new(Timeouts::new(
        Duration::from_secs(2),
        Duration::from_millis(250),
    ))
    .with_retry(shelfy_ai::RetryPolicy::NONE);
    let error = provider
        .chat(&streamed(ProviderKind::OpenAiCompatible), &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Transient);
    assert!(error.message().contains("longer than 250 ms"), "{error}");
}

#[tokio::test]
async fn a_text_stream_reports_reasoning_free_text() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = ChatRequest::new("m", vec![Message::user_text("hello")]).streamed(true);
    let answer = provider.chat(&request, &options()).await.unwrap();
    assert!(answer.text.starts_with("Stub answer"));
    assert_eq!(answer.json, None);
    assert_eq!(answer.requests, 1);
}
