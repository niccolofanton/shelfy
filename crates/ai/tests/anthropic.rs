//! The Anthropic Messages adapter against the stub: headers, the top-level
//! system prompt, base64 image blocks, forced tool use, `output_config`,
//! `input_json_delta` streaming, and the calls Anthropic does not serve.

mod support;

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use secrecy::SecretString;
use serde_json::{Value, json};
use shelfy_ai::stub::{AuthSeen, Endpoint, StubConfig};
use shelfy_ai::{
    ChatRequest, EmbedRequest, ErrorKind, FinishReason, Message, Part, ProviderConfig,
    ProviderKind, ReasoningEffort, Source, StructuredMode, TranscribeRequest,
};
use support::*;

const KEY: &str = "sk-ant-test-0000";

async fn keyed_stub() -> shelfy_ai::stub::Stub {
    stub_with(StubConfig {
        api_key: Some(SecretString::from(KEY)),
        ..StubConfig::default()
    })
    .await
}

fn anthropic(stub: &shelfy_ai::stub::Stub, mode: StructuredMode) -> shelfy_ai::Provider {
    provider(
        stub,
        ProviderConfig::new(ProviderKind::Anthropic, Source::Operator, stub.url())
            .with_key(SecretString::from(KEY))
            .with_structured(mode),
    )
}

#[tokio::test]
async fn a_text_call_uses_the_messages_shape() {
    let stub = keyed_stub().await;
    let provider = anthropic(&stub, StructuredMode::JsonSchema);
    let request = ChatRequest::new("claude-x", vec![Message::user_text("Which lamp?")])
        .with_system("You are a librarian.")
        .with_temperature(0.3);
    let answer = provider.chat(&request, &options()).await.unwrap();
    assert!(answer.text.starts_with("Stub answer "));
    assert_eq!(answer.finish, FinishReason::Stop);
    assert!(answer.usage.unwrap().input_tokens > 0);

    let logged = only_request(&stub);
    assert_eq!(logged.endpoint, Endpoint::Messages);
    assert_eq!(logged.path, "/v1/messages");
    assert_eq!(logged.auth, AuthSeen::Valid);
    assert_eq!(logged.headers["anthropic-version"], "2023-06-01");
    let body = logged.body.unwrap();
    assert_eq!(body["system"], "You are a librarian.");
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": "Which lamp?"}])
    );
    assert_eq!(body["max_tokens"], 4096, "the API requires a cap");
    assert_eq!(body["temperature"], 0.3);
    assert!(body.get("output_config").is_none());
    assert!(body.get("tools").is_none());
}

#[tokio::test]
async fn temperature_is_left_out_when_the_provider_refuses_it() {
    let stub = keyed_stub().await;
    let mut config = ProviderConfig::new(ProviderKind::Anthropic, Source::Operator, stub.url())
        .with_key(SecretString::from(KEY));
    config.send_temperature = false;
    let provider = provider(&stub, config);
    let request = ChatRequest::new("claude-x", vec![Message::user_text("x")]).with_temperature(0.2);
    provider.chat(&request, &options()).await.unwrap();
    assert!(
        only_request(&stub)
            .body
            .unwrap()
            .get("temperature")
            .is_none()
    );
    let preset = shelfy_ai::presets::preset("anthropic").unwrap();
    assert!(!preset.send_temperature);
    assert!(!preset.config(None, None).unwrap().send_temperature);
}

#[tokio::test]
async fn images_are_base64_blocks() {
    let stub = keyed_stub().await;
    let provider = anthropic(&stub, StructuredMode::JsonSchema);
    let image = webp();
    let request = ChatRequest::new(
        "claude-x",
        vec![Message::user(vec![
            Part::webp(image.clone()),
            Part::text("Catalog this."),
        ])],
    );
    provider.chat(&request, &options()).await.unwrap();
    let logged = only_request(&stub);
    assert_eq!(logged.images(), 1);
    let content = &logged.body.unwrap()["messages"][0]["content"];
    assert_eq!(
        content[0],
        json!({"type": "image", "source": {
            "type": "base64",
            "media_type": "image/webp",
            "data": base64::engine::general_purpose::STANDARD.encode(&image),
        }})
    );
    assert_eq!(content[1], json!({"type": "text", "text": "Catalog this."}));
}

#[tokio::test]
async fn forced_tool_use_carries_the_schema() {
    let stub = keyed_stub().await;
    let provider = anthropic(&stub, StructuredMode::Tool);
    let request = ChatRequest::new("claude-x", vec![Message::user_text("a lamp")])
        .with_system("Catalog the post.")
        .with_json(catalog())
        .with_max_tokens(768);
    let answer = provider.chat(&request, &options()).await.unwrap();
    assert_eq!(answer.finish, FinishReason::ToolUse);
    let json = answer.json.unwrap();
    assert!(json["specific_tags"].is_array());

    let body = only_request(&stub).body.unwrap();
    assert_eq!(body["system"], "Catalog the post.");
    assert_eq!(body["max_tokens"], 768);
    assert_eq!(body["tools"][0]["name"], "catalog");
    assert_eq!(body["tools"][0]["input_schema"], catalog_schema());
    assert_eq!(body["tools"][0]["strict"], true);
    assert!(
        body["tools"][0].get("eager_input_streaming").is_none(),
        "not streamed"
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type": "tool", "name": "catalog"})
    );
}

#[tokio::test]
async fn streamed_tool_input_is_joined_and_reported_as_it_grows() {
    let stub = keyed_stub().await;
    let provider = anthropic(&stub, StructuredMode::Tool);
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let options =
        options().with_text_callback(move |text| sink.lock().unwrap().push(text.to_owned()));
    let request = ChatRequest::new("claude-x", vec![Message::user_text("a lamp")])
        .with_json(catalog())
        .streamed(true);
    let answer = provider.chat(&request, &options).await.unwrap();
    let seen = seen.lock().unwrap().clone();
    assert!(seen.len() > 2, "{seen:?}");
    assert_eq!(seen.last().unwrap(), &answer.text);
    assert_eq!(
        serde_json::from_str::<Value>(&answer.text).unwrap(),
        answer.json.unwrap()
    );
    assert!(answer.usage.unwrap().output_tokens > 0);
    let logged = only_request(&stub);
    assert!(logged.streamed());
    assert_eq!(
        logged.body.unwrap()["tools"][0]["eager_input_streaming"],
        true
    );
}

#[tokio::test]
async fn json_schema_mode_uses_output_config_with_the_effort() {
    let stub = keyed_stub().await;
    let provider = anthropic(&stub, StructuredMode::JsonSchema);
    let request = ChatRequest::new("claude-x", vec![Message::user_text("a lamp")])
        .with_json(catalog())
        .with_reasoning_effort(ReasoningEffort::Low)
        .streamed(true);
    let answer = provider.chat(&request, &options()).await.unwrap();
    assert!(answer.json.is_some());
    let body = only_request(&stub).body.unwrap();
    assert_eq!(
        body["output_config"],
        json!({"effort": "low", "format": {"type": "json_schema", "schema": catalog_schema()}})
    );
    assert!(body.get("tools").is_none());
}

#[tokio::test]
async fn json_object_mode_is_the_prompt_alone() {
    let stub = keyed_stub().await;
    let provider = anthropic(&stub, StructuredMode::JsonObject);
    let request = ChatRequest::new("claude-x", vec![Message::user_text("a lamp")])
        .with_system("Catalog.")
        .with_json(catalog());
    assert!(
        provider
            .chat(&request, &options())
            .await
            .unwrap()
            .json
            .is_some()
    );
    let body = only_request(&stub).body.unwrap();
    assert!(
        body["system"]
            .as_str()
            .unwrap()
            .contains(shelfy_ai::structured::JSON_INSTRUCTION)
    );
    assert!(body.get("output_config").is_none() && body.get("tools").is_none());
}

#[tokio::test]
async fn embeddings_and_transcription_are_unsupported() {
    let stub = keyed_stub().await;
    let provider = anthropic(&stub, StructuredMode::JsonSchema);
    let embed = EmbedRequest {
        model: "x".into(),
        input: vec!["a".into()],
        dimensions: None,
    };
    assert_eq!(
        provider.embed(&embed, &options()).await.unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    let transcribe = TranscribeRequest {
        wav: wav(160).into(),
        language: None,
        model: None,
    };
    assert_eq!(
        provider
            .transcribe(&transcribe, &options())
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Unsupported
    );
    assert!(stub.requests().is_empty());
}

#[tokio::test]
async fn models_use_anthropics_list() {
    let stub = keyed_stub().await;
    let provider = anthropic(&stub, StructuredMode::JsonSchema);
    let models = provider.models(&options()).await.unwrap();
    assert_eq!(models[0].id, "stub-text");
    assert_eq!(models[0].display_name.as_deref(), Some("stub-text"));
    let logged = only_request(&stub);
    assert_eq!(logged.path, "/v1/models");
    assert_eq!(logged.query.as_deref(), Some("limit=1000"));
    assert_eq!(logged.auth, AuthSeen::Valid);
}

#[tokio::test]
async fn a_wrong_key_is_an_invalid_key() {
    let stub = keyed_stub().await;
    let provider = provider(
        &stub,
        ProviderConfig::new(ProviderKind::Anthropic, Source::Operator, stub.url())
            .with_key(SecretString::from("sk-ant-wrong")),
    );
    let error = provider
        .chat(
            &ChatRequest::new("claude-x", vec![Message::user_text("x")]),
            &options(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidKey);
    assert_eq!(error.status(), Some(401));
    assert_eq!(only_request(&stub).auth, AuthSeen::Invalid);
}
