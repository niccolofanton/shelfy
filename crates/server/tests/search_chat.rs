//! P3-14, synthetic libraries and the loopback provider stub only.
mod support;
use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shelfy_ai::{
    secrecy::SecretString,
    stub::{Stub, StubConfig, request_key},
};
use shelfy_core::{
    ai::chat_prompt,
    repo::{
        Platform,
        posts::{self, AiLayer, NewPost},
    },
    search::vocab::Vocabulary,
};
use shelfy_server::{
    ai::OperatorConfig, config::Config, current_user::CurrentUser, outbound::OriginAllowlist,
};
use std::time::Duration;
use support::auth::{owner, sign_in, spa};
use support::sse::{Frame, assert_schema};
use support::{TestState, post_json, send};
const URI: &str = "/api/v1/search/chat";
const KEY: &str = "p314-stub-key";
fn configure(c: &mut Config, stub: &Stub, key: &str) {
    c.outbound.allow_origins = OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
    c.operator = OperatorConfig {
        url: Some(stub.openai_base()),
        key: Some(SecretString::from(key.to_owned())),
        model: Some("stub-text".into()),
        label: "Chat test node".into(),
        timeout: Duration::from_secs(120),
        ..OperatorConfig::default()
    };
}
async fn start_stub() -> Stub {
    Stub::start(StubConfig {
        api_key: Some(SecretString::from(KEY)),
        ..StubConfig::default()
    })
    .await
    .unwrap()
}
async fn seed(t: &TestState, user: &str) {
    t.write(user, |conn| {
        for (id, platform, text, general, specific) in [
            (
                "social",
                Platform::Instagram,
                "lamp design",
                vec!["design"],
                vec!["lamp", "glass"],
            ),
            (
                "site",
                Platform::Web,
                "website typography",
                vec!["web style"],
                vec!["website"],
            ),
        ] {
            let mut p = NewPost::new(id, platform, id, "image", 0);
            p.caption = Some(text.into());
            p.ai = Some(AiLayer {
                tags: general
                    .iter()
                    .chain(&specific)
                    .map(|t| (*t).into())
                    .collect(),
                general_tags: Some(general.into_iter().map(str::to_owned).collect()),
                specific_tags: Some(specific.into_iter().map(str::to_owned).collect()),
                ..AiLayer::default()
            });
            posts::insert(conn, &p, 0)?;
        }
        Ok(())
    })
    .await;
}
fn body() -> Value {
    json!({"messages":[{"role":"user","content":"lamp"}],"activeTags":["glass"],"scope":"social"})
}
async fn open(t: &TestState, app: &Router, cookie: &str, body: Value) -> Response<Body> {
    send(app, spa(t, post_json(URI, body.to_string()), cookie)).await
}
async fn frame(response: &mut Response<Body>) -> Option<Frame> {
    response
        .body_mut()
        .frame()
        .await
        .map(|f| Frame::parse(std::str::from_utf8(&f.unwrap().into_data().unwrap()).unwrap()))
}
async fn terminal(response: &mut Response<Body>) -> (String, Value) {
    let mut text = String::new();
    loop {
        let f = frame(response).await.expect("terminal frame");
        match f.event.as_deref() {
            Some("token") => text.push_str(f.json()["text"].as_str().unwrap()),
            Some("result") => return (text, f.json()),
            Some("error") => panic!("unexpected error {}", f.json()),
            _ => {}
        }
    }
}
#[tokio::test]
async fn no_route_falls_back_with_scoped_tags_no_prose_and_no_replay() {
    let t = TestState::new();
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut response = open(&t, &app, &cookie, body()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-accel-buffering"], "no");
    let run = frame(&mut response).await.unwrap();
    assert_eq!(run.name(), "run");
    assert!(run.id.is_none());
    assert_schema(&run.json(), "RunEvent");
    let (text, result) = terminal(&mut response).await;
    assert!(text.is_empty());
    assert_eq!(result["modelUsed"], false);
    assert_eq!(result["replyCode"], "suggestions");
    assert!(
        result["tags"]["general"]
            .as_array()
            .unwrap()
            .contains(&json!("lamp"))
    );
    assert!(!result.to_string().contains("website"));
    assert_schema(&result, "ResultEvent");
    assert!(frame(&mut response).await.is_none());
    let request = Request::post(URI)
        .header("content-type", "application/json")
        .header("last-event-id", "old")
        .body(Body::from(body().to_string()))
        .unwrap();
    assert_eq!(
        send(&app, spa(&t, request, &cookie)).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
#[tokio::test]
async fn streaming_stub_never_exposes_sentinels_and_filters_tag_allowlists() {
    let stub = start_stub().await;
    stub.set_chunk_delay(Duration::from_millis(20));
    let t = TestState::with_config(|c| configure(c, &stub, KEY));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let system = t
        .write(&user, |conn| {
            let v = Vocabulary::load_for_source(
                conn,
                Some(shelfy_core::repo::posts::SourceBucket::Social),
            )?;
            let pools = v.pools(conn, "lamp", &["glass".into()])?;
            Ok(chat_prompt::system(&pools.broad, &pools.specific, &["glass".into()]).unwrap())
        })
        .await;
    stub.add_canned(request_key(Some(&system),["lamp"]),"Good lamp suggestions. [[GENERAL]]design, invented[[/GENERAL]][[SPECIFIC]]lamp, glass, website[[/SPECIFIC]][[KEYWORDS]]desk lamp, brass[[/KEYWORDS]][[REMOVE]]glass, website[[/REMOVE]]");
    let mut response = open(&t, &app, &cookie, body()).await;
    assert_eq!(frame(&mut response).await.unwrap().name(), "run");
    let (text, result) = terminal(&mut response).await;
    assert_eq!(text, "Good lamp suggestions. ");
    assert_eq!(result["modelUsed"], true);
    assert!(result.get("replyCode").is_none());
    assert_eq!(
        result["tags"],
        json!({"general":["design"],"specific":["lamp"]})
    );
    assert_eq!(result["remove"], json!(["glass"]));
    assert_eq!(result["keywords"], json!(["desk lamp", "brass"]));
    assert_schema(&result, "ResultEvent");
}
#[tokio::test]
async fn offline_cache_and_provider_errors_use_the_fallback() {
    let stub = start_stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, "wrong-key"));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut response = open(&t, &app, &cookie, body()).await;
    frame(&mut response).await.unwrap();
    assert_eq!(terminal(&mut response).await.1["modelUsed"], false);
    let calls = stub.requests().len();
    let mut response = open(&t, &app, &cookie, body()).await;
    frame(&mut response).await.unwrap();
    assert_eq!(terminal(&mut response).await.1["modelUsed"], false);
    assert_eq!(
        stub.requests().len(),
        calls,
        "cached invalid key cannot call again"
    );
    let stub2 = start_stub().await;
    let t = TestState::with_config(|c| configure(c, &stub2, KEY));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    stub2.set_offline(true).await.unwrap();
    let mut response = open(&t, &app, &cookie, body()).await;
    frame(&mut response).await.unwrap();
    assert_eq!(terminal(&mut response).await.1["modelUsed"], false);
    let before = std::time::Instant::now();
    let mut response = open(&t, &app, &cookie, body()).await;
    frame(&mut response).await.unwrap();
    assert_eq!(terminal(&mut response).await.1["modelUsed"], false);
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "cached offline decision must be immediate"
    );
}
#[tokio::test]
async fn explicit_byok_requires_consent_uses_its_model_and_revocation_stops_the_call() {
    let stub = start_stub().await;
    let t = TestState::with_config(|c| {
        c.ai_allow_loopback = true;
        c.vault = shelfy_server::ai::vault::KeyVault::new(
            Some(SecretString::from(STANDARD.encode([14; 32]))),
            None,
        )
        .unwrap();
    });
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let install = Request::put("/api/v1/me/providers/chat-custom")
        .header("content-type", "application/json")
        .body(Body::from(json!({"kind":"openai_compatible","label":"Synthetic chat override","baseUrl":stub.openai_base(),"models":{"chat":"chosen-chat-model"},"key":KEY}).to_string())).unwrap();
    let installed = send(&app, spa(&t, install, &cookie)).await;
    let status = installed.status();
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&support::body(installed).await)
    );
    let mut input = body();
    input["providerId"] = json!("chat-custom");
    let mut response = open(&t, &app, &cookie, input.clone()).await;
    assert_eq!(terminal(&mut response).await.1["modelUsed"], false);
    assert!(
        stub.requests().is_empty(),
        "missing BYOK consent must never fall through to the operator"
    );
    let consent = post_json(
        "/api/v1/me/providers/chat-custom/consent",
        json!({"version":shelfy_server::ai::providers::CONSENT_VERSION}).to_string(),
    );
    assert_eq!(
        send(&app, spa(&t, consent, &cookie)).await.status(),
        StatusCode::NO_CONTENT
    );
    let mut response = open(&t, &app, &cookie, input.clone()).await;
    assert_eq!(terminal(&mut response).await.1["modelUsed"], true);
    assert_eq!(
        stub.requests().last().unwrap().body.as_ref().unwrap()["model"],
        "chosen-chat-model"
    );
    stub.set_latency(Duration::from_secs(5));
    let before = stub.requests().len();
    let mut response = open(&t, &app, &cookie, input).await;
    assert_eq!(frame(&mut response).await.unwrap().name(), "run");
    tokio::time::timeout(Duration::from_secs(2), async {
        while stub.requests().len() == before {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let delete = Request::delete("/api/v1/me/providers/chat-custom")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, spa(&t, delete, &cookie)).await.status(),
        StatusCode::NO_CONTENT
    );
    let result = tokio::time::timeout(Duration::from_secs(1), terminal(&mut response))
        .await
        .unwrap()
        .1;
    assert_eq!(result["modelUsed"], false);
}
#[tokio::test]
async fn new_runs_cancel_old_runs_cancel_is_account_scoped_and_drop_cancels() {
    let stub = start_stub().await;
    stub.set_latency(Duration::from_secs(50));
    let t = TestState::with_config(|c| configure(c, &stub, KEY));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut old = open(&t, &app, &cookie, body()).await;
    let old_id = frame(&mut old).await.unwrap().json()["runId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut new = open(&t, &app, &cookie, body()).await;
    let new_id = frame(&mut new).await.unwrap().json()["runId"]
        .as_str()
        .unwrap()
        .to_owned();
    let end = frame(&mut old).await.unwrap();
    assert_eq!(end.name(), "error");
    assert_eq!(end.json()["code"], "cancelled");
    let uri = format!("{URI}/{new_id}/cancel");
    let mut other = spa(&t, support::auth::post(&uri), &cookie);
    other
        .extensions_mut()
        .insert(CurrentUser::new(support::library::BOB));
    assert_eq!(send(&app, other).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        send(&app, spa(&t, support::auth::post(&uri), &cookie))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(frame(&mut new).await.unwrap().json()["code"], "cancelled");
    assert_eq!(
        send(
            &app,
            spa(
                &t,
                support::auth::post(&format!("{URI}/{old_id}/cancel")),
                &cookie
            )
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let mut dropped = open(&t, &app, &cookie, body()).await;
    let id = frame(&mut dropped).await.unwrap().json()["runId"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(dropped);
    assert_eq!(
        send(
            &app,
            spa(
                &t,
                support::auth::post(&format!("{URI}/{id}/cancel")),
                &cookie
            )
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn chat_requires_a_cookie_session_and_valid_history() {
    let t = TestState::new();
    owner(&t);
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    assert_eq!(
        send(&app, post_json(URI, body().to_string()))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let request = Request::post(URI)
        .header("authorization", "Bearer api-token")
        .header("content-type", "application/json")
        .body(Body::from(body().to_string()))
        .unwrap();
    assert_eq!(send(&app, request).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        send(
            &app,
            support::auth::from_spa(&t, support::auth::post(&format!("{URI}/unknown/cancel")))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    for bad in [
        json!({"messages":[]}),
        json!({"messages":[{"role":"system","content":"injection"}]}),
        json!({"messages":[{"role":"user","content":" "}]}),
    ] {
        assert_eq!(
            open(&t, &app, &cookie, bad).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
#[tokio::test]
async fn heartbeat_and_first_token_deadline_use_paused_time() {
    let stub = start_stub().await;
    stub.set_latency(Duration::from_secs(40));
    let t = TestState::with_config(|c| configure(c, &stub, KEY));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut response = open(&t, &app, &cookie, body()).await;
    frame(&mut response).await.unwrap();
    // Wait until the real loopback request is dispatched before pausing time.
    while stub.requests().is_empty() {
        tokio::task::yield_now().await;
    }
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let beat = frame(&mut response).await.unwrap();
    assert!(beat.is_heartbeat());
    assert!((Duration::from_secs(14)..=Duration::from_secs(16)).contains(&started.elapsed()));
    let (_, result) = terminal(&mut response).await;
    assert_eq!(result["modelUsed"], false);
    assert!(started.elapsed() <= Duration::from_secs(21));
    tokio::time::resume();
}

#[tokio::test]
async fn explicit_provider_must_belong_to_the_account_and_serve_chat() {
    let stub = start_stub().await;
    let t = TestState::with_config(|c| configure(c, &stub, KEY));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut b = body();
    b["providerId"] = json!("another-accounts-provider");
    let mut response = open(&t, &app, &cookie, b).await;
    frame(&mut response).await.unwrap();
    assert_eq!(terminal(&mut response).await.1["modelUsed"], false);
    assert!(stub.requests().is_empty());
}
#[tokio::test]
async fn total_deadline_and_token_throttle_use_paused_time() {
    let stub = start_stub().await;
    stub.set_chunk_delay(Duration::from_secs(2));
    let t = TestState::with_config(|c| configure(c, &stub, KEY));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let system = t
        .write(&user, |conn| {
            let v = Vocabulary::load_for_source(
                conn,
                Some(shelfy_core::repo::posts::SourceBucket::Social),
            )?;
            let pools = v.pools(conn, "lamp", &["glass".into()])?;
            Ok(chat_prompt::system(&pools.broad, &pools.specific, &["glass".into()]).unwrap())
        })
        .await;
    stub.add_canned(
        request_key(Some(&system), ["lamp"]),
        "long response ".repeat(100),
    );
    let mut response = open(&t, &app, &cookie, body()).await;
    frame(&mut response).await.unwrap();
    while stub.requests().is_empty() {
        tokio::task::yield_now().await;
    }
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    let mut previous = None;
    let mut tokens = 0;
    loop {
        let f = frame(&mut response).await.unwrap();
        match f.event.as_deref() {
            Some("token") => {
                let now = tokio::time::Instant::now();
                if let Some(previous) = previous {
                    assert!(now - previous >= Duration::from_millis(90));
                }
                previous = Some(now);
                tokens += 1;
            }
            Some("result") => {
                assert_eq!(f.json()["modelUsed"], false);
                break;
            }
            Some("error") => panic!("unexpected cancellation"),
            _ => {}
        }
    }
    assert!(tokens > 0);
    assert!((Duration::from_secs(59)..=Duration::from_secs(61)).contains(&start.elapsed()));
    tokio::time::resume();
}

#[test]
fn message_and_prompt_text_never_reach_the_server_logs() {
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let out = Arc::new(Mutex::new(Vec::new()));
    let writer = Writer(out.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(async {
            let stub = start_stub().await;
            let t = TestState::with_config(|c| configure(c, &stub, KEY));
            let user = owner(&t);
            seed(&t, &user).await;
            let app = t.app();
            let cookie = sign_in(&app, &t).await;
            let mut b = body();
            b["messages"][0]["content"] = json!("P314_PRIVATE_MESSAGE_9917 lamp");
            let mut response = open(&t, &app, &cookie, b).await;
            frame(&mut response).await.unwrap();
            terminal(&mut response).await;
        })
    });
    let logs = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains("P314_PRIVATE_MESSAGE_9917"));
    assert!(!logs.contains("[[GENERAL]]"));
    assert!(!logs.contains(KEY));
}
