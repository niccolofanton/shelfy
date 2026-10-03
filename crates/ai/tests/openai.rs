//! The OpenAI-compatible adapter against the stub: request shapes, the base
//! URL used verbatim, images, structured output, streaming, embeddings,
//! transcription, models and the health probe.

mod support;

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Value, json};
use shelfy_ai::stub::{AuthSeen, Endpoint};
use shelfy_ai::{
    ChatRequest, EmbedRequest, ErrorKind, FinishReason, MaxTokensField, Message, Part,
    ProviderConfig, ProviderKind, ReasoningEffort, Source, StructuredMode, TranscribeRequest,
};
use support::*;

#[tokio::test]
async fn a_text_chat_sends_the_system_prompt_and_sampling_fields() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = ChatRequest::new(
        "ornith-1.5-35b-a3b",
        vec![Message::user_text("Which lamp?")],
    )
    .with_system("You are a librarian.")
    .with_temperature(0.2)
    .with_max_tokens(256)
    .with_reasoning_effort(ReasoningEffort::Low);
    let answer = provider.chat(&request, &options()).await.unwrap();

    assert!(answer.text.starts_with("Stub answer "), "{}", answer.text);
    assert_eq!(answer.finish, FinishReason::Stop);
    assert_eq!(answer.json, None);
    assert_eq!(answer.requests, 1);
    assert_eq!(answer.model.as_deref(), Some("ornith-1.5-35b-a3b"));
    let usage = answer.usage.unwrap();
    assert!(usage.input_tokens > 0 && usage.output_tokens > 0);
    assert!(answer.server_timings.unwrap().predicted_tokens.is_some());

    let logged = only_request(&stub);
    assert_eq!(logged.endpoint, Endpoint::Chat);
    assert_eq!(logged.path, "/v1/chat/completions");
    let body = logged.body.unwrap();
    assert_eq!(body["model"], "ornith-1.5-35b-a3b");
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "You are a librarian."},
            {"role": "user", "content": "Which lamp?"}
        ])
    );
    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["max_tokens"], 256);
    assert_eq!(body["reasoning_effort"], "low");
    assert!(body.get("response_format").is_none());
    assert!(body.get("stream").is_none());
    assert_eq!(logged.headers["content-type"], "application/json");
}

#[tokio::test]
async fn optional_fields_are_left_out_and_the_token_field_follows_the_config() {
    let stub = stub().await;
    let mut config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        Source::Operator,
        stub.openai_base(),
    );
    config.max_tokens_field = MaxTokensField::MaxCompletionTokens;
    let provider = provider(&stub, config);
    let request = ChatRequest::new("m", vec![Message::user_text("hi")]).with_max_tokens(64);
    provider.chat(&request, &options()).await.unwrap();
    let body = only_request(&stub).body.unwrap();
    assert_eq!(body["max_completion_tokens"], 64);
    for absent in ["max_tokens", "temperature", "reasoning_effort"] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
}

#[tokio::test]
async fn the_base_url_is_used_verbatim_like_geminis() {
    let stub = stub().await;
    let base = stub.url().join("/v1beta/openai/").unwrap();
    let provider = provider(
        &stub,
        ProviderConfig::new(ProviderKind::OpenAiCompatible, Source::Operator, base),
    );
    provider
        .chat(
            &ChatRequest::new("gemini-x", vec![Message::user_text("hi")]),
            &options(),
        )
        .await
        .unwrap();
    provider.models(&options()).await.unwrap();
    let paths: Vec<String> = stub
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert_eq!(
        paths,
        ["/v1beta/openai/chat/completions", "/v1beta/openai/models"]
    );
}

#[tokio::test]
async fn images_travel_as_data_urls() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let image = webp();
    let request = ChatRequest::new(
        "qwen3.8-27b",
        vec![Message::user(vec![
            Part::text("Catalog this."),
            Part::webp(image.clone()),
        ])],
    );
    provider.chat(&request, &options()).await.unwrap();
    let logged = only_request(&stub);
    assert_eq!(logged.images(), 1);
    let content = &logged.body.unwrap()["messages"][0]["content"];
    assert_eq!(content[0], json!({"type": "text", "text": "Catalog this."}));
    let expected = format!(
        "data:image/webp;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&image)
    );
    assert_eq!(
        content[1],
        json!({"type": "image_url", "image_url": {"url": expected}})
    );
}

#[tokio::test]
async fn a_strict_json_schema_answer_is_validated() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = ChatRequest::new("m", vec![Message::user_text("a lamp")])
        .with_system("Catalog the post.")
        .with_json(catalog());
    let answer = provider.chat(&request, &options()).await.unwrap();
    let json = answer.json.unwrap();
    assert!(json["general_tags"].is_array());
    assert_eq!(serde_json::from_str::<Value>(&answer.text).unwrap(), json);
    assert!(!answer.repaired);

    let body = only_request(&stub).body.unwrap();
    assert_eq!(
        body["response_format"],
        json!({"type": "json_schema", "json_schema": {"name": "catalog", "schema": catalog_schema(), "strict": true}})
    );
    // Strict mode leaves the system prompt alone.
    assert_eq!(body["messages"][0]["content"], "Catalog the post.");
}

#[tokio::test]
async fn json_object_mode_puts_the_schema_in_the_system_prompt() {
    let stub = stub().await;
    let provider = operator_with_mode(
        &stub,
        ProviderKind::OpenAiCompatible,
        StructuredMode::JsonObject,
    );
    let request = ChatRequest::new("m", vec![Message::user_text("a lamp")])
        .with_system("Catalog the post.")
        .with_json(catalog());
    let answer = provider.chat(&request, &options()).await.unwrap();
    assert!(answer.json.is_some());
    let logged = only_request(&stub);
    assert_eq!(response_format(&logged).as_deref(), Some("json_object"));
    let system = logged.body.unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(system.starts_with("Catalog the post.\n\n"));
    assert!(system.contains(shelfy_ai::structured::JSON_INSTRUCTION));
    assert!(system.ends_with(&catalog_schema().to_string()));
}

#[tokio::test]
async fn the_structured_mode_can_be_overridden_per_call() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = ChatRequest::new("m", vec![Message::user_text("x")]).with_json(catalog());
    provider
        .chat(
            &request,
            &options().with_structured(StructuredMode::JsonObject),
        )
        .await
        .unwrap();
    assert_eq!(
        response_format(&only_request(&stub)).as_deref(),
        Some("json_object")
    );
}

#[tokio::test]
async fn forced_tools_are_an_anthropic_mode() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = ChatRequest::new("m", vec![Message::user_text("x")]).with_json(catalog());
    let error = provider
        .chat(&request, &options().with_structured(StructuredMode::Tool))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert!(stub.requests().is_empty());
}

#[tokio::test]
async fn streamed_text_reaches_the_callback_and_the_usage_chunk_is_read() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let options =
        options().with_text_callback(move |text| sink.lock().unwrap().push(text.to_owned()));
    let request = ChatRequest::new("m", vec![Message::user_text("hello")])
        .with_json(catalog())
        .streamed(true);
    let answer = provider.chat(&request, &options).await.unwrap();

    let seen = seen.lock().unwrap().clone();
    assert!(seen.len() > 2, "{seen:?}");
    assert!(
        seen.windows(2).all(|pair| pair[1].starts_with(&pair[0])),
        "the text only grows"
    );
    assert_eq!(seen.last().unwrap(), &answer.text);
    assert!(answer.json.is_some());
    assert!(answer.usage.unwrap().output_tokens > 0);
    assert!(answer.timings.first_token.is_some());
    assert!(!answer.stream_fallback);
    let logged = only_request(&stub);
    assert!(logged.streamed());
    assert_eq!(logged.headers["accept"], "text/event-stream");
    assert_eq!(
        logged.body.unwrap()["stream_options"],
        json!({"include_usage": true})
    );
}

#[tokio::test]
async fn extra_fields_are_added_without_replacing_the_adapters() {
    let stub = stub().await;
    let config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        Source::Operator,
        stub.openai_base(),
    )
    .with_extra("chat_template_kwargs", json!({"enable_thinking": false}))
    .with_extra("model", json!("hijacked"))
    .with_extra("cache_prompt", json!(true));
    let provider = provider(&stub, config);
    let request = ChatRequest::new("m", vec![Message::user_text("x")])
        .with_extra("cache_prompt", json!(false));
    provider.chat(&request, &options()).await.unwrap();
    let body = only_request(&stub).body.unwrap();
    assert_eq!(
        body["chat_template_kwargs"],
        json!({"enable_thinking": false})
    );
    assert_eq!(body["model"], "m");
    assert_eq!(
        body["cache_prompt"], false,
        "the request's extras win over the provider's"
    );
}

#[tokio::test]
async fn a_provider_without_webp_refuses_webp_images_before_sending() {
    let stub = stub().await;
    let config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        Source::Operator,
        stub.openai_base(),
    )
    .without_webp();
    let provider = provider(&stub, config);
    let with_webp = ChatRequest::new(
        "qwen3.8-27b",
        vec![Message::user(vec![Part::text("x"), Part::webp(webp())])],
    );
    let error = provider.chat(&with_webp, &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert!(stub.requests().is_empty());
    let jpeg = shelfy_ai::Image {
        media_type: shelfy_ai::ImageType::Jpeg,
        data: b"\xff\xd8\xff\xe0 not really a jpeg".to_vec().into(),
    };
    let with_jpeg = ChatRequest::new(
        "qwen3.8-27b",
        vec![Message::user(vec![Part::text("x"), Part::Image(jpeg)])],
    );
    provider.chat(&with_jpeg, &options()).await.unwrap();
    let body = only_request(&stub).body.unwrap();
    let url = body["messages"][0]["content"][1]["image_url"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(url.starts_with("data:image/jpeg;base64,"), "{url}");
}

#[tokio::test]
async fn a_llama_like_stub_answers_webp_with_its_400() {
    let stub = stub_with(shelfy_ai::stub::StubConfig {
        webp_images: false,
        ..shelfy_ai::stub::StubConfig::default()
    })
    .await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = ChatRequest::new(
        "qwen3.8-27b",
        vec![Message::user(vec![Part::text("x"), Part::webp(webp())])],
    );
    let error = provider.chat(&request, &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::BadRequest);
    assert_eq!(error.status(), Some(400));
    assert_eq!(error.message(), "Failed to load image or audio file");
}

#[tokio::test]
async fn embeddings_come_back_in_order() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = EmbedRequest {
        model: "stub-embed".into(),
        input: vec!["lamp".into(), "chair".into(), "lamp".into()],
        dimensions: Some(8),
    };
    let embeddings = provider.embed(&request, &options()).await.unwrap();
    assert_eq!(embeddings.vectors.len(), 3);
    assert!(embeddings.vectors.iter().all(|vector| vector.len() == 8));
    assert_eq!(embeddings.vectors[0], embeddings.vectors[2]);
    assert_ne!(embeddings.vectors[0], embeddings.vectors[1]);
    assert!(embeddings.usage.unwrap().input_tokens > 0);
    let logged = only_request(&stub);
    assert_eq!(logged.path, "/v1/embeddings");
    let body = logged.body.unwrap();
    assert_eq!(body["input"], json!(["lamp", "chair", "lamp"]));
    assert_eq!(body["encoding_format"], "float");
    assert_eq!(body["dimensions"], 8);
}

#[tokio::test]
async fn transcriptions_send_the_desktop_form() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let audio = wav(1600);
    let request = TranscribeRequest {
        wav: audio.clone().into(),
        language: Some("it".into()),
        model: Some("whisper-1".into()),
    };
    let transcript = provider.transcribe(&request, &options()).await.unwrap();
    assert_eq!(
        transcript.text,
        format!("stub transcript of {} bytes in it", audio.len())
    );
    let logged = only_request(&stub);
    assert_eq!(logged.path, "/v1/audio/transcriptions");
    let form = logged.form.unwrap();
    assert_eq!(form["model"], "whisper-1");
    assert_eq!(form["response_format"], "json");
    assert_eq!(form["temperature"], "0");
    assert_eq!(form["language"], "it");
    let file = logged.file.unwrap();
    assert_eq!(file.filename.as_deref(), Some("audio.wav"));
    assert_eq!(file.content_type.as_deref(), Some("audio/wav"));
    assert_eq!(file.bytes, audio.len());

    let without_model = TranscribeRequest {
        model: None,
        ..request
    };
    let error = provider
        .transcribe(&without_model, &options())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::BadRequest);
}

#[tokio::test]
async fn models_and_the_optional_health_probe() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let models: Vec<String> = provider
        .models(&options())
        .await
        .unwrap()
        .into_iter()
        .map(|model| model.id)
        .collect();
    assert_eq!(models, ["stub-text", "stub-vision", "stub-embed"]);
    assert_eq!(
        provider.health(&options()).await.unwrap_err().kind(),
        ErrorKind::Unsupported
    );

    let config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        Source::Operator,
        stub.openai_base(),
    )
    .with_key(secrecy::SecretString::from("operator-key"))
    .with_llama_health();
    assert_eq!(config.health_url.as_ref(), Some(&stub.health_url()));
    let probed = support::provider(&stub, config);
    probed.health(&options()).await.unwrap();
    let health = stub.requests().pop().unwrap();
    assert_eq!(health.endpoint, Endpoint::Health);
    assert_eq!(health.method, "GET");
    assert_eq!(health.auth, AuthSeen::Valid);
}
