//! Structured output against the stub: validation, the one repair call that
//! quotes the error, `schema_invalid`, a cut answer, refusals.

mod support;

use serde_json::Value;
use shelfy_ai::stub::{Endpoint, Fault, FaultRule, key_of};
use shelfy_ai::{ChatRequest, ErrorKind, Message, ProviderKind, StructuredMode};
use support::*;

fn catalog_request() -> ChatRequest {
    ChatRequest::new("m", vec![Message::user_text("a blown-glass lamp")])
        .with_system("Catalog the post.")
        .with_json(catalog())
}

#[tokio::test]
async fn a_non_conforming_answer_gets_one_repair_call_that_quotes_the_error() {
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        let stub = stub().await;
        let provider = operator(&stub, kind);
        stub.inject(FaultRule::new(Fault::NonConformingJson));
        let answer = provider.chat(&catalog_request(), &options()).await.unwrap();
        assert!(answer.repaired, "{kind:?}");
        assert_eq!(answer.requests, 2);
        assert!(answer.json.unwrap()["general_tags"].is_array());

        let requests = stub.requests();
        assert_eq!(requests.len(), 2);
        let messages = requests[1].body.as_ref().unwrap()["messages"]
            .as_array()
            .unwrap()
            .clone();
        let texts: Vec<&str> = messages
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect();
        assert!(
            texts.contains(&r#"{"stub":"nonconforming"}"#),
            "{kind:?}: the bad answer goes back"
        );
        let repair = texts.last().unwrap();
        assert!(
            repair.contains("does not match the required JSON schema"),
            "{repair}"
        );
        assert!(
            repair.contains("description"),
            "the validation error is quoted: {repair}"
        );
        assert_eq!(messages.last().unwrap()["role"], "user");
    }
}

#[tokio::test]
async fn a_second_bad_answer_is_schema_invalid() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::NonConformingJson).times(2));
    let error = provider
        .chat(&catalog_request(), &options())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::SchemaInvalid);
    assert!(
        !error.message().contains("nonconforming"),
        "no answer content in the error"
    );
    assert_eq!(stub.requests().len(), 2);
}

#[tokio::test]
async fn text_that_is_not_json_is_repaired_too() {
    let stub = stub().await;
    let provider = operator_with_mode(
        &stub,
        ProviderKind::OpenAiCompatible,
        StructuredMode::JsonObject,
    );
    let request = catalog_request();
    stub.add_canned(key_of(&request), "Sure! Here is the catalog: lamp, glass.");
    let answer = provider.chat(&request, &options()).await.unwrap();
    assert!(answer.repaired);
    let repair = stub.requests()[1].body.clone().unwrap();
    let last = repair["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(last.contains("not valid JSON"), "{last}");
}

#[tokio::test]
async fn a_canned_valid_answer_needs_no_repair() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = catalog_request();
    let canned = r#"{"description":"A lamp","general_tags":["design"],"specific_tags":["murano glass"],"language":"en"}"#;
    stub.add_canned(key_of(&request), canned);
    let answer = provider.chat(&request, &options()).await.unwrap();
    assert_eq!(
        answer.json.unwrap(),
        serde_json::from_str::<Value>(canned).unwrap()
    );
    assert_eq!(answer.requests, 1);
}

#[tokio::test]
async fn an_answer_cut_at_the_token_cap_gets_no_repair() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    let request = catalog_request().with_max_tokens(5);
    let error = provider.chat(&request, &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::SchemaInvalid);
    assert!(error.message().contains("token cap"), "{error}");
    assert_eq!(stub.requests().len(), 1);
}

#[tokio::test]
async fn a_refusal_is_refused_on_both_protocols() {
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        for stream in [false, true] {
            let stub = stub().await;
            let provider = operator(&stub, kind);
            stub.inject(FaultRule::new(Fault::Refusal));
            let error = provider
                .chat(&catalog_request().streamed(stream), &options())
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Refused, "{kind:?} stream={stream}");
            assert_eq!(
                stub.requests().len(),
                1,
                "no retry or repair after a refusal"
            );
        }
    }
}

#[tokio::test]
async fn faults_can_target_one_endpoint() {
    let stub = stub().await;
    let provider = operator(&stub, ProviderKind::OpenAiCompatible);
    stub.inject(FaultRule::new(Fault::NonConformingJson).on(Endpoint::Messages));
    let answer = provider.chat(&catalog_request(), &options()).await.unwrap();
    assert!(!answer.repaired, "the fault waits for an Anthropic call");
}
