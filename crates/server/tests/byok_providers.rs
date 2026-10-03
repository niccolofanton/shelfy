//! BYOK HTTP contract and service behavior, with synthetic keys and local stubs.
mod support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use shelfy_ai::{
    ChatRequest, ErrorKind, Message,
    secrecy::SecretString,
    stub::{Stub, StubConfig},
};
use shelfy_server::{
    ai::{AiServiceError, CallHints, Caller, Task, providers::CONSENT_VERSION},
    control::provider_keys,
};
use std::time::Duration;
use support::auth::{control_db, owner, post, sign_in, spa, with_session};
use support::{TestState, body, get, json as response_json, send};
const KEY: &str = "synthetic-byok-never-leak-8193";
fn configured() -> TestState {
    TestState::with_config(|c| {
        c.vault = shelfy_server::ai::vault::KeyVault::new(
            Some(SecretString::from(STANDARD.encode([73; 32]))),
            None,
        )
        .unwrap();
        c.ai_allow_loopback = true;
    })
}
async fn stub(latency: Duration) -> Stub {
    Stub::start(StubConfig {
        api_key: Some(SecretString::from(KEY)),
        latency,
        ..StubConfig::default()
    })
    .await
    .unwrap()
}
fn input(url: &str) -> Value {
    json!({"kind":"openai_compatible","label":"Synthetic provider","baseUrl":url,"models":{"chat":"stub-text","catalog":"stub-vision","embed":"stub-embed", "qc":"specific-qc", "cluster":"specific-cluster", "alias":"specific-alias"},"key":KEY,"prices":{"inputPerMillionUsd":0.5,"outputPerMillionUsd":1.5}})
}
fn put(id: &str, value: Value) -> Request<Body> {
    Request::put(format!("/api/v1/me/providers/{id}"))
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
async fn install(t: &TestState, stub: &Stub) -> (String, String) {
    let id = owner(t);
    let app = t.app();
    let session = sign_in(&app, t).await;
    let r = send(
        &app,
        spa(
            t,
            put("custom", input(stub.openai_base().as_str())),
            &session,
        ),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    (id, session)
}
async fn consent(t: &TestState, session: &str) {
    let req = Request::post("/api/v1/me/providers/custom/consent")
        .header("content-type", "application/json")
        .body(Body::from(json!({"version":CONSENT_VERSION}).to_string()))
        .unwrap();
    assert_eq!(
        send(&t.app(), spa(t, req, session)).await.status(),
        StatusCode::NO_CONTENT
    );
}
#[tokio::test]
async fn sealed_write_only_and_no_consent_for_synthetic_probes() {
    let stub = stub(Duration::ZERO).await;
    let t = configured();
    let (id, session) = install(&t, &stub).await;
    let row = t
        .state
        .control()
        .read(|c| provider_keys::get(c, &id, "custom"))
        .unwrap()
        .unwrap();
    assert_eq!(row.sealed.last4, "8193");
    assert!(
        !row.sealed
            .ciphertext
            .windows(KEY.len())
            .any(|w| w == KEY.as_bytes())
    );
    let request = ChatRequest::new(
        "stub-text",
        vec![Message::user_text("Private library content")],
    );
    assert!(matches!(
        t.state
            .ai()
            .chat(
                &t.state,
                Caller::new(&id, false),
                Task::Chat,
                &request,
                CallHints::new()
            )
            .await,
        Err(AiServiceError::ConsentRequired(_))
    ));
    assert!(stub.requests().is_empty());
    let r = send(
        &t.app(),
        spa(&t, post("/api/v1/me/providers/custom/test"), &session),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let probes = response_json(r).await;
    for task in ["models", "text", "vision", "schema"] {
        assert_eq!(probes[task]["ok"], true, "{task}: {probes}");
    }
    assert_eq!(stub.requests().len(), 4);
    let logs = serde_json::to_string(&stub.requests()).unwrap();
    assert!(!logs.contains("Private library content"));
    assert!(!logs.contains(KEY));
    let response = send(
        &t.app(),
        with_session(get("/api/v1/me/providers"), &session),
    )
    .await;
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = body(response).await;
    assert!(!String::from_utf8_lossy(&bytes).contains(KEY));
    let summaries: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(summaries[0]["configured"], true);
    assert_eq!(summaries[0]["last4"], "8193");
    assert_eq!(summaries[0]["taskModels"]["qc"], "specific-qc");
    assert_eq!(summaries[0]["taskModels"]["cluster"], "specific-cluster");
    assert_eq!(summaries[0]["taskModels"]["alias"], "specific-alias");
    assert!(summaries[0]["consent"].is_null());
    consent(&t, &session).await;
    consent(&t, &session).await;
    t.state
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
    assert_eq!(control_db(&t).query_row("SELECT count(*) FROM audit_log WHERE action='ai.provider.consent' AND target='custom'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert!(
        t.state
            .control()
            .read(|c| provider_keys::get(c, &id, "custom"))
            .unwrap()
            .unwrap()
            .last_used_at
            .is_some()
    );
}
#[tokio::test]
async fn vault_disabled_is_conflict_and_mutations_are_authenticated() {
    let t = TestState::new();
    let app = t.app();
    assert_eq!(
        send(
            &app,
            support::auth::from_spa(&t, put("custom", input("https://8.8.8.8/v1")))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let session = sign_in(&app, &t).await;
    let r = send(
        &app,
        spa(&t, put("custom", input("https://8.8.8.8/v1")), &session),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(response_json(r).await["code"], "ai_vault_disabled");
    assert_eq!(
        send(
            &app,
            with_session(put("custom", input("https://8.8.8.8/v1")), &session)
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    for path in [
        "/api/v1/me/providers/custom/test",
        "/api/v1/me/providers/custom/consent",
    ] {
        assert_eq!(
            send(&app, support::auth::from_spa(&t, post(path)))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
#[tokio::test]
async fn guards_refuse_private_http_operator_and_unsafe_shapes() {
    let t = TestState::with_config(|c| {
        c.vault = shelfy_server::ai::vault::KeyVault::new(
            Some(SecretString::from(STANDARD.encode([74; 32]))),
            None,
        )
        .unwrap()
    });
    let app = t.app();
    let session = sign_in(&app, &t).await;
    for url in [
        "http://8.8.8.8/v1",
        "https://127.0.0.1/v1",
        "https://192.168.1.2/v1",
        "https://[::1]/v1",
        "https://user:pass@8.8.8.8/v1",
        "https://8.8.8.8:444/v1",
        "https://8.8.8.8/v1?key=synthetic-byok-never-leak-8193",
    ] {
        let r = send(&app, spa(&t, put("custom", input(url)), &session)).await;
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY, "{url}");
        assert!(!String::from_utf8_lossy(&body(r).await).contains(KEY));
    }
    let t = TestState::with_config(|c| {
        c.vault = shelfy_server::ai::vault::KeyVault::new(
            Some(SecretString::from(STANDARD.encode([74; 32]))),
            None,
        )
        .unwrap();
        c.outbound.allow_origins =
            shelfy_server::outbound::OriginAllowlist::parse("https://8.8.8.8").unwrap();
    });
    let session = sign_in(&t.app(), &t).await;
    assert_eq!(
        send(
            &t.app(),
            spa(&t, put("custom", input("https://8.8.8.8/v1")), &session)
        )
        .await
        .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
#[tokio::test]
async fn delete_clears_key_consent_and_routing_and_cancels_calls() {
    let stub = stub(Duration::from_secs(10)).await;
    let t = configured();
    let (id, session) = install(&t, &stub).await;
    consent(&t, &session).await;
    let db = t.state.user_db(&id).await.unwrap();
    db.write(|tx| {
        shelfy_core::repo::settings::update(
            tx,
            &shelfy_core::repo::settings::SettingsChange {
                ai_routing: Some(serde_json::from_value(json!({"chat":"custom"})).unwrap()),
                ..Default::default()
            },
            0,
        )
    })
    .unwrap();
    let state = t.state.clone();
    let user = id.clone();
    let call = tokio::spawn(async move {
        state
            .ai()
            .chat(
                &state,
                Caller::new(&user, false),
                Task::Chat,
                &ChatRequest::new("stub-text", vec![Message::user_text("Private content")]),
                CallHints::new(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while stub.requests().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let req = Request::delete("/api/v1/me/providers/custom")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&t.app(), spa(&t, req, &session)).await.status(),
        StatusCode::NO_CONTENT
    );
    let result = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result,Err(AiServiceError::Call(e))if e.kind()==ErrorKind::Cancelled));
    assert!(
        t.state
            .control()
            .read(|c| provider_keys::get(c, &id, "custom"))
            .unwrap()
            .is_none()
    );
    let settings = db.read(shelfy_core::repo::settings::read).unwrap();
    assert!(settings.ai.providers.is_empty());
    assert!(settings.ai.routing.0.is_empty());
}
#[tokio::test]
async fn editing_invalidates_consent_and_never_echoes_malformed_credentials() {
    let stub = stub(Duration::ZERO).await;
    let t = configured();
    let (id, session) = install(&t, &stub).await;
    consent(&t, &session).await;
    let mut changed = input(stub.openai_base().as_str());
    changed["key"] = json!("changed-synthetic-credential");
    assert_eq!(
        send(&t.app(), spa(&t, put("custom", changed), &session))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let db = t.state.user_db(&id).await.unwrap();
    assert!(
        db.read(shelfy_core::repo::settings::read)
            .unwrap()
            .ai
            .providers[0]
            .consent
            .is_none()
    );
    let mut malformed = input(stub.openai_base().as_str());
    malformed["kind"] = json!(KEY);
    let r = send(&t.app(), spa(&t, put("custom", malformed), &session)).await;
    assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!String::from_utf8_lossy(&body(r).await).contains(KEY));
}
#[tokio::test]
async fn provider_limit_and_reserved_operator() {
    let t = configured();
    let app = t.app();
    let session = sign_in(&app, &t).await;
    for n in 0..8 {
        assert_eq!(
            send(
                &app,
                spa(
                    &t,
                    put(&format!("p{n}"), input("https://8.8.8.8/v1")),
                    &session
                )
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
    }
    for id in ["ninth", "operator", "OPERATOR"] {
        assert_eq!(
            send(
                &app,
                spa(&t, put(id, input("https://8.8.8.8/v1")), &session)
            )
            .await
            .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        send(
            &app,
            spa(&t, put("p0", input("https://8.8.8.8/v1")), &session)
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
}
#[tokio::test]
async fn another_user_cannot_probe_consent_or_delete_owners_provider() {
    let stub = stub(Duration::ZERO).await;
    let t = configured();
    let (id, _) = install(&t, &stub).await;
    let member = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    t.state
        .control()
        .write(|c| {
            shelfy_server::control::users::insert(
                c,
                &shelfy_server::control::users::NewUser {
                    id: member,
                    email: "member@example.test",
                    display_name: None,
                    role: shelfy_server::control::users::Role::Member,
                    quota_bytes: 0,
                },
                0,
            )
        })
        .unwrap();
    let app = t.app();
    let token = support::auth::link_token(&t, "member@example.test");
    let login = send(&app, support::auth::redeem_request(&t, &token)).await;
    let session = support::auth::session_cookie(&login).unwrap();
    assert_eq!(
        response_json(send(&app, with_session(get("/api/v1/me/providers"), &session)).await).await,
        json!([])
    );
    assert_eq!(
        send(
            &app,
            spa(&t, post("/api/v1/me/providers/custom/test"), &session)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let req = Request::post("/api/v1/me/providers/custom/consent")
        .header("content-type", "application/json")
        .body(Body::from(json!({"version":CONSENT_VERSION}).to_string()))
        .unwrap();
    assert_eq!(
        send(&app, spa(&t, req, &session)).await.status(),
        StatusCode::NOT_FOUND
    );
    let req = Request::delete("/api/v1/me/providers/custom")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, spa(&t, req, &session)).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(
        t.state
            .control()
            .read(|c| provider_keys::get(c, member, "custom"))
            .unwrap()
            .is_none()
    );
    assert!(
        t.state
            .control()
            .read(|c| provider_keys::get(c, &id, "custom"))
            .unwrap()
            .is_some()
    );
    assert!(stub.requests().is_empty());
}

#[tokio::test]
async fn cancelling_a_probe_does_not_resurrect_deleted_settings() {
    let stub = stub(Duration::from_secs(10)).await;
    let t = configured();
    let (id, session) = install(&t, &stub).await;
    let app = t.app();
    let test = tokio::spawn({
        let app = app.clone();
        let req = spa(&t, post("/api/v1/me/providers/custom/test"), &session);
        async move { send(&app, req).await }
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while stub.requests().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let req = Request::delete("/api/v1/me/providers/custom")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, spa(&t, req, &session)).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), test)
            .await
            .unwrap()
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert!(
        t.state
            .user_db(&id)
            .await
            .unwrap()
            .read(shelfy_core::repo::settings::read)
            .unwrap()
            .ai
            .providers
            .is_empty()
    );
}
#[tokio::test]
async fn loopback_flag_requires_a_loopback_public_url() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = shelfy_server::config::Config::with_data_dir(
        shelfy_server::config::DataDir::new(dir.path()).unwrap(),
    );
    config.public_url =
        shelfy_server::config::PublicUrl::parse("https://shelfy.example.test").unwrap();
    config.ai_allow_loopback = true;
    assert!(shelfy_server::state::AppState::open(config).is_err());
}
#[tokio::test]
async fn restart_opens_the_same_sealed_key_and_consent() {
    let stub = stub(Duration::ZERO).await;
    let t = configured();
    let (id, session) = install(&t, &stub).await;
    consent(&t, &session).await;
    let mut config = shelfy_server::config::Config::with_data_dir(t.data_dir());
    config.ai_allow_loopback = true;
    config.vault = shelfy_server::ai::vault::KeyVault::new(
        Some(SecretString::from(STANDARD.encode([73; 32]))),
        None,
    )
    .unwrap();
    let reopened = shelfy_server::state::AppState::open(config).unwrap();
    reopened
        .ai()
        .chat(
            &reopened,
            Caller::new(&id, false),
            Task::Chat,
            &ChatRequest::new(
                "stub-text",
                vec![Message::user_text("Synthetic restart probe")],
            ),
            CallHints::new(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn domain_validation_rejects_every_private_dns_answer() {
    use std::net::IpAddr;
    for answers in [
        vec![
            "8.8.8.8".parse::<IpAddr>().unwrap(),
            "10.0.0.1".parse().unwrap(),
        ],
        vec![],
    ] {
        let t = TestState::with_config(|c| {
            c.vault = shelfy_server::ai::vault::KeyVault::new(
                Some(SecretString::from(STANDARD.encode([43; 32]))),
                None,
            )
            .unwrap();
            c.outbound.lookup =
                shelfy_server::outbound::Lookup::fixed([("provider.example.test", answers)]);
        });
        let session = sign_in(&t.app(), &t).await;
        assert_eq!(
            send(
                &t.app(),
                spa(
                    &t,
                    put("custom", input("https://provider.example.test/v1")),
                    &session
                )
            )
            .await
            .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
#[tokio::test]
async fn anthropic_probes_and_versioned_consent() {
    let stub = stub(Duration::ZERO).await;
    let t = configured();
    let id = owner(&t);
    let session = sign_in(&t.app(), &t).await;
    let mut value = input(stub.url().as_str());
    value["kind"] = json!("anthropic");
    assert_eq!(
        send(&t.app(), spa(&t, put("custom", value), &session))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let r = send(
        &t.app(),
        spa(&t, post("/api/v1/me/providers/custom/test"), &session),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let test = response_json(r).await;
    for part in ["models", "text", "vision", "schema"] {
        assert_eq!(test[part]["ok"], true, "{part}: {test}");
    }
    let req = Request::post("/api/v1/me/providers/custom/consent")
        .header("content-type", "application/json")
        .body(Body::from(json!({"version":"obsolete"}).to_string()))
        .unwrap();
    assert_eq!(
        send(&t.app(), spa(&t, req, &session)).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(
        t.state
            .user_db(&id)
            .await
            .unwrap()
            .read(shelfy_core::repo::settings::read)
            .unwrap()
            .ai
            .providers[0]
            .consent
            .is_none()
    );
    consent(&t, &session).await;
}

#[tokio::test]
async fn sensitive_edits_require_recent_auth_and_never_show_whole_short_keys() {
    let t = configured();
    let session = sign_in(&t.app(), &t).await;
    let mut value = input("https://8.8.8.8/v1");
    value["key"] = json!("tiny");
    assert_eq!(
        send(&t.app(), spa(&t, put("custom", value), &session))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let summary = response_json(
        send(
            &t.app(),
            with_session(get("/api/v1/me/providers"), &session),
        )
        .await,
    )
    .await;
    assert_eq!(summary[0]["last4"], "");
    assert!(!summary.to_string().contains("tiny"));
    control_db(&t)
        .execute(
            "UPDATE sessions SET created_at=?1, reauth_at=?1",
            [shelfy_server::ids::now_ms() - 600_000],
        )
        .unwrap();
    t.state.auth().forget_all_sessions();
    let r = send(
        &t.app(),
        spa(&t, put("custom", input("https://8.8.8.8/v1")), &session),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(response_json(r).await["code"], "reauth_required");
}
