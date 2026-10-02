//! Passkeys against a running server, over HTTP: a sign-in link, then a
//! passkey registration, a sign-out, a username-less sign-in, a
//! re-authentication and the passkey's removal, with the software passkey
//! of `support::passkey` playing the browser. Ignored by default; run it
//! against a server whose public URL is `SHELFY_LIVE_URL`, with a sign-in
//! link in `SHELFY_LIVE_LINK`, or the output of `admin login-link` in the
//! file `SHELFY_LIVE_LINK_FILE`:
//!
//! ```sh
//! shelfy-server admin create-owner --email owner@example.test
//! shelfy-server admin login-link --email owner@example.test > link.txt
//! SHELFY_LIVE_URL=http://localhost:18193 SHELFY_LIVE_LINK_FILE=link.txt \
//!   cargo test -p shelfy-server --test passkeys_live -- --ignored --nocapture
//! ```
//!
//! The link is spent; the passkey is removed at the end, so the account is
//! left as it was.

mod support;

use serde_json::{Value, json};
use support::passkey::SoftPasskey;
use ureq::Agent;
use ureq::http::Response;

const COOKIE: &str = "__Host-shelfy_session";

/// A client of the server at `base`, sending what the web app sends.
struct Live {
    agent: Agent,
    base: String,
    cookie: Option<String>,
}

impl Live {
    fn new(base: &str) -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            base: base.trim_end_matches('/').to_owned(),
            cookie: None,
        }
    }

    /// A request to `path` with `body` (JSON, or none); its status and JSON
    /// answer (`null` when empty). Keeps a session cookie the answer sets.
    fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let url = format!("{}{path}", self.base);
        let mut request = ureq::http::Request::builder()
            .method(method)
            .uri(&url)
            .header("origin", &self.base)
            .header("x-shelfy-client", "web");
        if let Some(cookie) = &self.cookie {
            request = request.header("cookie", format!("{COOKIE}={cookie}"));
        }
        let response = match body {
            Some(body) => self.agent.run(
                request
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .unwrap(),
            ),
            None => self.agent.run(request.body(()).unwrap()),
        }
        .unwrap_or_else(|err| panic!("{method} {url}: {err}"));
        self.keep_cookie(&response);
        let status = response.status().as_u16();
        let mut body = response.into_body();
        let text = body.read_to_string().unwrap();
        let json = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, json)
    }

    fn keep_cookie<B>(&mut self, response: &Response<B>) {
        for value in response.headers().get_all("set-cookie") {
            let value = value.to_str().unwrap();
            let pair = value.split(';').next().unwrap();
            if let Some(cookie) = pair.strip_prefix(&format!("{COOKIE}=")) {
                self.cookie = (!cookie.is_empty()).then(|| cookie.to_owned());
            }
        }
    }
}

/// The sign-in link: `SHELFY_LIVE_LINK`, or the last line of the file
/// `SHELFY_LIVE_LINK_FILE`.
fn live_link() -> String {
    if let Ok(link) = std::env::var("SHELFY_LIVE_LINK") {
        return link;
    }
    let path =
        std::env::var("SHELFY_LIVE_LINK_FILE").expect("SHELFY_LIVE_LINK or SHELFY_LIVE_LINK_FILE");
    let output = std::fs::read_to_string(&path).expect("the link file");
    output.lines().last().expect("a link line").to_owned()
}

#[tokio::test]
#[ignore = "needs a running server: set SHELFY_LIVE_URL and SHELFY_LIVE_LINK(_FILE)"]
async fn a_passkey_registers_and_signs_in_on_a_running_server() {
    let base = std::env::var("SHELFY_LIVE_URL").expect("SHELFY_LIVE_URL");
    let link = live_link();
    let token = link.split_once('#').expect("a sign-in link").1.trim();
    let mut live = Live::new(&base);

    let (status, methods) = live.send("GET", "/api/v1/auth/methods", None);
    assert_eq!((status, methods["passkeys"].clone()), (200, json!(true)));
    let (status, _) = live.send(
        "POST",
        "/api/v1/auth/magic-links/redeem",
        Some(&json!({ "token": token })),
    );
    assert_eq!(status, 204, "sign-in link");
    let (status, me) = live.send("GET", "/api/v1/me", None);
    assert_eq!(status, 200);
    println!("signed in with the link as {}", me["id"]);

    // Register a passkey.
    let mut device = SoftPasskey::on(&base);
    let (status, start) = live.send("POST", "/api/v1/me/passkeys/start", None);
    assert_eq!(status, 200, "{start}");
    println!(
        "registration options: rp {}, residentKey {}, userVerification {}",
        start["publicKey"]["rp"]["id"],
        start["publicKey"]["authenticatorSelection"]["residentKey"],
        start["publicKey"]["authenticatorSelection"]["userVerification"],
    );
    let credential = device.create(&start["publicKey"]).await;
    let body = json!({
        "ceremonyId": start["ceremonyId"],
        "credential": credential,
        "label": "live test",
    });
    let (status, passkey) = live.send("POST", "/api/v1/me/passkeys", Some(&body));
    assert_eq!(status, 201, "{passkey}");
    println!("registered passkey {}", passkey["id"]);

    // Sign out, then back in with the passkey alone.
    let (status, _) = live.send("POST", "/api/v1/auth/logout", None);
    assert_eq!(status, 204);
    assert!(live.cookie.is_none());
    let (status, _) = live.send("GET", "/api/v1/me", None);
    assert_eq!(status, 401);
    let (status, start) = live.send("POST", "/api/v1/auth/passkeys/login/start", None);
    assert_eq!(status, 200, "{start}");
    let assertion = device.get(&start["publicKey"]).await;
    let body = json!({ "ceremonyId": start["ceremonyId"], "credential": assertion });
    let (status, answer) = live.send("POST", "/api/v1/auth/passkeys/login/finish", Some(&body));
    assert_eq!(status, 204, "{answer}");
    let (status, again) = live.send("GET", "/api/v1/me", None);
    assert_eq!((status, again["id"].clone()), (200, me["id"].clone()));
    println!("signed in with the passkey, username-less");

    // Re-authenticate with it, and remove it.
    let (status, start) = live.send(
        "POST",
        "/api/v1/auth/reauth/start",
        Some(&json!({ "method": "passkey" })),
    );
    assert_eq!(status, 200, "{start}");
    let assertion = device.get(&start["publicKey"]).await;
    let body = json!({
        "method": "passkey",
        "ceremonyId": start["ceremonyId"],
        "credential": assertion,
    });
    let (status, answer) = live.send("POST", "/api/v1/auth/reauth/finish", Some(&body));
    assert_eq!(status, 204, "{answer}");
    let path = format!("/api/v1/me/passkeys/{}", passkey["id"]);
    let (status, answer) = live.send("DELETE", &path, None);
    assert_eq!(status, 204, "{answer}");
    let (status, list) = live.send("GET", "/api/v1/me/passkeys", None);
    assert_eq!((status, list["items"].clone()), (200, json!([])));
    println!("re-authenticated with the passkey and removed it");
}
