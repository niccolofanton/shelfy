//! The whisper.cpp adapter against the stub's `/inference`: the desktop's
//! multipart form, the URL used as is, an optional Bearer key.

mod support;

use secrecy::SecretString;
use shelfy_ai::stub::{AuthSeen, Endpoint, Fault, FaultRule, StubConfig};
use shelfy_ai::{
    ChatRequest, ErrorKind, Message, ProviderConfig, ProviderKind, Source, TranscribeRequest,
};
use support::*;

fn recording(language: Option<&str>) -> TranscribeRequest {
    TranscribeRequest {
        wav: wav(16_000).into(),
        language: language.map(str::to_owned),
        model: None,
    }
}

#[tokio::test]
async fn inference_sends_the_desktop_form() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::WhisperCpp);
    let transcript = provider
        .transcribe(&recording(Some("it")), &options())
        .await
        .unwrap();
    assert_eq!(
        transcript.text,
        format!("stub transcript of {} bytes in it", wav(16_000).len())
    );
    assert_eq!(transcript.requests, 1);

    let logged = only_request(&stub);
    assert_eq!(logged.endpoint, Endpoint::Inference);
    assert_eq!(logged.path, "/inference");
    assert_eq!(logged.auth, AuthSeen::None);
    let form = logged.form.unwrap();
    assert_eq!(form["response_format"], "json");
    assert_eq!(form["temperature"], "0");
    assert_eq!(form["language"], "it");
    assert!(!form.contains_key("model"));
    let file = logged.file.unwrap();
    assert_eq!(file.field, "file");
    assert_eq!(file.filename.as_deref(), Some("audio.wav"));
    assert_eq!(file.content_type.as_deref(), Some("audio/wav"));
    assert!(logged.headers["content-type"].starts_with("multipart/form-data; boundary="));
}

#[tokio::test]
async fn the_language_is_optional_and_the_text_is_trimmed() {
    let stub = stub().await;
    let audio = wav(800);
    stub.add_canned(shelfy_ai::stub::sha256_hex(&audio), "  ciao a tutti \n");
    let provider = operator(&stub, ProviderKind::WhisperCpp);
    let request = TranscribeRequest {
        wav: audio.into(),
        language: None,
        model: None,
    };
    assert_eq!(
        provider
            .transcribe(&request, &options())
            .await
            .unwrap()
            .text,
        "ciao a tutti"
    );
    assert!(!only_request(&stub).form.unwrap().contains_key("language"));
}

#[tokio::test]
async fn an_optional_bearer_key_is_sent() {
    let stub = stub_with(StubConfig {
        api_key: Some(SecretString::from("stt-key")),
        ..StubConfig::default()
    })
    .await;
    let keyed = provider(
        &stub,
        ProviderConfig::new(
            ProviderKind::WhisperCpp,
            Source::Operator,
            stub.whisper_url(),
        )
        .with_key(SecretString::from("stt-key")),
    );
    keyed
        .transcribe(&recording(None), &options())
        .await
        .unwrap();
    let keyless = operator(&stub, ProviderKind::WhisperCpp);
    let error = keyless
        .transcribe(&recording(None), &options())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidKey);
    let seen: Vec<AuthSeen> = stub
        .requests()
        .into_iter()
        .map(|request| request.auth)
        .collect();
    assert_eq!(seen, [AuthSeen::Valid, AuthSeen::None]);
}

#[tokio::test]
async fn a_file_that_is_not_a_wav_is_refused_before_sending() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::WhisperCpp);
    let request = TranscribeRequest {
        wav: b"OggS not a wav file".to_vec().into(),
        language: None,
        model: None,
    };
    assert_eq!(
        provider
            .transcribe(&request, &options())
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::BadRequest
    );
    assert!(stub.requests().is_empty());
}

#[tokio::test]
async fn faults_map_like_every_other_endpoint() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::WhisperCpp);
    stub.inject(FaultRule::new(Fault::ServerError).on(Endpoint::Inference));
    let transcript = provider
        .transcribe(&recording(None), &options())
        .await
        .unwrap();
    assert_eq!(transcript.requests, 2, "one retry after the 500");
    stub.inject(FaultRule::new(Fault::QuotaExhausted));
    let error = provider
        .transcribe(&recording(None), &options())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::QuotaExhausted);
    assert_eq!(error.status(), Some(402));
}

#[tokio::test]
async fn a_whisper_server_only_transcribes() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::WhisperCpp);
    let chat = provider
        .chat(
            &ChatRequest::new("m", vec![Message::user_text("x")]),
            &options(),
        )
        .await
        .unwrap_err();
    assert_eq!(chat.kind(), ErrorKind::Unsupported);
    assert_eq!(
        provider.models(&options()).await.unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    assert!(stub.requests().is_empty());
}
