//! Sign-in in the JSON logs: the request span names the signed-in user, and
//! no session token, link token, email address, passkey credential id,
//! public key, challenge or ceremony id is ever written, at any level
//! (plan §3.7).
//!
//! Its own test binary: it installs the global log subscriber, at every
//! level, so the work on the blocking pool is captured too.

mod support;

use std::io::{self, Write};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use shelfy_server::config::Config;
use shelfy_server::mail::MailConfig;
use shelfy_server::telemetry::http::REQUEST_ID_HEADER;
use shelfy_server::telemetry::json_layer;
use support::auth::{
    OWNER_EMAIL, control_db, link_in, mailbox, make_sessions_stale, owner, post, redeem_request,
    session_cookie, sign_in, spa, token_of, with_session,
};
use support::passkey::{
    SoftPasskey, finish_sign_in_request, post_body, reauth_with, start_registration, start_sign_in,
};
use support::{TestState, get, json as body_json, post_json, send};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

fn email_request(email: &str) -> Request<Body> {
    post_json(
        "/api/v1/auth/magic-links",
        json!({ "email": email }).to_string(),
    )
}

/// Collects everything the log layer writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// The process's log subscriber: the server's JSON layer, at every level
/// (no `RUST_LOG` filter), writing to one buffer that the tests of this
/// binary share.
fn capture() -> Capture {
    static CAPTURE: OnceLock<Capture> = OnceLock::new();
    CAPTURE
        .get_or_init(|| {
            let capture = Capture::default();
            let subscriber = tracing_subscriber::registry().with(json_layer(capture.clone()));
            tracing::subscriber::set_global_default(subscriber).expect("the only subscriber");
            capture
        })
        .clone()
}

#[tokio::test(flavor = "current_thread")]
async fn logs_name_the_user_but_never_tokens_or_addresses() {
    let capture = capture();

    let t = TestState::with_config(|config: &mut Config| {
        config.mail = MailConfig::dev_mailbox(&config.data_dir);
    });
    let app = t.app();
    let owner_id = owner(&t);
    let request_id = |response: &axum::http::Response<Body>| {
        response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_owned()
    };

    let response = send(&app, email_request(OWNER_EMAIL)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let email_id = request_id(&response);
    let link = link_in(&mailbox(&t).await[0]);
    let link_token = token_of(&link);
    let response = send(&app, redeem_request(&t, &link_token)).await;
    let open_id = request_id(&response);
    let cookie = session_cookie(&response).unwrap();
    let response = send(&app, with_session(get("/api/v1/me"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let me_id = request_id(&response);
    let response = send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let logout_id = request_id(&response);

    let text = capture.text();
    for secret in [OWNER_EMAIL, link_token.as_str(), cookie.as_str()] {
        assert!(
            !text.contains(secret),
            "{secret} leaked into the logs:\n{text}"
        );
    }
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let find = |id: &str, message: &str| {
        lines
            .iter()
            .find(|line| line["span"]["request_id"] == id && line["message"] == message)
            .unwrap_or_else(|| panic!("no {message:?} line for request {id} in\n{text}"))
    };
    // The user is named once known: signing in, signed-in requests, signing out.
    for id in [&open_id, &me_id, &logout_id] {
        assert_eq!(find(id, "request")["span"]["user_id"], owner_id.as_str());
    }
    assert!(find(&email_id, "request")["span"]["user_id"].is_null());
    // The background email keeps the request's span.
    assert_eq!(
        find(&email_id, "sign-in email sent")["transport"],
        "dev-mailbox"
    );
}

/// The forms a binary secret could take in a log line: base64url, standard
/// base64, hex, and the start of a Rust byte list (`Debug`).
fn forms(bytes: &[u8]) -> Vec<String> {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let list = format!("{:?}", &bytes[..bytes.len().min(8)]);
    vec![
        URL_SAFE_NO_PAD.encode(bytes),
        STANDARD.encode(bytes).trim_end_matches('=').to_owned(),
        hex,
        list.trim_end_matches(']').to_owned(),
    ]
}

/// The bytes of a base64url JSON string.
fn decoded(value: &Value) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(value.as_str().expect("a base64url string"))
        .expect("base64url")
}

/// The request id of `response`.
fn request_id(response: &axum::http::Response<Body>) -> String {
    response.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn passkey_ceremonies_never_log_credentials_challenges_or_ceremonies() {
    let capture = capture();
    let t = TestState::with_config(|config: &mut Config| {
        config.mail = MailConfig::dev_mailbox(&config.data_dir);
    });
    let app = t.app();
    let owner_id = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let mut device = SoftPasskey::for_app(&t);
    let mut secrets: Vec<Vec<u8>> = Vec::new();
    let mut texts: Vec<String> = vec![OWNER_EMAIL.to_owned(), cookie.clone()];

    // Registration.
    let start = start_registration(&app, &t, &cookie).await;
    texts.push(start["ceremonyId"].as_str().unwrap().to_owned());
    secrets.push(decoded(&start["publicKey"]["challenge"]));
    secrets.push(decoded(&start["publicKey"]["user"]["id"]));
    let credential = device.create(&start["publicKey"]).await;
    secrets.push(decoded(&credential["rawId"]));
    secrets.push(decoded(&credential["response"]["publicKey"]));
    let body = json!({ "ceremonyId": start["ceremonyId"], "credential": credential });
    let response = send(
        &app,
        spa(&t, post_body("/api/v1/me/passkeys", &body), &cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created_id = request_id(&response);
    // The key's coordinates, as stored.
    let stored: String = control_db(&t)
        .query_row("SELECT passkey_json FROM passkeys", [], |row| row.get(0))
        .unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    let key = &stored["cred"]["cred"]["key"]["EC_EC2"];
    secrets.push(decoded(&key["x"]));
    secrets.push(decoded(&key["y"]));

    // Sign-in, then a replay of its answer, which is refused.
    let start = start_sign_in(&app, &t).await;
    texts.push(start["ceremonyId"].as_str().unwrap().to_owned());
    secrets.push(decoded(&start["publicKey"]["challenge"]));
    let assertion = device.get(&start["publicKey"]).await;
    secrets.push(decoded(&assertion["response"]["signature"]));
    let response = send(
        &app,
        finish_sign_in_request(&t, &start["ceremonyId"], &assertion),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let signed_in_id = request_id(&response);
    let session = session_cookie(&response).unwrap();
    texts.push(session.clone());
    let other = start_sign_in(&app, &t).await;
    let response = send(
        &app,
        finish_sign_in_request(&t, &other["ceremonyId"], &assertion),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let refused_id = request_id(&response);

    // Re-authentication, with the passkey and with an emailed link.
    make_sessions_stale(&t);
    reauth_with(&app, &t, &session, &mut device).await;
    let request = spa(
        &t,
        post_body("/api/v1/auth/reauth/start", &json!({ "method": "email" })),
        &session,
    );
    assert_eq!(send(&app, request).await.status(), StatusCode::ACCEPTED);
    let message = mailbox(&t).await.remove(0);
    let page = "/login/reauth#";
    let url = message
        .split_whitespace()
        .find(|word| word.contains(page))
        .expect("a re-authentication link");
    let token = url.split_once(page).unwrap().1.to_owned();
    texts.push(token.clone());
    let body = json!({ "method": "link", "token": token });
    let request = spa(&t, post_body("/api/v1/auth/reauth/finish", &body), &session);
    assert_eq!(send(&app, request).await.status(), StatusCode::NO_CONTENT);
    let response = send(&app, with_session(get("/api/v1/me/passkeys"), &session)).await;
    assert_eq!(
        body_json(response).await["items"].as_array().unwrap().len(),
        1
    );

    let text = capture.text();
    for secret in &texts {
        assert!(
            !text.contains(secret.as_str()),
            "{secret} leaked into the logs"
        );
    }
    for secret in &secrets {
        for form in forms(secret) {
            assert!(!text.contains(&form), "{form} leaked into the logs");
        }
    }
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let find = |id: &str, message: &str| {
        lines
            .iter()
            .find(|line| line["span"]["request_id"] == id && line["message"] == message)
            .unwrap_or_else(|| panic!("no {message:?} line for request {id}"))
            .clone()
    };
    // The account is named, and a refusal says why, by code.
    assert_eq!(
        find(&created_id, "request")["span"]["user_id"],
        owner_id.as_str()
    );
    assert_eq!(
        find(&signed_in_id, "request")["span"]["user_id"],
        owner_id.as_str()
    );
    assert_eq!(
        find(&refused_id, "passkey refused")["reason"],
        "challenge_mismatch"
    );
}
