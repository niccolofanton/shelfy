//! HTTP dictation with generated silence and an in-process provider stub only.
mod support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use shelfy_ai::{
    secrecy::SecretString,
    stub::{Stub, StubConfig},
};
use shelfy_server::{
    ai::OperatorConfig, control::usage_daily, error::ErrorCode, ids::now_ms,
    outbound::OriginAllowlist, rate_limit::Quota,
};
use std::time::Duration;
use support::auth::{control_db, owner, sign_in, spa, with_session};
use support::{TestState, json, problem, send};
const KEY: &str = "synthetic-stt-key";
fn wav(seconds: usize) -> Vec<u8> {
    let len = u32::try_from(seconds * 32_000).unwrap();
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
    out.resize(44 + usize::try_from(len).unwrap(), 0);
    out
}
fn request(audio: Vec<u8>, language: &str) -> Request<Body> {
    Request::post(format!("/api/v1/stt/transcriptions?language={language}"))
        .header("content-type", "audio/wav")
        .body(Body::from(audio))
        .unwrap()
}
async fn stub() -> Stub {
    Stub::start(StubConfig {
        api_key: Some(SecretString::from(KEY)),
        ..StubConfig::default()
    })
    .await
    .unwrap()
}
fn configure(c: &mut shelfy_server::config::Config, stub: &Stub) {
    c.outbound.allow_origins = OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
    c.operator = OperatorConfig {
        url: Some(stub.openai_base()),
        key: Some(SecretString::from(KEY)),
        model: Some("stub-text".into()),
        stt_url: Some(stub.whisper_url()),
        stt_key: Some(SecretString::from(KEY)),
        timeout: Duration::from_secs(10),
        ..OperatorConfig::default()
    };
}
#[tokio::test]
async fn one_wav_reaches_whisper_with_language_and_zero_token_usage() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    let id = owner(&t);
    let app = t.app();
    let session = sign_in(&app, &t).await;
    let response = send(&app, spa(&t, request(wav(1), "en"), &session)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let answer = json(response).await;
    assert!(answer["text"].as_str().unwrap().contains("in en"));
    let requests: Vec<_> = stub
        .requests()
        .into_iter()
        .filter(|r| r.path == "/inference")
        .collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].form.as_ref().unwrap()["language"], "en");
    assert_eq!(requests[0].file.as_ref().unwrap().bytes, wav(1).len());
    let daily = usage_daily::of_day(&control_db(&t), &id, now_ms()).unwrap();
    assert_eq!(daily.ai_calls, 1);
    assert_eq!(daily.ai_in_tokens, 0);
    assert_eq!(daily.ai_out_tokens, 0);
}
#[tokio::test]
async fn auth_csrf_wav_duration_format_and_body_limits_precede_egress() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    owner(&t);
    let app = t.app();
    let session = sign_in(&app, &t).await;
    assert_eq!(
        send(&app, support::from_app(request(wav(1), "it")))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, with_session(request(wav(1), "it"), &session))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    for (audio, language, code) in [
        (wav(121), "it", ErrorCode::SttTooLong),
        (vec![0; 44], "en", ErrorCode::ValidationFailed),
        (wav(1), "long", ErrorCode::ValidationFailed),
    ] {
        let response = send(&app, spa(&t, request(audio, language), &session)).await;
        assert_eq!(
            problem(response, StatusCode::UNPROCESSABLE_ENTITY)
                .await
                .code,
            code
        );
    }
    let oversized = Request::post("/api/v1/stt/transcriptions")
        .header("content-type", "audio/wav")
        .header("content-length", (25 * 1024 * 1024 + 1).to_string())
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        problem(
            send(&app, spa(&t, oversized, &session)).await,
            StatusCode::PAYLOAD_TOO_LARGE
        )
        .await
        .code,
        ErrorCode::PayloadTooLarge
    );
    let wrong_type = Request::post("/api/v1/stt/transcriptions")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(
        send(&app, spa(&t, wrong_type, &session)).await.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert!(stub.requests().is_empty());
}
#[tokio::test]
async fn no_route_and_offline_are_stable_errors() {
    let t = TestState::new();
    owner(&t);
    let app = t.app();
    let session = sign_in(&app, &t).await;
    assert_eq!(
        problem(
            send(&app, spa(&t, request(wav(1), "it"), &session)).await,
            StatusCode::UNPROCESSABLE_ENTITY
        )
        .await
        .code,
        ErrorCode::AiNotConfigured
    );
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    owner(&t);
    let app = t.app();
    let session = sign_in(&app, &t).await;
    stub.set_offline(true).await.unwrap();
    assert_eq!(
        problem(
            send(&app, spa(&t, request(wav(1), "it"), &session)).await,
            StatusCode::SERVICE_UNAVAILABLE
        )
        .await
        .code,
        ErrorCode::ProviderOffline
    );
}
#[tokio::test]
async fn ten_dictation_requests_per_user_are_admitted_then_rate_limited() {
    let t = TestState::with_config(|c| c.rate_limits.stt = Some(Quota::per_hour(10)));
    owner(&t);
    let app = t.app();
    let session = sign_in(&app, &t).await;
    for _ in 0..10 {
        assert_eq!(
            send(&app, spa(&t, request(wav(1), "en"), &session))
                .await
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let response = send(&app, spa(&t, request(wav(1), "en"), &session)).await;
    assert!(response.headers().contains_key("retry-after"));
    assert_eq!(
        problem(response, StatusCode::TOO_MANY_REQUESTS).await.code,
        ErrorCode::RateLimited
    );
}
#[tokio::test]
async fn byok_audio_requires_consent_and_uses_the_configured_model() {
    let stub = stub().await;
    let t = TestState::with_config(|c| {
        c.ai_allow_loopback = true;
        c.vault = shelfy_server::ai::vault::KeyVault::new(
            Some(SecretString::from(STANDARD.encode([42; 32]))),
            None,
        )
        .unwrap();
    });
    owner(&t);
    let app = t.app();
    let session = sign_in(&app, &t).await;
    let put = Request::put("/api/v1/me/providers/voice").header("content-type", "application/json").body(Body::from(serde_json::json!({"label":"Synthetic voice", "kind":"openai_compatible", "baseUrl":stub.openai_base(), "models":{"stt":"specific-whisper"}, "key":KEY}).to_string())).unwrap();
    assert_eq!(
        send(&app, spa(&t, put, &session)).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        problem(
            send(&app, spa(&t, request(wav(1), "en"), &session)).await,
            StatusCode::FORBIDDEN
        )
        .await
        .code,
        ErrorCode::AiConsentRequired
    );
    assert!(stub.requests().is_empty());
    let consent = Request::post("/api/v1/me/providers/voice/consent")
        .header("content-type", "application/json")
        .body(Body::from("{\"version\":\"ai-provider-v1\"}"))
        .unwrap();
    assert_eq!(
        send(&app, spa(&t, consent, &session)).await.status(),
        StatusCode::NO_CONTENT
    );
    let response = send(&app, spa(&t, request(wav(1), "en"), &session)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let log = stub.requests();
    let form = log.last().unwrap().form.as_ref().unwrap();
    assert_eq!(form["model"], "specific-whisper");
    assert_eq!(form["language"], "en");
}
#[tokio::test]
async fn dropping_a_request_releases_the_operator_slot() {
    let stub = stub().await;
    stub.set_latency(Duration::from_secs(1));
    let t = TestState::with_config(|c| configure(c, &stub));
    owner(&t);
    let app = t.app();
    let session = sign_in(&app, &t).await;
    let first_app = app.clone();
    let first = spa(&t, request(wav(1), "en"), &session);
    let pending = tokio::spawn(async move { send(&first_app, first).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !stub.requests().iter().any(|r| r.path == "/inference") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    let _ = pending.await;
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        send(&app, spa(&t, request(wav(1), "en"), &session)),
    )
    .await
    .expect("cancelled request releases its permit");
    assert_eq!(response.status(), StatusCode::OK);
}
#[tokio::test]
async fn members_cannot_send_audio_to_the_owners_operator() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    owner(&t);
    let app = t.app_as(support::library::ALICE);
    let response = send(&app, support::from_app(request(wav(1), "it"))).await;
    assert_eq!(
        problem(response, StatusCode::UNPROCESSABLE_ENTITY)
            .await
            .code,
        ErrorCode::AiNotConfigured
    );
    assert!(stub.requests().is_empty());
}
