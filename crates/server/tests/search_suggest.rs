//! P3-15 acceptance against cookie auth, synthetic libraries and provider stub.
mod support;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use shelfy_ai::{
    secrecy::SecretString,
    stub::{Stub, StubConfig, request_key},
};
use shelfy_core::{
    ai::suggest,
    repo::{
        Platform,
        posts::{self, AiLayer, NewPost},
    },
};
use shelfy_server::{
    ai::OperatorConfig, config::Config, outbound::OriginAllowlist, rate_limit::Quota,
};
use std::time::Duration;
use support::auth::{owner, sign_in, spa};
use support::{TestState, post_json, send};
const URI: &str = "/api/v1/search/suggest";
const KEY: &str = "p315-synthetic-key";
async fn stub() -> Stub {
    Stub::start(StubConfig {
        api_key: Some(SecretString::from(KEY)),
        ..StubConfig::default()
    })
    .await
    .unwrap()
}
fn configure(c: &mut Config, stub: &Stub) {
    c.outbound.allow_origins = OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
    c.operator = OperatorConfig {
        url: Some(stub.openai_base()),
        key: Some(SecretString::from(KEY)),
        model: Some("stub-suggest".into()),
        ..OperatorConfig::default()
    };
}
async fn seed(t: &TestState, user: &str) {
    t.write(user,|c| {
        for (key,platform,tags) in [("social",Platform::Instagram,vec!["desk lamp","glass","design","wood","lighting","brass","interior","minimal","music"]),("site",Platform::Web,vec!["website","typography"]),("trash",Platform::Instagram,vec!["trashed"])] {
            let mut p=NewPost::new(key,platform,key,"image",0);
            p.ai=Some(AiLayer {tags:tags.into_iter().map(str::to_owned).collect(),..AiLayer::default()});
            posts::insert(c,&p,0)?;
        }
        c.execute("UPDATE posts SET deleted_at=1 WHERE key='trash'",[])?;
        c.execute("INSERT INTO tag_alias(alias_norm,canonical_norm,canonical_form,status,created_at) VALUES('lamps','desk lamp','Desk Lamp','accepted',0)",[])?;
        Ok(())
    }).await;
}
fn canned(stub: &Stub, q: &str, text: &str) {
    let p = suggest::request(q).unwrap();
    stub.add_canned(request_key(Some(&p.system), [p.user.as_str()]), text);
}
async fn suggest_json(t: &TestState, app: &Router, cookie: &str, q: &str, scope: &str) -> Value {
    let response = send(
        app,
        spa(
            t,
            post_json(URI, json!({"q":q,"scope":scope}).to_string()),
            cookie,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    support::json(response).await
}
#[tokio::test]
async fn cache_hits_normalize_query_survive_own_writes_and_expire_after_24h() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    canned(
        &stub,
        "lamp",
        r#"{"tags":["glass","lamps","fabricated","website","trashed"]}"#,
    );
    let first = suggest_json(&t, &app, &cookie, "lamp", "social").await;
    assert_eq!(first, json!({"tags":["glass","Desk Lamp"]}));
    assert_eq!(stub.requests().len(), 1);
    assert_eq!(
        suggest_json(&t, &app, &cookie, "  LAMP  ", "social").await,
        first
    );
    assert_eq!(stub.requests().len(), 1, "cache hit cannot call provider");
    t.write(&user, |c| {
        c.execute(
            "UPDATE ai_cache SET created_at=created_at-?1",
            [suggest::TTL_MS],
        )?;
        Ok(())
    })
    .await;
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "social").await,
        first
    );
    assert_eq!(stub.requests().len(), 2, "24h is a strict expiry");
}
#[tokio::test]
async fn scopes_live_deletion_and_alias_generation_invalidate_results() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    canned(
        &stub,
        "lamp",
        r#"{"tags":["glass","lamps","website","typography","fabricated","trashed"]}"#,
    );
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "sites").await,
        json!({"tags":["website","typography"]})
    );
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "social").await,
        json!({"tags":["glass","Desk Lamp"]})
    );
    assert_eq!(stub.requests().len(), 2);
    t.write(&user, |c| {
        c.execute("UPDATE tag_alias SET status='proposed'", [])?;
        Ok(())
    })
    .await;
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "social").await,
        json!({"tags":["glass"]})
    );
    t.write(&user, |c| {
        c.execute("UPDATE posts SET deleted_at=2 WHERE key='social'", [])?;
        Ok(())
    })
    .await;
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "social").await,
        json!({"tags":[]})
    );
    assert_eq!(stub.requests().len(), 4);
}
#[tokio::test]
async fn no_route_empty_query_and_offline_answer_200_without_fabricated_tags() {
    let t = TestState::new();
    owner(&t);
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "all").await,
        json!({"tags":[],"reason":"ai_not_configured"})
    );
    assert_eq!(
        suggest_json(&t, &app, &cookie, " ", "all").await,
        json!({"tags":[],"reason":"empty_query"})
    );
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    stub.set_offline(true).await.unwrap();
    let result = suggest_json(&t, &app, &cookie, "lamp", "all").await;
    assert_eq!(result["tags"], json!([]));
    assert!(result["reason"].is_string());
    let count = stub.requests().len();
    let again = suggest_json(&t, &app, &cookie, "lamp", "all").await;
    assert_eq!(again["tags"], json!([]));
    assert!(again["reason"].is_string());
    assert_eq!(stub.requests().len(), count, "offline hold makes no call");
}
#[tokio::test]
async fn cookie_auth_csrf_account_isolation_and_input_validation() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    canned(&stub, "lamp", r#"{"tags":["glass"]}"#);
    suggest_json(&t, &app, &cookie, "lamp", "all").await;
    assert_eq!(
        send(&app, post_json(URI, r#"{"q":"lamp"}"#)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let request = Request::post(URI)
        .header("authorization", "Bearer fake")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"q":"lamp"}"#))
        .unwrap();
    assert_eq!(send(&app, request).await.status(), StatusCode::UNAUTHORIZED);
    let request = Request::post(URI)
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"q":"lamp"}"#))
        .unwrap();
    assert_eq!(send(&app, request).await.status(), StatusCode::FORBIDDEN);
    let bob = t.app_as(support::library::BOB);
    assert_eq!(
        suggest_json(&t, &bob, &cookie, "lamp", "all").await,
        json!({"tags":[],"reason":"ai_not_configured"})
    );
    assert_eq!(
        stub.requests().len(),
        1,
        "another account never uses owner cache or node"
    );
    for bad in [
        json!({"q":"x","scope":"web"}),
        json!({"q":"x".repeat(501)}),
        json!({"q":"x","extra":true}),
        json!({"scope":"all"}),
    ] {
        assert_eq!(
            send(&app, spa(&t, post_json(URI, bad.to_string()), &cookie))
                .await
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
#[tokio::test]
async fn suggest_limit_is_one_per_second_with_burst_two_and_retry_after() {
    let t = TestState::with_config(|c| c.rate_limits.suggest = Some(Quota::per_second(1).burst(2)));
    owner(&t);
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    for _ in 0..2 {
        suggest_json(&t, &app, &cookie, "lamp", "all").await;
    }
    let r = send(&app, spa(&t, post_json(URI, r#"{"q":"lamp"}"#), &cookie)).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers()["retry-after"], "1");
}
#[tokio::test]
async fn byok_consent_revocation_and_missing_route_are_checked_before_cache_hits() {
    let stub = stub().await;
    let t = TestState::with_config(|c| {
        c.ai_allow_loopback = true;
        c.vault = shelfy_server::ai::vault::KeyVault::new(
            Some(SecretString::from(STANDARD.encode([15; 32]))),
            None,
        )
        .unwrap();
    });
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let install=Request::put("/api/v1/me/providers/chips").header("content-type","application/json").body(Body::from(json!({"kind":"openai_compatible","label":"Synthetic chips","baseUrl":stub.openai_base(),"models":{"suggest":"stub-chips"},"key":KEY}).to_string())).unwrap();
    assert_eq!(
        send(&app, spa(&t, install, &cookie)).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "all").await,
        json!({"tags":[],"reason":"ai_consent_required"})
    );
    assert!(stub.requests().is_empty());
    let consent = post_json(
        "/api/v1/me/providers/chips/consent",
        json!({"version":shelfy_server::ai::providers::CONSENT_VERSION}).to_string(),
    );
    assert_eq!(
        send(&app, spa(&t, consent, &cookie)).await.status(),
        StatusCode::NO_CONTENT
    );
    canned(&stub, "lamp", r#"{"tags":["glass","design"]}"#);
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "all").await,
        json!({"tags":["glass","design"]})
    );
    assert_eq!(
        stub.requests().last().unwrap().body.as_ref().unwrap()["model"],
        "stub-chips"
    );
    t.write(&user, |c| {
        let mut ai = shelfy_core::repo::settings::read(c)?.ai;
        ai.providers[0].consent = None;
        shelfy_core::repo::settings::update(
            c,
            &shelfy_core::repo::settings::SettingsChange {
                ai_providers: Some(ai.providers),
                ..Default::default()
            },
            0,
        )?;
        Ok(())
    })
    .await;
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "all").await,
        json!({"tags":[],"reason":"ai_consent_required"})
    );
    let delete = Request::delete("/api/v1/me/providers/chips")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, spa(&t, delete, &cookie)).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        suggest_json(&t, &app, &cookie, "lamp", "all").await,
        json!({"tags":[],"reason":"ai_not_configured"})
    );
    assert_eq!(stub.requests().len(), 1);
}
#[tokio::test]
async fn caps_and_malformed_output_are_safe_empty_results() {
    let stub = stub().await;
    let t = TestState::with_config(|c| configure(c, &stub));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    canned(
        &stub,
        "many",
        r#"{"tags":["glass","desk lamp","design","wood","lighting","brass","interior","minimal","music"]}"#,
    );
    assert_eq!(
        suggest_json(&t, &app, &cookie, "many", "social").await["tags"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    // A single malformed canned answer may be repaired by the SDK's second
    // structured-output attempt. Both attempts must fail for this case.
    stub.inject(shelfy_ai::stub::FaultRule {
        fault: shelfy_ai::stub::Fault::NonConformingJson,
        times: Some(2),
        endpoint: Some(shelfy_ai::stub::Endpoint::Chat),
    });
    let broken = suggest_json(&t, &app, &cookie, "broken", "social").await;
    assert_eq!(broken["tags"], json!([]));
    assert!(broken["reason"].is_string());
    assert_eq!(
        stub.requests().len(),
        3,
        "one valid call, then two invalid structured attempts"
    );
    let cached = t
        .write(&user, |c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM ai_cache WHERE kind='suggest'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await;
    assert_eq!(cached, 1, "only the preceding valid answer can be cached");
}

#[tokio::test]
async fn edits_during_inference_recheck_membership_and_do_not_cache_the_old_generation() {
    let stub = stub().await;
    stub.set_latency(Duration::from_millis(300));
    let t = TestState::with_config(|c| configure(c, &stub));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    canned(&stub, "lamp", r#"{"tags":["glass","website"]}"#);
    let request = spa(&t, post_json(URI, r#"{"q":"lamp","scope":"all"}"#), &cookie);
    let cloned = app.clone();
    let response = tokio::spawn(async move { send(&cloned, request).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while stub.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    t.write(&user, |c| {
        c.execute("UPDATE posts SET deleted_at=2 WHERE key='social'", [])?;
        Ok(())
    })
    .await;
    let response = response.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(support::json(response).await, json!({"tags":["website"]}));
    let cached = t
        .write(&user, |c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM ai_cache WHERE kind='suggest'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await;
    assert_eq!(
        cached, 0,
        "old prompt generation cannot cache a fresh vocabulary"
    );
}

#[tokio::test]
async fn occupied_node_returns_empty_200_before_the_route_timeout() {
    let stub = stub().await;
    stub.set_latency(Duration::from_secs(40));
    let t = TestState::with_config(|c| configure(c, &stub));
    let user = owner(&t);
    seed(&t, &user).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let request = spa(&t, post_json(URI, r#"{"q":"lamp"}"#), &cookie);
    let response = tokio::spawn(async move { send(&app, request).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while stub.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let response = response.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        support::json(response).await,
        json!({"tags":[],"reason":"provider_unavailable"})
    );
    assert!(started.elapsed() <= Duration::from_secs(21));
    tokio::time::resume();
}
