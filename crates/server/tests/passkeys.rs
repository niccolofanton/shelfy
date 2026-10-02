//! Owner passkeys and re-authentication through the real middleware stack
//! (plan §2.11, P1-13), with a software passkey standing in for a browser
//! and its authenticator (`support::passkey`): registration of a
//! discoverable credential, username-less sign-in, re-authentication by
//! passkey and by link, the options sent to the browser, the relying party
//! taken from the public URL, single-use and expiring ceremonies, cloned
//! authenticators, user verification, the user handle, audit rows and
//! per-route access. The logs are checked in `auth_logs.rs`.

mod support;

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::params;
use serde_json::{Value, json};
use shelfy_server::admin::login_link::{LinkPurpose, create_link};
use shelfy_server::config::{Config, PublicUrl};
use shelfy_server::error::ErrorCode;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::mail::MailConfig;
use shelfy_server::routes;
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::{
    OWNER_EMAIL, add_member, audit_rows, control_db, from_spa, mailbox, make_sessions_stale, owner,
    post, redeem_request, session_cookie, sign_in, sign_in_as, spa, with_session,
};
use support::passkey::{
    SoftPasskey, finish_sign_in_request, post_body, reauth_with, register, sign_in_with,
    start_registration, start_sign_in,
};
use support::{TestState, get, json, problem, send};

const MEMBER_EMAIL: &str = "member@example.test";

/// A state whose sign-in rate limit leaves room for many ceremonies: every
/// in-process request counts as the same client. `edit` adjusts the rest.
fn roomy(edit: impl FnOnce(&mut Config)) -> TestState {
    TestState::with_config(|config: &mut Config| {
        config.auth.ip_limit.max = 1_000;
        edit(config);
    })
}

async fn me(app: &Router, cookie: &str) -> StatusCode {
    send(app, with_session(get("/api/v1/me"), cookie))
        .await
        .status()
}

async fn passkeys(app: &Router, cookie: &str) -> Vec<Value> {
    let response = send(app, with_session(get("/api/v1/me/passkeys"), cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    json(response).await["items"].as_array().unwrap().clone()
}

fn delete(id: &Value) -> Request<Body> {
    Request::delete(format!("/api/v1/me/passkeys/{id}"))
        .body(Body::empty())
        .unwrap()
}

fn decoded_len(value: &Value) -> usize {
    URL_SAFE_NO_PAD
        .decode(value.as_str().expect("a base64url string"))
        .expect("base64url")
        .len()
}

/// Checks `instance` against the OpenAPI schema `name`.
fn assert_schema(name: &str, instance: &Value) {
    let doc = serde_json::to_value(routes::openapi()).unwrap();
    let root = json!({
        "$ref": format!("#/components/schemas/{name}"),
        "components": doc["components"],
    });
    let validator = jsonschema::validator_for(&root).expect("the schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|err| format!("{err} at {}", err.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{name}: {errors:?}\n{instance:#}");
}

/// Sends `request` and checks it fails with 400 and `code`; returns the
/// problem's detail.
async fn refused(app: &Router, request: Request<Body>, code: ErrorCode) -> Option<String> {
    let problem = problem(send(app, request).await, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem.code, code);
    problem.detail
}

/// A re-authentication link for `email`; returns its token.
fn reauth_link_token(t: &TestState, email: &str) -> String {
    let link = create_link(
        &t.data_dir(),
        &t.state.config().public_url,
        email,
        LinkPurpose::Reauth,
        Duration::from_secs(15 * 60),
    )
    .unwrap();
    let url = link.url.expose();
    let (page, token) = url.split_once('#').unwrap();
    assert_eq!(
        page,
        format!("{}/login/reauth", t.state.config().public_url)
    );
    token.to_owned()
}

fn reauth_finish(t: &TestState, cookie: &str, body: &Value) -> Request<Body> {
    spa(t, post_body("/api/v1/auth/reauth/finish", body), cookie)
}

fn reauth_start(t: &TestState, cookie: &str, method: &str) -> Request<Body> {
    spa(
        t,
        post_body("/api/v1/auth/reauth/start", &json!({ "method": method })),
        cookie,
    )
}

#[tokio::test]
async fn the_owner_registers_a_passkey_and_signs_in_without_a_username() {
    let t = roomy(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    assert_eq!(
        json(send(&app, get("/api/v1/auth/methods")).await).await,
        json!({ "emailLink": false, "passkeys": true })
    );

    // The way in for a first passkey: a sign-in link, then Settings.
    let cookie = sign_in(&app, &t).await;
    assert!(passkeys(&app, &cookie).await.is_empty());
    let mut laptop = SoftPasskey::for_app(&t);
    let created = register(&app, &t, &cookie, &mut laptop, Some("  MacBook  ")).await;
    assert!(created["id"].as_i64().unwrap() > 0);
    assert_eq!(created["label"], "MacBook", "trimmed");
    assert!(created["createdAt"].as_i64().unwrap() > 0);
    assert_eq!(created["lastUsedAt"], Value::Null);
    assert_eq!(
        passkeys(&app, &cookie).await,
        std::slice::from_ref(&created)
    );
    assert_eq!(laptop.credentials(), 1, "a discoverable credential");

    // Signed out, the passkey alone signs back in: no username, no email.
    let response = send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(me(&app, &cookie).await, StatusCode::UNAUTHORIZED);
    let fresh = sign_in_with(&app, &t, &mut laptop).await;
    let profile = json(send(&app, with_session(get("/api/v1/me"), &fresh)).await).await;
    assert_eq!(profile["id"], owner_id.as_str());
    let listed = passkeys(&app, &fresh).await;
    assert!(listed[0]["lastUsedAt"].as_i64().unwrap() >= created["createdAt"].as_i64().unwrap());

    // A passkey sign-in is a recent sign-in, and it is audited.
    let response = send(&app, spa(&t, post("/api/v1/me/passkeys/start"), &fresh)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let conn = control_db(&t);
    assert_eq!(
        audit_rows(&conn, "passkey.create"),
        [json!({ "id": created["id"] })]
    );
    assert_eq!(
        audit_rows(&conn, "session.create").last().unwrap(),
        &json!({ "method": "passkey", "rotated": false })
    );
    assert_eq!(
        audit_rows(&conn, "session.delete"),
        [json!({ "scope": "current", "count": 1 })],
        "the sign-out"
    );
    // The row keeps the credential's state, never a secret of the browser.
    let (label, state): (String, String) = conn
        .query_row("SELECT label, passkey_json FROM passkeys", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(label, "MacBook");
    assert!(state.contains("\"counter\":1"), "{state}");
}

#[tokio::test]
async fn the_options_ask_for_a_discoverable_verified_passkey() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;

    let start = start_registration(&app, &t, &cookie).await;
    assert_eq!(start["ceremonyId"].as_str().unwrap().len(), 43);
    let options = &start["publicKey"];
    assert_eq!(
        options["rp"],
        json!({ "id": "localhost", "name": "Shelfy" })
    );
    assert_eq!(decoded_len(&options["user"]["id"]), 16, "an opaque handle");
    assert_eq!(options["user"]["name"], OWNER_EMAIL);
    assert_eq!(options["user"]["displayName"], OWNER_EMAIL);
    assert_eq!(decoded_len(&options["challenge"]), 32);
    let algorithms: Vec<i64> = options["pubKeyCredParams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|param| param["alg"].as_i64().unwrap())
        .collect();
    assert_eq!(algorithms, [-7, -257], "ES256, then RS256");
    assert_eq!(options["timeout"], 300_000);
    assert_eq!(options["attestation"], "none");
    assert_eq!(
        options["authenticatorSelection"],
        json!({
            "residentKey": "required",
            "requireResidentKey": true,
            "userVerification": "required",
        })
    );
    assert_eq!(options.get("excludeCredentials"), None, "no passkey yet");
    assert_eq!(options["extensions"]["credProps"], true);
    assert_schema("PasskeyRegistrationStart", &start);

    let mut device = SoftPasskey::for_app(&t);
    let credential = device.create(options).await;
    assert_schema("RegistrationResponseJSON", &credential);
    let body = json!({ "ceremonyId": start["ceremonyId"], "credential": credential });
    let response = send(
        &app,
        spa(&t, post_body("/api/v1/me/passkeys", &body), &cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = json(response).await;
    assert_schema("Passkey", &created);

    // The next registration excludes the passkey: the device holding it
    // creates no second one.
    let start = start_registration(&app, &t, &cookie).await;
    let excluded = start["publicKey"]["excludeCredentials"].as_array().unwrap();
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0]["id"], credential["rawId"]);
    assert!(device.try_create(&start["publicKey"]).await.is_err());
    assert_eq!(device.credentials(), 1);

    // Sign-in: no account named, the browser offers what it holds.
    let start = start_sign_in(&app, &t).await;
    assert_eq!(
        start.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["ceremonyId", "publicKey"],
        "no mediation: the page picks it"
    );
    let options = &start["publicKey"];
    assert_eq!(options["rpId"], "localhost");
    assert_eq!(options["allowCredentials"], json!([]));
    assert_eq!(options["userVerification"], "required");
    assert_eq!(decoded_len(&options["challenge"]), 32);
    assert_eq!(options["timeout"], 300_000);
    assert_schema("PasskeyAssertionStart", &start);
    let assertion = device.get(options).await;
    assert_schema("AuthenticationResponseJSON", &assertion);
    let response = send(
        &app,
        finish_sign_in_request(&t, &start["ceremonyId"], &assertion),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(session_cookie(&response).is_some());
}

#[tokio::test]
async fn the_relying_party_follows_the_public_url() {
    let t = roomy(|config| {
        config.public_url = PublicUrl::parse("https://refs.example.test").unwrap();
    });
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let start = start_registration(&app, &t, &cookie).await;
    assert_eq!(start["publicKey"]["rp"]["id"], "refs.example.test");
    let mut device = SoftPasskey::on("https://refs.example.test");
    register(&app, &t, &cookie, &mut device, None).await;
    let start = start_sign_in(&app, &t).await;
    assert_eq!(start["publicKey"]["rpId"], "refs.example.test");
    sign_in_with(&app, &t, &mut device).await;

    // The same authenticator on another origin of the host (another port):
    // the browser signs for the RP ID, the server refuses the origin.
    let mut elsewhere = device.at("https://refs.example.test:8443");
    let start = start_sign_in(&app, &t).await;
    let assertion = elsewhere.get(&start["publicKey"]).await;
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_eq!(detail.as_deref(), Some("origin_mismatch"));
}

#[tokio::test]
async fn a_public_url_without_a_secure_domain_turns_passkeys_off() {
    for url in ["http://127.0.0.1:8080", "http://refs.example.test"] {
        let t = roomy(|config| {
            config.public_url = PublicUrl::parse(url).unwrap();
        });
        let app = t.app();
        let methods = json(send(&app, get("/api/v1/auth/methods")).await).await;
        assert_eq!(methods["passkeys"], false, "{url}");
        let request = from_spa(&t, post("/api/v1/auth/passkeys/login/start"));
        let response = send(&app, request).await;
        assert_eq!(
            problem(response, StatusCode::NOT_FOUND).await.code,
            ErrorCode::NotFound,
            "{url}"
        );
    }
}

#[tokio::test]
async fn ceremonies_are_used_once_and_expire() {
    let t = roomy(|config| {
        config.auth.passkey_ceremony_ttl = Duration::from_secs(1);
    });
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);
    register(&app, &t, &cookie, &mut device, None).await;

    // Replayed: the ceremony is gone after its first finish.
    let start = start_sign_in(&app, &t).await;
    let assertion = device.get(&start["publicKey"]).await;
    let finish = || finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    assert_eq!(send(&app, finish()).await.status(), StatusCode::NO_CONTENT);
    refused(&app, finish(), ErrorCode::ChallengeExpired).await;
    // The same answer for a new ceremony: signed over another challenge.
    let other = start_sign_in(&app, &t).await;
    let request = finish_sign_in_request(&t, &other["ceremonyId"], &assertion);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_eq!(detail.as_deref(), Some("challenge_mismatch"));

    // A refused answer uses the ceremony up too.
    let start = start_sign_in(&app, &t).await;
    let assertion = device.get(&start["publicKey"]).await;
    let mut tampered = assertion.clone();
    tampered["response"]["signature"] = json!(URL_SAFE_NO_PAD.encode([7_u8; 70]));
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &tampered);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_ne!(detail.as_deref(), Some("challenge_mismatch"));
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    refused(&app, request, ErrorCode::ChallengeExpired).await;

    // Unknown, malformed and expired ceremonies.
    let unknown = json!(SecretToken::generate().expose());
    for id in [unknown, json!("short"), json!("")] {
        let start = start_sign_in(&app, &t).await;
        let assertion = device.get(&start["publicKey"]).await;
        let request = finish_sign_in_request(&t, &id, &assertion);
        refused(&app, request, ErrorCode::ChallengeExpired).await;
    }
    let start = start_sign_in(&app, &t).await;
    let assertion = device.get(&start["publicKey"]).await;
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    refused(&app, request, ErrorCode::ChallengeExpired).await;

    // A ceremony of one kind does not finish another.
    let cookie = sign_in(&app, &t).await;
    let sign_in_start = start_sign_in(&app, &t).await;
    let mut second = SoftPasskey::for_app(&t);
    let registration = start_registration(&app, &t, &cookie).await;
    let credential = second.create(&registration["publicKey"]).await;
    let body = json!({ "ceremonyId": sign_in_start["ceremonyId"], "credential": credential });
    let request = spa(&t, post_body("/api/v1/me/passkeys", &body), &cookie);
    refused(&app, request, ErrorCode::ChallengeExpired).await;
}

#[tokio::test]
async fn ceremonies_finish_in_the_session_that_started_them() {
    let t = roomy(|_| {});
    let app = t.app();
    let laptop_session = sign_in(&app, &t).await;
    let phone_session = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);

    // A registration started in one session cannot finish in another.
    let start = start_registration(&app, &t, &laptop_session).await;
    let credential = device.create(&start["publicKey"]).await;
    let body = json!({ "ceremonyId": start["ceremonyId"], "credential": credential });
    let request = spa(&t, post_body("/api/v1/me/passkeys", &body), &phone_session);
    refused(&app, request, ErrorCode::ChallengeExpired).await;
    let request = spa(&t, post_body("/api/v1/me/passkeys", &body), &laptop_session);
    refused(&app, request, ErrorCode::ChallengeExpired).await;
    assert!(passkeys(&app, &laptop_session).await.is_empty());

    // Nor in another account's.
    add_member(&t, MEMBER_EMAIL);
    let member = sign_in_as(&app, &t, MEMBER_EMAIL).await;
    let start = start_registration(&app, &t, &laptop_session).await;
    let mut other = SoftPasskey::for_app(&t);
    let credential = other.create(&start["publicKey"]).await;
    let body = json!({ "ceremonyId": start["ceremonyId"], "credential": credential });
    let request = spa(&t, post_body("/api/v1/me/passkeys", &body), &member);
    refused(&app, request, ErrorCode::ChallengeExpired).await;

    // Re-authentication likewise.
    register(&app, &t, &laptop_session, &mut device, None).await;
    let response = send(&app, reauth_start(&t, &laptop_session, "passkey")).await;
    let start = json(response).await;
    let assertion = device.get(&start["publicKey"]).await;
    let body =
        json!({ "method": "passkey", "ceremonyId": start["ceremonyId"], "credential": assertion });
    refused(
        &app,
        reauth_finish(&t, &phone_session, &body),
        ErrorCode::ChallengeExpired,
    )
    .await;
}

#[tokio::test]
async fn a_cloned_authenticator_is_refused() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);
    let created = register(&app, &t, &cookie, &mut device, None).await;
    let mut clone = device.clone_device();

    sign_in_with(&app, &t, &mut device).await;
    // The copy signs with a counter the server has seen already.
    let start = start_sign_in(&app, &t).await;
    let assertion = clone.get(&start["publicKey"]).await;
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_eq!(detail.as_deref(), Some("counter_not_increased"));
    assert_eq!(
        audit_rows(&control_db(&t), "passkey.clone_suspected"),
        [json!({ "id": created["id"] })]
    );
    // The original goes on: its counter keeps growing.
    sign_in_with(&app, &t, &mut device).await;
}

#[tokio::test]
async fn a_synced_passkey_signs_in_again_and_again() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    // iCloud Keychain, Google Password Manager: counter 0, backed up.
    let mut phone = SoftPasskey::for_app(&t).synced();
    register(&app, &t, &cookie, &mut phone, Some("iPhone")).await;
    for _ in 0..3 {
        let session = sign_in_with(&app, &t, &mut phone).await;
        assert_eq!(me(&app, &session).await, StatusCode::OK);
    }
    let state: String = control_db(&t)
        .query_row("SELECT passkey_json FROM passkeys", [], |row| row.get(0))
        .unwrap();
    let state: Value = serde_json::from_str(&state).unwrap();
    let credential = &state["cred"];
    assert_eq!(credential["counter"], 0);
    assert_eq!(credential["backup_eligible"], true);
    assert_eq!(credential["backup_state"], true);
    assert_eq!(credential["user_verified"], true);
    assert!(audit_rows(&control_db(&t), "passkey.clone_suspected").is_empty());
}

#[tokio::test]
async fn the_user_must_be_verified() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);

    // A client that asks the authenticator for presence only.
    let start = start_registration(&app, &t, &cookie).await;
    let mut options = start["publicKey"].clone();
    options["authenticatorSelection"]["userVerification"] = json!("discouraged");
    let credential = device.create(&options).await;
    let body = json!({ "ceremonyId": start["ceremonyId"], "credential": credential });
    let request = spa(&t, post_body("/api/v1/me/passkeys", &body), &cookie);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_eq!(detail.as_deref(), Some("user_not_verified"));

    let mut device = SoftPasskey::for_app(&t);
    register(&app, &t, &cookie, &mut device, None).await;
    let start = start_sign_in(&app, &t).await;
    let mut options = start["publicKey"].clone();
    options["userVerification"] = json!("discouraged");
    let assertion = device.get(&options).await;
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_eq!(detail.as_deref(), Some("user_not_verified"));

    // An authenticator that cannot verify the user refuses on its own.
    let mut unverified = SoftPasskey::for_app(&t).without_user_verification();
    let start = start_registration(&app, &t, &cookie).await;
    assert!(unverified.try_create(&start["publicKey"]).await.is_err());
    assert_eq!(unverified.credentials(), 0);
}

#[tokio::test]
async fn the_user_handle_must_name_the_passkeys_account() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);
    register(&app, &t, &cookie, &mut device, None).await;

    // The signature does not cover the user handle: the server checks it.
    for (handle, reason) in [
        (
            Some(json!(URL_SAFE_NO_PAD.encode([9_u8; 16]))),
            "user_handle_mismatch",
        ),
        (None, "bad_user_handle"),
    ] {
        let start = start_sign_in(&app, &t).await;
        let mut assertion = device.get(&start["publicKey"]).await;
        let response = assertion["response"].as_object_mut().unwrap();
        match handle {
            Some(handle) => {
                response.insert("userHandle".to_owned(), handle);
            }
            None => {
                response.remove("userHandle");
            }
        }
        let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
        let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
        assert_eq!(detail.as_deref(), Some(reason));
    }
}

#[tokio::test]
async fn unknown_removed_and_inactive_passkeys_sign_nobody_in() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);
    let created = register(&app, &t, &cookie, &mut device, None).await;

    // A passkey this server never registered.
    let mut stranger = SoftPasskey::for_app(&t);
    let options = json!({
        "rp": { "id": "localhost", "name": "Elsewhere" },
        "user": { "id": URL_SAFE_NO_PAD.encode([1_u8; 16]), "name": "x", "displayName": "x" },
        "challenge": URL_SAFE_NO_PAD.encode([2_u8; 32]),
        "pubKeyCredParams": [{ "type": "public-key", "alg": -7 }],
        "authenticatorSelection": { "residentKey": "required", "userVerification": "required" },
    });
    stranger.create(&options).await;
    let start = start_sign_in(&app, &t).await;
    let assertion = stranger.get(&start["publicKey"]).await;
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_eq!(detail.as_deref(), Some("unknown_credential"));

    // A disabled account's passkey.
    control_db(&t)
        .execute("UPDATE users SET status = 'disabled'", [])
        .unwrap();
    let start = start_sign_in(&app, &t).await;
    let assertion = device.get(&start["publicKey"]).await;
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_eq!(detail.as_deref(), Some("account_inactive"));
    control_db(&t)
        .execute("UPDATE users SET status = 'active'", [])
        .unwrap();

    // A removed passkey.
    let cookie = sign_in(&app, &t).await;
    let response = send(&app, spa(&t, delete(&created["id"]), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let start = start_sign_in(&app, &t).await;
    let assertion = device.get(&start["publicKey"]).await;
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &assertion);
    let detail = refused(&app, request, ErrorCode::PasskeyInvalid).await;
    assert_eq!(detail.as_deref(), Some("unknown_credential"));
}

#[tokio::test]
async fn sensitive_passkey_changes_need_a_recent_sign_in() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);
    let created = register(&app, &t, &cookie, &mut device, None).await;

    make_sessions_stale(&t);
    for request in [
        spa(&t, post("/api/v1/me/passkeys/start"), &cookie),
        spa(&t, delete(&created["id"]), &cookie),
    ] {
        let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
        assert_eq!(refused.code, ErrorCode::ReauthRequired);
    }
    // Listing needs no recent sign-in.
    assert_eq!(passkeys(&app, &cookie).await.len(), 1);

    // Re-authenticated with the passkey: the options name the account's
    // passkeys, and the routes open again.
    let response = send(&app, reauth_start(&t, &cookie, "passkey")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let start = json(response).await;
    assert_schema("PasskeyAssertionStart", &start);
    let allowed = start["publicKey"]["allowCredentials"].as_array().unwrap();
    assert_eq!(allowed.len(), 1);
    assert_eq!(allowed[0]["type"], "public-key");
    assert_eq!(start["publicKey"]["userVerification"], "required");
    reauth_with(&app, &t, &cookie, &mut device).await;
    let response = send(&app, spa(&t, delete(&created["id"]), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(passkeys(&app, &cookie).await.is_empty());

    let conn = control_db(&t);
    assert_eq!(
        audit_rows(&conn, "session.reauth"),
        [json!({ "method": "passkey" })]
    );
    assert_eq!(
        audit_rows(&conn, "passkey.delete"),
        [json!({ "id": created["id"] })]
    );

    // Without a passkey, re-authentication by passkey is not offered.
    let response = send(&app, reauth_start(&t, &cookie, "passkey")).await;
    assert_eq!(
        problem(response, StatusCode::NOT_FOUND).await.code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn a_reauth_link_reopens_sensitive_routes_for_its_account_only() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    add_member(&t, MEMBER_EMAIL);
    make_sessions_stale(&t);
    let start = || spa(&t, post("/api/v1/me/passkeys/start"), &cookie);
    assert_eq!(send(&app, start()).await.status(), StatusCode::FORBIDDEN);

    // Another account's link, a sign-in link, nonsense: refused, unused.
    let members = reauth_link_token(&t, MEMBER_EMAIL);
    let login = support::auth::link_token(&t, OWNER_EMAIL);
    for token in [members.as_str(), login.as_str(), "nonsense"] {
        let body = json!({ "method": "link", "token": token });
        refused(
            &app,
            reauth_finish(&t, &cookie, &body),
            ErrorCode::InvalidLink,
        )
        .await;
    }
    assert_eq!(send(&app, start()).await.status(), StatusCode::FORBIDDEN);
    let response = send(&app, redeem_request(&t, &login)).await;
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "still a sign-in link"
    );

    // The account's own link works once, and only for re-authentication.
    let token = reauth_link_token(&t, OWNER_EMAIL);
    refused(&app, redeem_request(&t, &token), ErrorCode::InvalidLink).await;
    let body = json!({ "method": "link", "token": token });
    let response = send(&app, reauth_finish(&t, &cookie, &body)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(send(&app, start()).await.status(), StatusCode::OK);
    refused(
        &app,
        reauth_finish(&t, &cookie, &body),
        ErrorCode::InvalidLink,
    )
    .await;

    let conn = control_db(&t);
    assert_eq!(
        audit_rows(&conn, "session.reauth"),
        [json!({ "method": "magic_link" })]
    );
    let minted = audit_rows(&conn, "magic_link.create");
    assert!(minted.contains(&json!({ "via": "cli", "purpose": "reauth" })));
}

#[tokio::test]
async fn a_reauth_link_can_come_by_email() {
    let t = roomy(|config| {
        config.mail = MailConfig::dev_mailbox(&config.data_dir);
    });
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    make_sessions_stale(&t);

    let response = send(&app, reauth_start(&t, &cookie, "email")).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let messages = mailbox(&t).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("Subject: Confirm it is you on Shelfy"));
    assert!(messages[0].contains("To: owner@example.test"));
    let page = format!("{}/login/reauth#", t.state.config().public_url);
    let url = messages[0]
        .split_whitespace()
        .find(|word| word.starts_with(&page))
        .expect("a re-authentication link");
    let token = &url[page.len()..];
    let body = json!({ "method": "link", "token": token });
    let response = send(&app, reauth_finish(&t, &cookie, &body)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        audit_rows(&control_db(&t), "magic_link.create")
            .last()
            .unwrap(),
        &json!({ "via": "email", "purpose": "reauth" })
    );

    // Three emails an hour per address, sign-in emails included.
    for _ in 0..2 {
        let response = send(&app, reauth_start(&t, &cookie, "email")).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    let response = send(&app, reauth_start(&t, &cookie, "email")).await;
    problem(response, StatusCode::TOO_MANY_REQUESTS).await;

    // Without email, there is no such way.
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let response = send(&app, reauth_start(&t, &cookie, "email")).await;
    problem(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn passkeys_belong_to_their_account() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut owners_device = SoftPasskey::for_app(&t);
    let owners = register(&app, &t, &cookie, &mut owners_device, Some("Owner")).await;

    add_member(&t, MEMBER_EMAIL);
    let member = sign_in_as(&app, &t, MEMBER_EMAIL).await;
    assert!(passkeys(&app, &member).await.is_empty());
    // Another account's passkey is a 404, like a missing one.
    for id in [owners["id"].clone(), json!(999_999)] {
        let response = send(&app, spa(&t, delete(&id), &member)).await;
        assert_eq!(
            problem(response, StatusCode::NOT_FOUND).await.code,
            ErrorCode::NotFound
        );
    }
    assert_eq!(passkeys(&app, &cookie).await, std::slice::from_ref(&owners));

    // The member re-authenticates with their own passkey, not the owner's.
    let mut members_device = SoftPasskey::for_app(&t);
    register(&app, &t, &member, &mut members_device, None).await;
    let start = json(send(&app, reauth_start(&t, &member, "passkey")).await).await;
    let mut options = start["publicKey"].clone();
    options["allowCredentials"] = json!([]);
    let assertion = owners_device.get(&options).await;
    let body =
        json!({ "method": "passkey", "ceremonyId": start["ceremonyId"], "credential": assertion });
    let detail = refused(
        &app,
        reauth_finish(&t, &member, &body),
        ErrorCode::PasskeyInvalid,
    )
    .await;
    assert_eq!(detail.as_deref(), Some("unknown_credential"));
    // Each passkey signs its own account in.
    let session = sign_in_with(&app, &t, &mut members_device).await;
    let profile = json(send(&app, with_session(get("/api/v1/me"), &session)).await).await;
    assert_eq!(profile["email"], MEMBER_EMAIL);
}

#[tokio::test]
async fn labels_are_checked_before_the_ceremony_is_used() {
    let t = roomy(|_| {});
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);
    let start = start_registration(&app, &t, &cookie).await;
    let credential = device.create(&start["publicKey"]).await;
    let mut body = json!({
        "ceremonyId": start["ceremonyId"],
        "credential": credential,
        "label": "x".repeat(65),
    });
    let request = spa(&t, post_body("/api/v1/me/passkeys", &body), &cookie);
    let problem = problem(send(&app, request).await, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(problem.errors[0].field, "label");
    body["label"] = json!("Phone");
    let request = spa(&t, post_body("/api/v1/me/passkeys", &body), &cookie);
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(json(response).await["label"], "Phone");
}

/// `request` as if it came over TCP from `peer`.
fn from_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    let ip = peer.parse().unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 40_000)));
    request
}

#[tokio::test]
async fn passkey_sign_in_shares_the_sign_in_rate_limit() {
    let t = TestState::new();
    let app = t.app();
    let start = |peer: &str| {
        from_peer(
            from_spa(&t, post("/api/v1/auth/passkeys/login/start")),
            peer,
        )
    };
    // 10 sign-in requests a minute per client: link redemptions and passkey
    // ceremonies count alike.
    for i in 0..5 {
        let unknown = SecretToken::generate();
        let request = from_peer(redeem_request(&t, unknown.expose()), "198.51.100.7");
        let response = send(&app, request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "redeem {i}");
        assert_eq!(
            send(&app, start("198.51.100.7")).await.status(),
            StatusCode::OK
        );
    }
    let response = send(&app, start("198.51.100.7")).await;
    assert!(response.headers().contains_key("retry-after"));
    problem(response, StatusCode::TOO_MANY_REQUESTS).await;
    let begun = json(send(&app, start("198.51.100.8")).await).await;
    let request = from_peer(
        finish_sign_in_request(&t, &begun["ceremonyId"], &sample_assertion()),
        "198.51.100.7",
    );
    problem(send(&app, request).await, StatusCode::TOO_MANY_REQUESTS).await;
    // Another client has its own budget.
    assert_eq!(
        send(&app, start("198.51.100.8")).await.status(),
        StatusCode::OK
    );
}

/// A well-formed answer that no passkey of this server signed.
fn sample_assertion() -> Value {
    let bytes = |n: usize| json!(URL_SAFE_NO_PAD.encode(vec![5_u8; n]));
    json!({
        "id": bytes(16),
        "rawId": bytes(16),
        "type": "public-key",
        "response": {
            "clientDataJSON": bytes(64),
            "authenticatorData": bytes(37),
            "signature": bytes(70),
            "userHandle": bytes(16),
        },
        "clientExtensionResults": {},
    })
}

/// Inserts an API token with every scope for `user_id`; returns its value.
fn api_token(t: &TestState, user_id: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    control_db(t)
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, 'extension', ?3, ?4, ?5)",
            params![
                new_ulid(),
                user_id,
                hash_token(&token).as_slice(),
                "ingest tasks uploads lookup links:create migrate",
                now_ms()
            ],
        )
        .unwrap();
    token
}

#[tokio::test]
async fn every_passkey_route_has_its_access() {
    let t = roomy(|_| {});
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let token = api_token(&t, &owner_id);
    let session_routes: [(&str, &str); 6] = [
        ("GET", "/api/v1/me/passkeys"),
        ("POST", "/api/v1/me/passkeys/start"),
        ("POST", "/api/v1/me/passkeys"),
        ("DELETE", "/api/v1/me/passkeys/1"),
        ("POST", "/api/v1/auth/reauth/start"),
        ("POST", "/api/v1/auth/reauth/finish"),
    ];
    for (method, uri) in session_routes {
        let request = || {
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(json!({ "method": "passkey" }).to_string()))
                .unwrap();
            from_spa(&t, request)
        };
        // No session, an unknown one, or an API token beside a valid cookie.
        let unknown = SecretToken::generate();
        let mut with_token = with_session(request(), &cookie);
        with_token
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        for (what, request) in [
            ("no session", request()),
            (
                "an unknown session",
                with_session(request(), unknown.expose()),
            ),
            ("a token", with_token),
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
    // The sign-in routes are public: they answer without a session.
    let start = start_sign_in(&app, &t).await;
    let request = finish_sign_in_request(&t, &start["ceremonyId"], &json!({}));
    let response = send(&app, request).await;
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "a malformed answer"
    );
}
