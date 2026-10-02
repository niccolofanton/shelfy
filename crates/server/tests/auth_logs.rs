//! Sign-in in the JSON logs: the request span names the signed-in user, and
//! no session token, link token or email address is ever written (plan §3.7).
//!
//! Its own test binary: it installs a scoped log subscriber, which other
//! tests running in parallel threads would race with.

mod support;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use shelfy_server::config::Config;
use shelfy_server::mail::MailConfig;
use shelfy_server::telemetry::http::REQUEST_ID_HEADER;
use shelfy_server::telemetry::json_layer;
use support::auth::{
    OWNER_EMAIL, link_in, mailbox, owner, post, redeem_request, session_cookie, spa, token_of,
    with_session,
};
use support::{TestState, get, post_json, send};
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

#[tokio::test(flavor = "current_thread")]
async fn logs_name_the_user_but_never_tokens_or_addresses() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(json_layer(capture.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);

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

    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
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
