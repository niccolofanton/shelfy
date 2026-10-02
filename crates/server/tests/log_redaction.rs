//! Plan §3.7: secrets never reach the logs. Each kind of value the plan
//! names is planted in real requests and library rows (a session id, a
//! bearer token, an email address, a caption, a post URL, a query string,
//! a note), sent through the whole stack, and none of them may appear in
//! the captured JSON logs, at any level.
//!
//! Its own test binary: it installs the global log subscriber, at every
//! level, so the work on the blocking pool and in background tasks is
//! captured too.

mod support;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, NewPost};
use shelfy_server::admin::migrate_token::{MIGRATE_TOKEN_TTL, create_migrate_token};
use shelfy_server::config::Config;
use shelfy_server::mail::MailConfig;
use shelfy_server::telemetry::json_layer;
use shelfy_server::tokens::SecretToken;
use support::auth::{OWNER_EMAIL, from_spa, link_in, mailbox, owner, sign_in, spa, with_session};
use support::library::NOW;
use support::{TestState, body, get, post_json, send};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

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

const POST_KEY: &str = "ig_3459872345987234598";
const CAPTION: &str = "Planted caption: a blown-glass lamp from 1962";
const POST_URL: &str = "https://www.instagram.com/p/PlantedUrl77x/";
const NOTE: &str = "planted note about the lamp";
const QUERY_TEXT: &str = "plantedquery91";
const STRANGER: &str = "stranger.planted@example.test";

#[tokio::test(flavor = "current_thread")]
async fn planted_secrets_never_reach_the_logs() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(json_layer(capture.clone()));
    tracing::subscriber::set_global_default(subscriber).expect("the only subscriber");

    let t = TestState::with_config(|config: &mut Config| {
        config.mail = MailConfig::dev_mailbox(&config.data_dir);
    });
    let app = t.app();
    let owner_id = owner(&t);

    // A session id: a real sign-in, and a forged cookie.
    let cookie = sign_in(&app, &t).await;
    let forged = SecretToken::generate();
    // A bearer token: the migration CLI's, and an unknown one.
    let token = create_migrate_token(&t.data_dir(), OWNER_EMAIL, MIGRATE_TOKEN_TTL)
        .unwrap()
        .token
        .expose()
        .clone();
    let unknown_token = format!("shx_{}", SecretToken::generate().expose());
    // A caption and a post URL, in the library.
    t.write(&owner_id, |tx| {
        let mut post = NewPost::new(
            POST_KEY,
            Platform::Instagram,
            "3459872345987234598",
            "image",
            NOW,
        );
        post.caption = Some(CAPTION.into());
        post.post_url = Some(POST_URL.into());
        post.shortcode = Some("PlantedUrl77x".into());
        posts::insert(tx, &post, NOW)
    })
    .await;

    let mut statuses = Vec::new();
    let mut run = async |request: Request<Body>| {
        let response = send(&app, request).await;
        statuses.push(response.status());
        String::from_utf8_lossy(&body(response).await).into_owned()
    };
    // Reads that return the caption and the URL; searches by query string.
    let listed = run(with_session(
        get("/api/v1/posts?includeTotal=true"),
        &cookie,
    ))
    .await;
    assert!(
        listed.contains(CAPTION) && listed.contains(POST_URL),
        "{listed}"
    );
    run(with_session(
        get(&format!("/api/v1/posts/{POST_KEY}")),
        &cookie,
    ))
    .await;
    for path in [
        format!("/api/v1/search?q={QUERY_TEXT}"),
        format!("/api/v1/posts?q={QUERY_TEXT}&limit=5"),
        format!("/api/v1/posts/count?q={QUERY_TEXT}"),
        format!("/api/v1/nope?url={POST_URL}&q={QUERY_TEXT}"),
    ] {
        run(with_session(get(&path), &cookie)).await;
    }
    // A note, stored and read back.
    let edit = Request::patch(format!("/api/v1/posts/{POST_KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "userNote": NOTE }).to_string()))
        .unwrap();
    assert!(run(spa(&t, edit, &cookie)).await.contains(NOTE));
    let post = with_session(get(&format!("/api/v1/posts/{POST_KEY}")), &cookie);
    assert!(run(post).await.contains(NOTE));
    // Credentials that sign nobody in.
    run(with_session(get("/api/v1/stats"), forged.expose())).await;
    let bearer = Request::get("/api/v1/stats")
        .header(header::AUTHORIZATION, format!("Bearer {unknown_token}"))
        .body(Body::empty())
        .unwrap();
    run(bearer).await;
    let migrate = Request::post("/api/v1/migrations/missing-objects")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"objects":[]}"#))
        .unwrap();
    run(migrate).await;
    // Email addresses: the account's (an email goes out) and a stranger's.
    for address in [OWNER_EMAIL, STRANGER] {
        let request = post_json(
            "/api/v1/auth/magic-links",
            json!({ "email": address }).to_string(),
        );
        run(from_spa(&t, request)).await;
    }
    let sent = mailbox(&t).await;
    assert_eq!(sent.len(), 1);
    let link = link_in(&sent[0]);
    // A crash report whose free text carries them all.
    let message = format!(
        "Failed to load {POST_URL} for {OWNER_EMAIL}: GET /api/v1/search?q={QUERY_TEXT} \
         with {cookie} and {token}; open {link}"
    );
    let report = post_json(
        "/api/v1/client-errors",
        json!({
            "view": "postModal",
            "message": message,
            "stack": format!("Error\n    at Modal (http://localhost:8080/assets/index-3f2a.js:1:2)\n    at {POST_URL}"),
        })
        .to_string(),
    );
    run(spa(&t, report, &cookie)).await;
    drop(run);
    assert!(
        !statuses.iter().any(StatusCode::is_server_error),
        "{statuses:?}"
    );

    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("every log line is JSON"))
        .collect();
    let link_token = link.rsplit('#').next().unwrap().to_owned();
    for secret in [
        cookie.as_str(),
        forged.expose(),
        token.as_str(),
        unknown_token.as_str(),
        OWNER_EMAIL,
        STRANGER,
        CAPTION,
        "blown-glass lamp",
        POST_URL,
        "PlantedUrl77x",
        QUERY_TEXT,
        NOTE,
        link_token.as_str(),
    ] {
        assert!(
            !text.contains(secret),
            "{secret} leaked into the logs:\n{text}"
        );
    }

    // The logs are there: one line per response, and the crash report with
    // its free text scrubbed.
    let responses = lines
        .iter()
        .filter(|line| line["message"] == "request")
        .count();
    assert!(
        responses >= statuses.len(),
        "{responses} lines for {statuses:?}"
    );
    let report = lines
        .iter()
        .find(|line| line["message"] == "client error")
        .expect("the crash report is logged");
    let logged = report["error_message"].as_str().unwrap();
    for kept in [
        "Failed to load [url]",
        "[email]",
        "?[redacted]",
        "open [url]",
    ] {
        assert!(logged.contains(kept), "{logged}");
    }
    let stack = report["stack"].as_str().unwrap();
    assert!(
        stack.contains("http://localhost:8080/assets/index-3f2a.js:1:2"),
        "{stack}"
    );
    assert!(stack.ends_with("at [url]"), "{stack}");
}
