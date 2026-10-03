//! The account API through the real middleware stack (plan §2.9 Account,
//! §2.11, §2.13; P1-17): `GET /me` and its capabilities, settings, consent,
//! usage and its job, the session list with remote sign-out, API tokens
//! (minting after re-authentication, scopes, last use, revocation, the
//! `lookup` scope on `POST /posts/lookup`) and per-route access. The device
//! flow is in `device.rs`; the logs are checked in `auth_logs.rs`.

mod support;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use rusqlite::params;
use serde_json::{Value, json};
use shelfy_server::config::Config;
use shelfy_server::error::ErrorCode;
use shelfy_server::events::model::JobState;
use shelfy_server::extension::VERSION_HEADER;
use shelfy_server::ids::now_ms;
use shelfy_server::jobs::{NewJob, kinds, usage};
use shelfy_server::mail::MailConfig;
use shelfy_server::tokens::hash_token;
use support::auth::{
    OWNER_EMAIL, add_member, audit_rows, control_db, from_spa, link_token, make_sessions_stale,
    owner, redeem_request, session_cookie, sign_in, sign_in_as, spa, with_session,
};
use support::library::fixture;
use support::{TestState, body, get, json, problem, send};
use tokio_util::sync::CancellationToken;

const MEMBER_EMAIL: &str = "member@example.test";

/// A state whose sign-in limit leaves room for many sign-ins: every
/// in-process request counts as the same client.
fn roomy(edit: impl FnOnce(&mut Config)) -> TestState {
    TestState::with_config(|config: &mut Config| {
        config.auth.ip_limit.max = 1_000;
        edit(config);
    })
}

/// A request with a JSON body, as the SPA sends it with `cookie`.
fn spa_json(t: &TestState, method: Method, uri: &str, body: &Value, cookie: &str) -> Request<Body> {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    spa(t, request, cookie)
}

/// A bodiless request, as the SPA sends it with `cookie`.
fn spa_empty(t: &TestState, method: Method, uri: &str, cookie: &str) -> Request<Body> {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    spa(t, request, cookie)
}

/// `request` with `Authorization: Bearer token`, and the version header an
/// extension token's requests carry (P2-03, contract C1).
fn bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
        .headers_mut()
        .insert(VERSION_HEADER, "0.2.0".parse().unwrap());
    request
}

/// `GET uri` with `cookie`; checks 200 and returns the JSON body.
async fn read(app: &Router, uri: &str, cookie: &str) -> Value {
    let response = send(app, with_session(get(uri), cookie)).await;
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    json(response).await
}

/// Signs the owner in from a browser with `agent` as its `User-Agent`.
async fn sign_in_with_agent(app: &Router, t: &TestState, agent: &str) -> String {
    owner(t);
    let mut request = redeem_request(t, &link_token(t, OWNER_EMAIL));
    request
        .headers_mut()
        .insert(header::USER_AGENT, agent.parse().unwrap());
    let response = send(app, request).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    session_cookie(&response).unwrap()
}

#[tokio::test]
async fn me_reports_the_capabilities_and_consent() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let me = read(&app, "/api/v1/me", &cookie).await;
    assert_eq!(me["email"], OWNER_EMAIL);
    assert_eq!(me["role"], "owner");
    assert_eq!(
        me["capabilities"],
        json!({
            "admin": true,
            "passkeys": true,
            "emailLink": false,
            "extension": true,
            "ai.tasks": false,
            "capture": false,
            "video.onDemand": false,
        })
    );
    assert_eq!(
        me["consent"],
        json!({
            "disclaimerVersion": null,
            "disclaimerAcceptedAt": null,
            "privacyVersion": null,
            "privacyAcceptedAt": null,
        })
    );

    // A member is no admin; email and passkeys follow the configuration.
    let t = roomy(|config: &mut Config| {
        config.mail = MailConfig::dev_mailbox(&config.data_dir);
        config.public_url =
            shelfy_server::config::PublicUrl::parse("http://127.0.0.1:8080").unwrap();
    });
    let app = t.app();
    owner(&t);
    add_member(&t, MEMBER_EMAIL);
    let member = sign_in_as(&app, &t, MEMBER_EMAIL).await;
    let me = read(&app, "/api/v1/me", &member).await;
    assert_eq!(me["role"], "member");
    let capabilities = &me["capabilities"];
    assert_eq!(capabilities["admin"], false);
    assert_eq!(capabilities["emailLink"], true);
    assert_eq!(
        capabilities["passkeys"], false,
        "an IP address has no RP ID"
    );
}

#[tokio::test]
async fn settings_take_the_allowlisted_keys_only() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let defaults = json!({
        "language": null,
        "archiveAssetTypes": { "thumbnail": true, "image": true, "video": true },
        "aiRouting": {}, "aiConcurrency": 4, "aiSuggestions": true,
        "aiVisionQc": false, "aiAutoAnalyzeWebsites": false, "aiDictationInterim": false,
    });
    assert_eq!(read(&app, "/api/v1/me/settings", &cookie).await, defaults);

    let mut expected = defaults.clone();
    expected["language"] = json!("en");
    let put = |body: Value| spa_json(&t, Method::PUT, "/api/v1/me/settings", &body, &cookie);
    let response = send(&app, put(json!({ "language": "en" }))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["language"], "en");
    let types = json!({ "thumbnail": true, "image": false, "video": false });
    expected["archiveAssetTypes"] = types.clone();
    let response = send(&app, put(json!({ "archiveAssetTypes": types }))).await;
    assert_eq!(
        json(response).await,
        expected,
        "a change keeps the other setting"
    );
    let response = send(&app, put(json!({}))).await;
    assert_eq!(json(response).await["language"], "en", "an empty change");

    for refused in [
        json!({ "theme": "dark" }),
        json!({ "aiProviders": [] }),
        json!({ "aiConcurrency": 0 }),
        json!({ "aiConcurrency": 9 }),
        json!({ "aiRouting": { "unknown": "operator" } }),
        json!({ "aiRouting": { "chat": "" } }),
        json!({ "language": "fr" }),
        json!({ "archiveAssetTypes": { "thumbnail": true } }),
        json!({ "archiveAssetTypes": { "thumbnail": true, "image": true, "video": true, "pdf": true } }),
    ] {
        let response = send(&app, put(refused.clone())).await;
        let problem = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(problem.code, ErrorCode::ValidationFailed, "{refused}");
    }
    assert_eq!(read(&app, "/api/v1/me/settings", &cookie).await, expected);

    let ai = json!({ "aiRouting": { "chat": "operator", "catalog": "byok-1" },
        "aiConcurrency": 8, "aiSuggestions": false, "aiVisionQc": true,
        "aiAutoAnalyzeWebsites": true, "aiDictationInterim": true });
    let response = send(&app, put(ai.clone())).await;
    assert_eq!(response.status(), StatusCode::OK);
    for (key, value) in ai.as_object().unwrap() {
        expected[key] = value.clone();
    }
    assert_eq!(json(response).await, expected);
    assert_eq!(read(&app, "/api/v1/me/settings", &cookie).await, expected);

    // Kept in the user's library, in the form the migration writes.
    let owner_id = owner(&t);
    let stored: String = t
        .state
        .user_db(&owner_id)
        .await
        .unwrap()
        .read(|conn| {
            conn.query_row(
                "SELECT value_json FROM settings WHERE key = 'language'",
                [],
                |row| row.get(0),
            )
            .map_err(shelfy_core::repo::RepoError::from)
        })
        .unwrap();
    assert_eq!(stored, "\"en\"");

    // Each account has its own.
    add_member(&t, MEMBER_EMAIL);
    let member = sign_in_as(&app, &t, MEMBER_EMAIL).await;
    assert_eq!(read(&app, "/api/v1/me/settings", &member).await, defaults);
}

#[tokio::test]
async fn consent_is_stored_with_its_time_and_audited() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let before = now_ms();
    let body = json!({ "disclaimerVersion": "2026-10", "privacyVersion": "1" });
    let response = send(
        &app,
        spa_json(&t, Method::POST, "/api/v1/me/consent", &body, &cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let consent = json(response).await;
    assert_eq!(consent["disclaimerVersion"], "2026-10");
    assert_eq!(consent["privacyVersion"], "1");
    let at = consent["disclaimerAcceptedAt"].as_i64().unwrap();
    assert!(at >= before && at <= now_ms());
    assert_eq!(consent["privacyAcceptedAt"], at);
    assert_eq!(read(&app, "/api/v1/me", &cookie).await["consent"], consent);
    assert_eq!(
        audit_rows(&control_db(&t), "consent.accept"),
        [json!({ "disclaimerVersion": "2026-10", "privacyVersion": "1" })]
    );

    // Malformed versions are refused, field by field, and nothing changes.
    let bad = json!({ "disclaimerVersion": "", "privacyVersion": "v1 <b>" });
    let response = send(
        &app,
        spa_json(&t, Method::POST, "/api/v1/me/consent", &bad, &cookie),
    )
    .await;
    let refused = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    let fields: Vec<&str> = refused.errors.iter().map(|e| e.field.as_str()).collect();
    assert_eq!(fields, ["disclaimerVersion", "privacyVersion"]);
    let missing = json!({ "disclaimerVersion": "2026-10" });
    let response = send(
        &app,
        spa_json(&t, Method::POST, "/api/v1/me/consent", &missing, &cookie),
    )
    .await;
    problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(read(&app, "/api/v1/me", &cookie).await["consent"], consent);
    assert_eq!(audit_rows(&control_db(&t), "consent.accept").len(), 1);
}

#[tokio::test]
async fn usage_is_counted_by_its_job() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let owner_id = owner(&t);
    t.write(&owner_id, |tx| fixture(tx).map(|_| ())).await;
    let media: i64 = t
        .state
        .user_db(&owner_id)
        .await
        .unwrap()
        .read(|conn| {
            conn.query_row("SELECT sum(bytes) FROM media_objects", [], |row| row.get(0))
                .map_err(shelfy_core::repo::RepoError::from)
        })
        .unwrap();
    assert!(media > 0);

    // Never counted: zeros, and the first read starts a count.
    let usage = read(&app, "/api/v1/me/usage", &cookie).await;
    assert_eq!(
        usage,
        json!({ "usedBytes": 0, "mediaBytes": 0, "dbBytes": 0, "quotaBytes": 0, "updatedAt": null })
    );
    let queued: i64 = control_db(&t)
        .query_row(
            "SELECT count(*) FROM jobs WHERE kind = 'usage.recompute' AND state = 'queued'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 1);
    read(&app, "/api/v1/me/usage", &cookie).await;
    let queued: i64 = control_db(&t)
        .query_row(
            "SELECT count(*) FROM jobs WHERE kind = 'usage.recompute'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 1, "one count at a time");

    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let id: i64 = control_db(&t)
        .query_row(
            "SELECT id FROM jobs WHERE kind = 'usage.recompute'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    t.wait_job(&owner_id, id, |job| job.state == JobState::Succeeded)
        .await;
    let usage = read(&app, "/api/v1/me/usage", &cookie).await;
    assert_eq!(usage["mediaBytes"], media);
    let db = usage["dbBytes"].as_i64().unwrap();
    assert!(db >= 4096, "{usage}");
    assert_eq!(usage["usedBytes"], media + db);
    assert_eq!(usage["quotaBytes"], 0, "the owner's quota is unlimited");
    assert!(usage["updatedAt"].as_i64().unwrap() <= now_ms());

    // A user without a library uses nothing, and gets no library.
    let member = add_member(&t, MEMBER_EMAIL);
    let job = usage::enqueue(t.state.jobs(), &member).await.unwrap().job;
    t.wait_job(&member, job.id, |job| job.state == JobState::Succeeded)
        .await;
    let (used, counted): (i64, Option<i64>) = control_db(&t)
        .query_row(
            "SELECT usage_bytes, usage_updated_at FROM users WHERE id = ?1",
            [&member],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(used, 0);
    assert!(counted.is_some());
    assert!(!t.data_dir().library_db(&member).exists());
}

#[test]
fn usage_is_recounted_nightly() {
    let registry = kinds::registry();
    let kind = registry.get(usage::KIND).expect("registered");
    assert!(kind.spec().nightly);
    assert_eq!(kind.spec().per_user, 1);
    let job = NewJob::new("u", usage::KIND);
    assert_eq!(job.kind, "usage.recompute");
}

#[tokio::test]
async fn sessions_are_listed_and_signed_out_from_another_one() {
    let t = roomy(|_| {});
    let app = t.app();
    let laptop = sign_in_with_agent(&app, &t, "Laptop Firefox").await;
    let phone = sign_in_with_agent(&app, &t, "Phone Safari").await;
    let tablet = sign_in_with_agent(&app, &t, "Tablet").await;

    let list = read(&app, "/api/v1/me/sessions", &laptop).await;
    let items = list["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["current"], true, "the current session first");
    assert_eq!(items[0]["userAgent"], "Laptop Firefox");
    assert!(items[1..].iter().all(|s| s["current"] == false));
    for session in &items {
        let id = session["id"].as_str().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        for cookie in [&laptop, &phone, &tablet] {
            assert!(!cookie.contains(id) && !id.contains(cookie.as_str()));
        }
        assert!(session["expiresAt"].as_i64().unwrap() > now_ms());
        assert!(session["createdAt"].as_i64().unwrap() <= now_ms());
        assert!(session["lastSeenAt"].is_i64());
    }
    // The ids are stable: the phone sees the same ones.
    let from_phone = read(&app, "/api/v1/me/sessions", &phone).await;
    let ids = |list: &Value| -> Vec<String> {
        let mut ids: Vec<String> = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(&from_phone), ids(&list));
    let phone_id = from_phone["items"][0]["id"].as_str().unwrap().to_owned();

    // The laptop signs the phone out: it stops working at once.
    let uri = format!("/api/v1/me/sessions/{phone_id}");
    let response = send(&app, spa_empty(&t, Method::DELETE, &uri, &laptop)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        session_cookie(&response).is_none(),
        "the laptop keeps its cookie"
    );
    let response = send(&app, with_session(get("/api/v1/me"), &phone)).await;
    problem(response, StatusCode::UNAUTHORIZED).await;
    let response = send(&app, spa_empty(&t, Method::DELETE, &uri, &laptop)).await;
    problem(response, StatusCode::NOT_FOUND).await;
    assert_eq!(
        read(&app, "/api/v1/me/sessions", &laptop).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Unknown and malformed ids are 404s.
    for id in ["0".repeat(32), "nope".to_owned(), "Z".repeat(32)] {
        let uri = format!("/api/v1/me/sessions/{id}");
        let response = send(&app, spa_empty(&t, Method::DELETE, &uri, &laptop)).await;
        problem(response, StatusCode::NOT_FOUND).await;
    }

    // Every other session: the tablet goes, the laptop stays.
    let response = send(
        &app,
        spa_empty(&t, Method::DELETE, "/api/v1/me/sessions", &laptop),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = send(&app, with_session(get("/api/v1/me"), &tablet)).await;
    problem(response, StatusCode::UNAUTHORIZED).await;
    let list = read(&app, "/api/v1/me/sessions", &laptop).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // Signing out the current session from the list is a sign-out.
    let own = list["items"][0]["id"].as_str().unwrap().to_owned();
    let uri = format!("/api/v1/me/sessions/{own}");
    let response = send(&app, spa_empty(&t, Method::DELETE, &uri, &laptop)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(session_cookie(&response).as_deref(), Some(""), "cleared");
    let response = send(&app, with_session(get("/api/v1/me"), &laptop)).await;
    problem(response, StatusCode::UNAUTHORIZED).await;

    let scopes: Vec<Value> = audit_rows(&control_db(&t), "session.delete")
        .into_iter()
        .map(|meta| meta["scope"].clone())
        .collect();
    assert_eq!(scopes, [json!("remote"), json!("others"), json!("current")]);
}

#[tokio::test]
async fn another_accounts_sessions_and_tokens_are_404s() {
    let t = roomy(|_| {});
    let app = t.app();
    let owner_cookie = sign_in(&app, &t).await;
    add_member(&t, MEMBER_EMAIL);
    let member = sign_in_as(&app, &t, MEMBER_EMAIL).await;

    let owners = read(&app, "/api/v1/me/sessions", &owner_cookie).await;
    let owner_session = owners["items"][0]["id"].as_str().unwrap().to_owned();
    let members = read(&app, "/api/v1/me/sessions", &member).await;
    assert_eq!(
        members["items"].as_array().unwrap().len(),
        1,
        "own sessions only"
    );
    let uri = format!("/api/v1/me/sessions/{owner_session}");
    let response = send(&app, spa_empty(&t, Method::DELETE, &uri, &member)).await;
    problem(response, StatusCode::NOT_FOUND).await;
    assert_eq!(
        send(&app, with_session(get("/api/v1/me"), &owner_cookie))
            .await
            .status(),
        StatusCode::OK
    );

    let created = create_token(&app, &t, &owner_cookie, &json!({ "kind": "shortcut" })).await;
    let id = created["apiToken"]["id"].as_str().unwrap().to_owned();
    assert!(
        read(&app, "/api/v1/me/tokens", &member).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let uri = format!("/api/v1/me/tokens/{id}");
    let response = send(&app, spa_empty(&t, Method::DELETE, &uri, &member)).await;
    problem(response, StatusCode::NOT_FOUND).await;
    assert_eq!(
        read(&app, "/api/v1/me/tokens", &owner_cookie).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// `POST /me/tokens` with `body`; checks 201 and returns the answer.
async fn create_token(app: &Router, t: &TestState, cookie: &str, body: &Value) -> Value {
    let response = send(
        app,
        spa_json(t, Method::POST, "/api/v1/me/tokens", body, cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED, "{body}");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    json(response).await
}

#[tokio::test]
async fn tokens_need_a_recent_sign_in_and_show_once() {
    let t = roomy(|config: &mut Config| {
        config.auth.token_touch_every = Duration::ZERO;
    });
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let owner_id = owner(&t);

    // Six minutes after the sign-in, creating a token needs a re-auth.
    make_sessions_stale(&t);
    let body = json!({ "kind": "extension", "label": " Chrome ", "scopes": ["lookup"] });
    let response = send(
        &app,
        spa_json(&t, Method::POST, "/api/v1/me/tokens", &body, &cookie),
    )
    .await;
    assert_eq!(
        problem(response, StatusCode::FORBIDDEN).await.code,
        ErrorCode::ReauthRequired
    );
    // The list and revocation do not.
    read(&app, "/api/v1/me/tokens", &cookie).await;

    let cookie = sign_in(&app, &t).await;
    let created = create_token(&app, &t, &cookie, &body).await;
    let token = created["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("shx_") && token.len() == 47, "{token}");
    let api_token = &created["apiToken"];
    assert_eq!(api_token["kind"], "extension");
    assert_eq!(api_token["label"], "Chrome");
    assert_eq!(api_token["scopes"], json!(["lookup"]));
    assert_eq!(api_token["lastUsedAt"], Value::Null);
    assert_eq!(api_token["expiresAt"], Value::Null);
    let id = api_token["id"].as_str().unwrap().to_owned();

    // Stored as its hash; the list never shows the value again.
    let (stored_hash, scopes): (Vec<u8>, String) = control_db(&t)
        .query_row(
            "SELECT token_hash, scopes FROM api_tokens WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_hash, hash_token(&token).to_vec());
    assert_eq!(scopes, "lookup");
    let list = read(&app, "/api/v1/me/tokens", &cookie).await;
    assert_eq!(list["items"], json!([api_token.clone()]));
    assert!(!list.to_string().contains(&token[4..]));

    // Defaults: every scope of the kind.
    let all = create_token(&app, &t, &cookie, &json!({ "kind": "extension" })).await;
    assert_eq!(
        all["apiToken"]["scopes"],
        json!(["ingest", "tasks", "uploads", "lookup"])
    );
    let shortcut = create_token(&app, &t, &cookie, &json!({ "kind": "shortcut" })).await;
    assert_eq!(shortcut["apiToken"]["scopes"], json!(["links:create"]));
    assert_eq!(shortcut["apiToken"]["label"], Value::Null);
    let newest_first: Vec<String> = read(&app, "/api/v1/me/tokens", &cookie).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(newest_first.len(), 3);
    assert_eq!(newest_first[2], id);

    // Refused: a migrate token, foreign scopes, no scope, a long label.
    for refused in [
        json!({ "kind": "migrate" }),
        json!({ "kind": "shortcut", "scopes": ["lookup"] }),
        json!({ "kind": "extension", "scopes": ["migrate"] }),
        json!({ "kind": "extension", "scopes": [] }),
        json!({ "kind": "extension", "label": "x".repeat(65) }),
        json!({ "kind": "extension", "scopes": ["admin"] }),
        json!({ "kind": "extension", "owner": "someone" }),
    ] {
        let response = send(
            &app,
            spa_json(&t, Method::POST, "/api/v1/me/tokens", &refused, &cookie),
        )
        .await;
        let problem = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(problem.code, ErrorCode::ValidationFailed, "{refused}");
    }

    let audit = audit_rows(&control_db(&t), "api_token.create");
    assert_eq!(audit.len(), 3);
    assert_eq!(
        audit[0],
        json!({ "id": id, "kind": "extension", "via": "account" })
    );
    let actor: String = control_db(&t)
        .query_row(
            "SELECT actor_user_id FROM audit_log WHERE action = 'api_token.create' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actor, owner_id);
}

#[tokio::test]
async fn a_lookup_token_reads_saved_posts_and_nothing_else() {
    let t = roomy(|config: &mut Config| {
        config.auth.token_touch_every = Duration::ZERO;
    });
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let owner_id = owner(&t);
    t.write(&owner_id, |tx| fixture(tx).map(|_| ())).await;
    let created = create_token(
        &app,
        &t,
        &cookie,
        &json!({ "kind": "extension", "scopes": ["lookup"] }),
    )
    .await;
    let token = created["token"].as_str().unwrap().to_owned();
    let id = created["apiToken"]["id"].as_str().unwrap().to_owned();
    let lookup = json!({ "platform": "instagram", "keys": ["1001", "1003", "9999"] });
    let lookup_request = || {
        Request::post("/api/v1/posts/lookup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(lookup.to_string()))
            .unwrap()
    };

    // The token, from anywhere: no cookie, no CSRF headers.
    let response = send(&app, bearer(lookup_request(), &token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let found = json(response).await;
    let keys: Vec<&str> = found["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["postKey"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["ig_1001", "ig_1003"]);
    // The session too, as the SPA sends it.
    let response = send(&app, spa(&t, lookup_request(), &cookie)).await;
    assert_eq!(json(response).await, found);

    // Its use is recorded.
    let list = read(&app, "/api/v1/me/tokens", &cookie).await;
    let last_used = list["items"][0]["lastUsedAt"].as_i64().unwrap();
    assert!(last_used <= now_ms());

    // Elsewhere it is refused, even beside a valid cookie.
    for request in [
        bearer(get("/api/v1/posts"), &token),
        bearer(get("/api/v1/me"), &token),
        bearer(with_session(get("/api/v1/me/tokens"), &cookie), &token),
        bearer(
            from_spa(
                &t,
                Request::post("/api/v1/migrations/missing-objects")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "objects": [] }).to_string()))
                    .unwrap(),
            ),
            &token,
        ),
    ] {
        let uri = request.uri().to_string();
        let status = send(&app, request).await.status();
        assert!(
            status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
            "{uri}: {status}"
        );
    }

    // A token of another kind lacks the scope: 403.
    let shortcut = create_token(&app, &t, &cookie, &json!({ "kind": "shortcut" })).await;
    let shortcut = shortcut["token"].as_str().unwrap();
    let refused = problem(
        send(&app, bearer(lookup_request(), shortcut)).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(refused.code, ErrorCode::Forbidden);
    assert!(refused.detail.unwrap().contains("lookup"));

    // Revoked: it stops working at once, and leaves the list.
    let uri = format!("/api/v1/me/tokens/{id}");
    let response = send(&app, spa_empty(&t, Method::DELETE, &uri, &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = send(&app, bearer(lookup_request(), &token)).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    problem(response, StatusCode::UNAUTHORIZED).await;
    let response = send(&app, spa_empty(&t, Method::DELETE, &uri, &cookie)).await;
    problem(response, StatusCode::NOT_FOUND).await;
    let ids: Vec<Value> = read(&app, "/api/v1/me/tokens", &cookie).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].clone())
        .collect();
    assert!(!ids.contains(&json!(id)));
    assert_eq!(
        audit_rows(&control_db(&t), "api_token.revoke"),
        [json!({ "id": id, "kind": "extension" })]
    );
}

#[tokio::test]
async fn an_expired_token_stops_working_and_leaves_the_list() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let created = create_token(
        &app,
        &t,
        &cookie,
        &json!({ "kind": "extension", "scopes": ["lookup"] }),
    )
    .await;
    let token = created["token"].as_str().unwrap();
    control_db(&t)
        .execute(
            "UPDATE api_tokens SET expires_at = ?1",
            params![now_ms() - 1],
        )
        .unwrap();
    let request = Request::post("/api/v1/posts/lookup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "platform": "twitter", "keys": ["1"] }).to_string(),
        ))
        .unwrap();
    problem(
        send(&app, bearer(request, token)).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert!(
        read(&app, "/api/v1/me/tokens", &cookie).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn too_many_tokens_are_refused() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    for _ in 0..50 {
        create_token(&app, &t, &cookie, &json!({ "kind": "shortcut" })).await;
    }
    let response = send(
        &app,
        spa_json(
            &t,
            Method::POST,
            "/api/v1/me/tokens",
            &json!({ "kind": "shortcut" }),
            &cookie,
        ),
    )
    .await;
    assert_eq!(
        problem(response, StatusCode::CONFLICT).await.code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn every_account_route_has_its_access() {
    let t = roomy(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = {
        let created = create_token(&app, &t, &cookie, &json!({ "kind": "extension" })).await;
        created["token"].as_str().unwrap().to_owned()
    };
    // A token with every scope, as a stolen extension token would hold.
    let every = format!(
        "shx_{}",
        shelfy_server::tokens::SecretToken::generate().expose()
    );
    control_db(&t)
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES ('01TOKEN0000000000000000000', ?1, 'extension', ?2, \
             'ingest tasks uploads lookup links:create migrate', ?3)",
            params![owner_id, hash_token(&every).as_slice(), now_ms()],
        )
        .unwrap();
    let routes: [(Method, &str, Value); 12] = [
        (Method::GET, "/api/v1/me", Value::Null),
        (Method::GET, "/api/v1/me/settings", Value::Null),
        (
            Method::PUT,
            "/api/v1/me/settings",
            json!({ "language": "it" }),
        ),
        (
            Method::POST,
            "/api/v1/me/consent",
            json!({ "disclaimerVersion": "1", "privacyVersion": "1" }),
        ),
        (Method::GET, "/api/v1/me/usage", Value::Null),
        (Method::GET, "/api/v1/me/sessions", Value::Null),
        (Method::DELETE, "/api/v1/me/sessions", Value::Null),
        (
            Method::DELETE,
            &format!("/api/v1/me/sessions/{}", "a".repeat(32)),
            Value::Null,
        ),
        (Method::GET, "/api/v1/me/tokens", Value::Null),
        (
            Method::POST,
            "/api/v1/me/tokens",
            json!({ "kind": "shortcut" }),
        ),
        (
            Method::DELETE,
            "/api/v1/me/tokens/01NOTATOKEN000000000000000",
            Value::Null,
        ),
        (Method::POST, "/api/v1/me/tokens/pairing-code", Value::Null),
    ];
    for (method, uri, body) in &routes {
        let request = || {
            let mut builder = Request::builder().method(method.clone()).uri(*uri);
            let body = if body.is_null() {
                Body::empty()
            } else {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            };
            from_spa(&t, builder.body(body).unwrap())
        };
        let unknown = shelfy_server::tokens::SecretToken::generate();
        for (what, request) in [
            ("no session", request()),
            (
                "an unknown session",
                with_session(request(), unknown.expose()),
            ),
            ("a token", bearer(request(), &token)),
            (
                "an all-scope token beside a valid cookie",
                bearer(with_session(request(), &cookie), &every),
            ),
        ] {
            let response = send(&app, request).await;
            let problem = problem(response, StatusCode::UNAUTHORIZED).await;
            assert_eq!(
                problem.code,
                ErrorCode::Unauthorized,
                "{method} {uri}: {what}"
            );
        }
    }
    // Still signed in, nothing changed: no consent, the same two tokens.
    let me = read(&app, "/api/v1/me", &cookie).await;
    assert_eq!(me["consent"]["privacyVersion"], Value::Null);
    let body = body(send(&app, with_session(get("/api/v1/me/tokens"), &cookie)).await).await;
    let list: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        read(&app, "/api/v1/me/sessions", &cookie).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
