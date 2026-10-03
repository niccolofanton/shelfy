//! The outbound client (P2-04 with P4-01, L11, L15) against the fixture CDN
//! and the proxy stub (`support::cdn`): the address policy, redirects, caps,
//! cookies, decompression, the in-flight limit, the proxy mode, the operator
//! allowlist, the internal client, the metric, and the rule that no other
//! code builds an HTTP client. Nothing here leaves the machine.
//!
//! This file never spells the HTTP client crate's name, so that the
//! single-construction rule can scan it like any other.

mod support;

use std::collections::HashMap;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderValue, Method, header};
use futures_util::StreamExt as _;
use regex::Regex;
use shelfy_server::outbound::{
    EgressError, EgressResponse, HostSet, Lookup, Origin, OriginAllowlist, Outbound,
    OutboundConfig, Purpose, Refusal,
};
use shelfy_server::telemetry::metrics;
use support::cdn::{Answer, FixtureCdn, ProxyStub};
use support::sleeping::SleepingNode;
use tokio::time::Instant;
use url::Url;

const PLAIN: &str = "plain.example.test";
const OTHER: &str = "other.example.test";
const SECURE: &str = "secure.example.test";

/// "hello", gzipped.
const GZIP_HELLO: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07,
    0x00, 0x86, 0xa6, 0x10, 0x36, 0x05, 0x00, 0x00, 0x00,
];

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn refusal(result: Result<EgressResponse, EgressError>) -> Refusal {
    match result {
        Err(EgressError::Refused(refusal)) => refusal,
        other => panic!("not refused: {other:?}"),
    }
}

async fn text(result: Result<EgressResponse, EgressError>) -> String {
    let response = result.expect("a response");
    response.text_capped(64 * 1024).await.expect("a body")
}

/// The fixture, with `PLAIN` and `OTHER` on its http listener and `SECURE`
/// on its https one, and private answers for a few names.
async fn direct(edit: impl FnOnce(&mut OutboundConfig)) -> (FixtureCdn, Outbound) {
    let fixture = FixtureCdn::start().await;
    let lookup: &[(&str, &[IpAddr])] = &[
        ("rebind.example.test", &[ip("127.0.0.1")]),
        ("metadata.example.test", &[ip("169.254.169.254")]),
        ("cgnat.example.test", &[ip("100.100.100.200")]),
        ("ula.example.test", &[ip("fd00::5")]),
        ("mapped.example.test", &[ip("::ffff:192.168.0.1")]),
    ];
    let mut config = fixture.config(&[SECURE], &[PLAIN, OTHER], lookup);
    edit(&mut config);
    let outbound = Outbound::new(&config).unwrap();
    (fixture, outbound)
}

#[tokio::test]
async fn literal_private_addresses_are_refused_before_any_connection() {
    let (fixture, outbound) = direct(|_| {}).await;
    let link = outbound.client(Purpose::Link);
    for url in [
        "http://127.0.0.1/",
        "http://127.1/",
        "http://2130706433/",
        "http://0x7f000001/",
        "http://0177.0.0.1/",
        "http://0x7f.1/",
        "http://[::1]/",
        "http://[::ffff:127.0.0.1]/",
        "http://[::ffff:a00:1]/",
        "http://10.0.0.1/",
        "http://172.16.5.4/",
        "http://192.168.1.1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://100.64.0.1/",
        "http://100.101.102.103/",
        "http://[fd00::1]/",
        "http://[fe80::1]/",
        "http://224.0.0.1/",
        "http://0.0.0.0/",
        "http://[::]/",
        "http://[64:ff9b::a00:1]/",
        "http://[2002:7f00:1::1]/",
        "https://198.18.0.1/",
    ] {
        assert_eq!(
            refusal(link.get(url).send().await),
            Refusal::Address,
            "{url}"
        );
    }
    assert!(fixture.hits().is_empty());
}

#[tokio::test]
async fn hostile_urls_are_refused() {
    let (fixture, outbound) = direct(|_| {}).await;
    let link = outbound.client(Purpose::Link);
    for (url, expected) in [
        ("http://localhost/", Refusal::Host),
        ("http://LocalHost./", Refusal::Host),
        ("http://app.localhost/", Refusal::Host),
        ("http://metadata/computeMetadata/v1/", Refusal::Host),
        ("http://plain.example.test./", Refusal::Host),
        ("http://shelfy-api:8080/health", Refusal::Port),
        ("http://plain.example.test:8080/", Refusal::Port),
        ("https://secure.example.test:8443/", Refusal::Port),
        ("http://plain.example.test:22/", Refusal::Port),
        ("http://plain.example.test:9464/metrics", Refusal::Port),
        ("ftp://plain.example.test/", Refusal::Scheme),
        ("file:///etc/passwd", Refusal::Scheme),
        ("gopher://plain.example.test/", Refusal::Scheme),
        ("javascript:alert(1)", Refusal::Scheme),
        ("data:text/plain,hi", Refusal::Scheme),
        ("http://user:pw@plain.example.test/", Refusal::Credentials),
        ("http://user@plain.example.test/", Refusal::Credentials),
    ] {
        assert_eq!(refusal(link.get(url).send().await), expected, "{url}");
    }
    let invalid = link.get("not a url").send().await;
    assert!(
        matches!(invalid, Err(EgressError::InvalidUrl)),
        "{invalid:?}"
    );
    // The CDN purpose takes https only.
    let cdn = outbound.client(Purpose::Cdn);
    let plain = cdn.get("http://plain.example.test/x.jpg").send().await;
    assert_eq!(refusal(plain), Refusal::Scheme);
    assert!(fixture.hits().is_empty());
}

#[tokio::test]
async fn names_with_only_private_addresses_are_refused() {
    let (fixture, outbound) = direct(|_| {}).await;
    let link = outbound.client(Purpose::Link);
    for host in [
        "rebind.example.test",
        "metadata.example.test",
        "cgnat.example.test",
        "ula.example.test",
        "mapped.example.test",
    ] {
        let url = format!("http://{host}/");
        assert_eq!(
            refusal(link.get(&url).send().await),
            Refusal::Address,
            "{host}"
        );
    }
    // A name that does not resolve is a connection failure, not a refusal.
    let missing = link.get("http://missing.example.test/").send().await;
    assert!(
        matches!(missing, Err(EgressError::Connect(_))),
        "{missing:?}"
    );
    assert!(fixture.hits().is_empty());
}

#[tokio::test]
async fn the_fixture_answers_over_http_and_https() {
    let (fixture, outbound) = direct(|_| {}).await;
    fixture.route(PLAIN, "/hello", [Answer::text(200, "plain")]);
    fixture.route(SECURE, "/hello", [Answer::text(200, "secure")]);
    let link = outbound.client(Purpose::Link);
    assert_eq!(
        text(link.get("http://plain.example.test/hello").send().await).await,
        "plain"
    );
    assert_eq!(
        text(link.get("https://secure.example.test/hello").send().await).await,
        "secure"
    );
    let hits = fixture.hits();
    assert_eq!(hits.len(), 2);
    assert!(!hits[0].tls && hits[1].tls);
    let user_agent = hits[0].header("user-agent").unwrap();
    assert!(user_agent.starts_with("shelfy-server/"), "{user_agent}");
}

#[tokio::test]
async fn redirects_are_followed_up_to_five_hops_and_rechecked() {
    let (fixture, outbound) = direct(|_| {}).await;
    let link = outbound.client(Purpose::Link);
    fixture.route(
        PLAIN,
        "/relative",
        [Answer::redirect(302, "/landing?x=1#frag")],
    );
    fixture.route(PLAIN, "/landing?x=1", [Answer::text(200, "landed")]);
    let response = link
        .get("http://plain.example.test/relative")
        .send()
        .await
        .unwrap();
    assert_eq!(response.redirects(), 1);
    assert_eq!(
        response.url().as_str(),
        "http://plain.example.test/landing?x=1"
    );
    assert_eq!(response.text_capped(100).await.unwrap(), "landed");

    for i in 0..6 {
        let next = format!("/hop/{}", i + 1);
        fixture.route(PLAIN, &format!("/hop/{i}"), [Answer::redirect(302, &next)]);
    }
    fixture.route(PLAIN, "/hop/6", [Answer::text(200, "end")]);
    let five = link
        .get("http://plain.example.test/hop/1")
        .send()
        .await
        .unwrap();
    assert_eq!(five.redirects(), 5);
    assert_eq!(five.text_capped(100).await.unwrap(), "end");
    let six = link.get("http://plain.example.test/hop/0").send().await;
    assert!(
        matches!(six, Err(EgressError::TooManyRedirects(5))),
        "{six:?}"
    );
    assert_eq!(
        fixture.hits_of(PLAIN, "/hop/6").len(),
        1,
        "the hop past the fifth was not requested"
    );
    // Fewer hops on request.
    let capped = link
        .get("http://plain.example.test/hop/4")
        .max_redirects(1)
        .send()
        .await;
    assert!(
        matches!(capped, Err(EgressError::TooManyRedirects(1))),
        "{capped:?}"
    );
    // At 0 the redirect is the response.
    let zero = link
        .get("http://plain.example.test/hop/5")
        .max_redirects(0)
        .send()
        .await
        .unwrap();
    assert_eq!(zero.status(), 302);

    let refused_targets = [
        ("/to/file", "file:///etc/passwd", Refusal::Scheme),
        (
            "/to/port",
            "http://plain.example.test:8080/x",
            Refusal::Port,
        ),
        ("/to/loopback", "http://127.0.0.1/secret", Refusal::Address),
        (
            "/to/mapped",
            "http://[::ffff:10.0.0.1]/secret",
            Refusal::Address,
        ),
        (
            "/to/metadata",
            "http://169.254.169.254/latest/meta-data/",
            Refusal::Address,
        ),
        (
            "/to/rebind",
            "http://rebind.example.test/secret",
            Refusal::Address,
        ),
        ("/to/localhost", "http://localhost/secret", Refusal::Host),
        (
            "/to/credentials",
            "http://u:p@other.example.test/secret",
            Refusal::Credentials,
        ),
    ];
    for (from, to, expected) in refused_targets {
        fixture.route(PLAIN, from, [Answer::redirect(307, to)]);
        let url = format!("http://plain.example.test{from}");
        assert_eq!(
            refusal(link.get(&url).send().await),
            expected,
            "{from} → {to}"
        );
    }
    // A host allowlist holds on every hop.
    fixture.route(
        PLAIN,
        "/to/other",
        [Answer::redirect(302, "http://other.example.test/x")],
    );
    let hosts = Arc::new(HostSet::new([PLAIN]));
    let off_list = link
        .get("http://plain.example.test/to/other")
        .hosts(hosts)
        .send()
        .await;
    assert_eq!(refusal(off_list), Refusal::Host);
    // https stays https when the request says so.
    fixture.route(
        SECURE,
        "/downgrade",
        [Answer::redirect(302, "http://plain.example.test/x")],
    );
    let downgrade = link
        .get("https://secure.example.test/downgrade")
        .https_only()
        .send()
        .await;
    assert_eq!(refusal(downgrade), Refusal::Scheme);
    let secret_hits = fixture
        .hits()
        .into_iter()
        .filter(|hit| hit.target.contains("secret") || hit.host == OTHER)
        .count();
    assert_eq!(secret_hits, 0, "no refused target was requested");
}

#[tokio::test]
async fn redirects_change_methods_and_drop_credentials_across_origins() {
    let (fixture, outbound) = direct(|_| {}).await;
    let link = outbound.client(Purpose::Link);
    fixture.route(PLAIN, "/post/303", [Answer::redirect(303, "/landing")]);
    fixture.route(PLAIN, "/post/302", [Answer::redirect(302, "/landing")]);
    fixture.route(PLAIN, "/post/307", [Answer::redirect(307, "/landing")]);
    fixture.route(PLAIN, "/landing", [Answer::text(200, "landed")]);
    for (from, method, body) in [
        ("/post/303", "GET", ""),
        ("/post/302", "GET", ""),
        ("/post/307", "POST", "{\"a\":1}"),
    ] {
        let url = format!("http://plain.example.test{from}");
        let response = link
            .post(&url)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .body("{\"a\":1}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let landing = fixture.hits_of(PLAIN, "/landing").pop().unwrap();
        assert_eq!(landing.method, method, "{from}");
        assert_eq!(String::from_utf8_lossy(&landing.body), body, "{from}");
        assert_eq!(
            landing.header("content-type").is_some(),
            method == "POST",
            "{from}"
        );
    }

    fixture.route(
        PLAIN,
        "/auth/same",
        [Answer::redirect(302, "/auth/landing")],
    );
    fixture.route(
        PLAIN,
        "/auth/cross",
        [Answer::redirect(
            302,
            "http://other.example.test/auth/landing",
        )],
    );
    fixture.route(PLAIN, "/auth/landing", [Answer::text(200, "ok")]);
    fixture.route(OTHER, "/auth/landing", [Answer::text(200, "ok")]);
    for from in ["/auth/same", "/auth/cross"] {
        let url = format!("http://plain.example.test{from}");
        link.request(Method::GET, &url)
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_static("Bearer planted-key"),
            )
            .header(
                header::HeaderName::from_static("x-api-key"),
                HeaderValue::from_static("planted"),
            )
            .header(header::ACCEPT, HeaderValue::from_static("application/json"))
            .send()
            .await
            .unwrap();
    }
    let same = fixture.hits_of(PLAIN, "/auth/landing").pop().unwrap();
    assert_eq!(same.header("authorization"), Some("Bearer planted-key"));
    assert_eq!(same.header("x-api-key"), Some("planted"));
    let cross = fixture.hits_of(OTHER, "/auth/landing").pop().unwrap();
    assert_eq!(cross.header("authorization"), None);
    assert_eq!(cross.header("x-api-key"), None);
    assert_eq!(cross.header("accept"), Some("application/json"));
}

#[tokio::test]
async fn bodies_are_read_with_a_cap() {
    let (fixture, outbound) = direct(|_| {}).await;
    let link = outbound.client(Purpose::Link);
    fixture.route(
        PLAIN,
        "/2000",
        [Answer::new(200, "application/octet-stream", vec![7; 2000])],
    );
    fixture.route(
        PLAIN,
        "/chunked",
        [Answer::large(
            "application/octet-stream",
            vec![1, 2, 3],
            2000,
            true,
        )],
    );
    fixture.route(
        PLAIN,
        "/json",
        [Answer::new(200, "application/json", "{\"n\":42}")],
    );
    let get = |path: &str| link.get(&format!("http://plain.example.test{path}")).send();

    let declared = get("/2000").await.unwrap().read_capped(1999).await;
    assert!(
        matches!(declared, Err(EgressError::TooLarge { limit: 1999 })),
        "{declared:?}"
    );
    let whole = get("/2000").await.unwrap().read_capped(2000).await.unwrap();
    assert_eq!(whole.len(), 2000);
    let streamed = get("/chunked").await.unwrap().read_capped(1000).await;
    assert!(
        matches!(streamed, Err(EgressError::TooLarge { limit: 1000 })),
        "{streamed:?}"
    );
    let all = get("/chunked")
        .await
        .unwrap()
        .read_capped(2000)
        .await
        .unwrap();
    assert_eq!((all.len(), &all[..3]), (2000, &[1, 2, 3][..]));

    let mut stream = get("/chunked").await.unwrap().stream_capped(1500);
    let mut read = 0;
    let mut failed = false;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => read += chunk.len(),
            Err(EgressError::TooLarge { limit: 1500 }) => failed = true,
            Err(other) => panic!("{other:?}"),
        }
    }
    assert!(failed && read <= 1500, "{read}");

    let head = get("/2000").await.unwrap().read_prefix(10).await.unwrap();
    assert_eq!(head.len(), 10);
    #[derive(serde::Deserialize)]
    struct Body {
        n: u32,
    }
    let body: Body = get("/json").await.unwrap().json_capped(100).await.unwrap();
    assert_eq!(body.n, 42);
    let bad: Result<Body, _> = get("/2000").await.unwrap().json_capped(5000).await;
    assert!(matches!(bad, Err(EgressError::Decode(_))));
}

#[tokio::test]
async fn no_cookie_is_kept_and_no_body_is_decompressed() {
    let (fixture, outbound) = direct(|_| {}).await;
    let link = outbound.client(Purpose::Link);
    fixture.route(
        PLAIN,
        "/cookie/set",
        [Answer::text(200, "set").with_header("set-cookie", "session=planted; Path=/")],
    );
    fixture.route(PLAIN, "/cookie/check", [Answer::text(200, "checked")]);
    fixture.route(
        PLAIN,
        "/gzip",
        [Answer::new(200, "text/plain", GZIP_HELLO).with_header("content-encoding", "gzip")],
    );
    text(
        link.get("http://plain.example.test/cookie/set")
            .send()
            .await,
    )
    .await;
    text(
        link.get("http://plain.example.test/cookie/check")
            .send()
            .await,
    )
    .await;
    let check = fixture.hits_of(PLAIN, "/cookie/check").pop().unwrap();
    assert_eq!(check.header("cookie"), None);

    let response = link
        .get("http://plain.example.test/gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    let raw = response.read_capped(1000).await.unwrap();
    assert_eq!(&raw[..], GZIP_HELLO, "the bytes as sent");
    for hit in fixture.hits() {
        assert_eq!(hit.header("accept-encoding"), None, "{}", hit.target);
    }
}

#[tokio::test]
async fn at_most_the_configured_requests_are_in_flight() {
    let (fixture, outbound) = direct(|config| config.max_in_flight = 2).await;
    fixture.route(
        PLAIN,
        "/slow",
        [Answer::text(200, "slow").delayed(Duration::from_millis(200))],
    );
    let link = outbound.client(Purpose::Link);
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let link = link.clone();
            tokio::spawn(async move {
                let response = link
                    .get("http://plain.example.test/slow")
                    .send()
                    .await
                    .unwrap();
                response.read_capped(100).await.unwrap()
            })
        })
        .collect();
    for task in tasks {
        assert_eq!(&task.await.unwrap()[..], b"slow");
    }
    assert_eq!(fixture.hits().len(), 6);
    assert_eq!(fixture.peak_in_flight(), 2);
}

#[tokio::test]
async fn a_hanging_destination_times_out() {
    let (fixture, outbound) = direct(|_| {}).await;
    fixture.route(PLAIN, "/hang", [Answer::Hang]);
    let started = Instant::now();
    let result = outbound
        .client(Purpose::Link)
        .get("http://plain.example.test/hang")
        .timeout(Duration::from_millis(300))
        .send()
        .await;
    assert!(matches!(result, Err(EgressError::Timeout)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The outbound client with `node` as the operator's allowlisted origin.
fn sleeping_operator(node: &SleepingNode, edit: impl FnOnce(&mut OutboundConfig)) -> Outbound {
    let mut config = OutboundConfig {
        allow_origins: OriginAllowlist::parse(&node.origin()).unwrap(),
        ..OutboundConfig::default()
    };
    edit(&mut config);
    Outbound::new(&config).unwrap()
}

#[tokio::test]
async fn a_connect_timeout_is_a_connect_failure_after_the_purposes_timeout() {
    let node = SleepingNode::start();
    let url = format!("{}/health", node.origin());

    // The operator's node: 3 s by default, then a connect failure, not a
    // timeout (F15).
    let outbound = sleeping_operator(&node, |_| {});
    let started = Instant::now();
    let result = outbound.client(Purpose::AiOperator).get(&url).send().await;
    let elapsed = started.elapsed();
    let error = result.unwrap_err();
    assert!(matches!(error, EgressError::Connect(_)), "{error:?}");
    assert!(error.is_connect_timeout(), "{error:?}");
    assert!(
        elapsed >= Duration::from_millis(2_900) && elapsed < Duration::from_secs(6),
        "{elapsed:?}"
    );

    // An override per purpose.
    let outbound = sleeping_operator(&node, |config| {
        config.connect_timeouts =
            HashMap::from([(Purpose::AiOperator, Duration::from_millis(300))]);
    });
    let started = Instant::now();
    let error = outbound
        .client(Purpose::AiOperator)
        .get(&url)
        .send()
        .await
        .unwrap_err();
    assert!(error.is_connect_timeout(), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(2));

    // The request's own timeout, shorter than connecting, stays a timeout.
    let started = Instant::now();
    let result = sleeping_operator(&node, |_| {})
        .client(Purpose::AiOperator)
        .get(&url)
        .timeout(Duration::from_millis(300))
        .send()
        .await;
    assert!(matches!(result, Err(EgressError::Timeout)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(2));

    // A refused connection is a connect failure, not a connect timeout.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let outbound = Outbound::new(&OutboundConfig {
        allow_origins: OriginAllowlist::parse(&origin).unwrap(),
        ..OutboundConfig::default()
    })
    .unwrap();
    let error = outbound
        .client(Purpose::AiOperator)
        .get(&format!("{origin}/health"))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, EgressError::Connect(_)), "{error:?}");
    assert!(!error.is_connect_timeout(), "{error:?}");
}

#[tokio::test]
async fn a_connect_timeout_is_counted_as_a_timeout() {
    let handle = metrics::install();
    let node = SleepingNode::start();
    let outbound = sleeping_operator(&node, |config| {
        config.connect_timeouts =
            HashMap::from([(Purpose::AiOperator, Duration::from_millis(200))]);
    });
    let error = outbound
        .client(Purpose::AiOperator)
        .get(&format!("{}/health", node.origin()))
        .send()
        .await
        .unwrap_err();
    assert!(error.is_connect_timeout(), "{error:?}");
    handle.run_upkeep();
    let text = handle.render();
    let series = "shelfy_egress_requests_total{purpose=\"ai_operator\",outcome=\"timeout\"}";
    assert!(
        text.lines().any(|line| line.starts_with(series)),
        "no {series} in\n{text}"
    );
}

#[test]
fn a_connect_timeout_must_be_positive() {
    let config = OutboundConfig {
        connect_timeouts: HashMap::from([(Purpose::Link, Duration::ZERO)]),
        ..OutboundConfig::default()
    };
    assert!(Outbound::new(&config).is_err());
}

#[tokio::test]
async fn only_the_test_loopback_handle_reaches_a_loopback_port() {
    let (fixture, outbound) = direct(|_| {}).await;
    let port = fixture.http_addr().port();
    fixture.route("127.0.0.1", "/v1/models", [Answer::text(200, "stub")]);
    let url = format!("http://127.0.0.1:{port}/v1/models");

    // The real handle: user AI providers stay on ports 80 and 443 and on
    // public addresses (F15 keeps them).
    let ai = outbound.client(Purpose::Ai);
    assert_eq!(refusal(ai.get(&url).send().await), Refusal::Port);
    assert_eq!(
        refusal(ai.get("http://127.0.0.1/v1/models").send().await),
        Refusal::Address
    );

    // The test-only handle reaches the loopback port, under `Purpose::Ai`.
    let loopback = outbound.client(Purpose::Ai).loopback_for_tests();
    assert_eq!(loopback.purpose(), Purpose::Ai);
    assert_eq!(text(loopback.get(&url).send().await).await, "stub");

    // Everything else keeps the strict rules.
    for (url, expected) in [
        ("http://10.0.0.1/v1/models".to_owned(), Refusal::Address),
        ("http://169.254.169.254/".to_owned(), Refusal::Address),
        (format!("http://localhost:{port}/v1/models"), Refusal::Port),
        ("http://localhost/v1/models".to_owned(), Refusal::Host),
        (
            format!("http://user:pw@127.0.0.1:{port}/"),
            Refusal::Credentials,
        ),
        (
            "https://rebind.example.test/v1".to_owned(),
            Refusal::Address,
        ),
        (format!("http://plain.example.test:{port}/"), Refusal::Port),
    ] {
        assert_eq!(refusal(loopback.get(&url).send().await), expected, "{url}");
    }
}

#[tokio::test]
async fn errors_carry_no_url() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let (_fixture, outbound) = direct(|config| {
        config.dev_hosts.insert(
            "down.example.test".to_owned(),
            ([127, 0, 0, 1], port).into(),
        );
    })
    .await;
    let err = outbound
        .client(Purpose::Link)
        .get("http://down.example.test/planted-path?token=planted-token")
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, EgressError::Connect(_)), "{err:?}");
    let mut chain = format!("{err:?}");
    let mut source: Option<&dyn std::error::Error> = Some(&err);
    while let Some(err) = source {
        chain.push_str(&err.to_string());
        source = err.source();
    }
    assert!(!chain.contains("planted"), "{chain}");
}

fn proxied(fixture: &FixtureCdn, stub: &ProxyStub) -> OutboundConfig {
    OutboundConfig {
        proxy: Some(Url::parse(&stub.url()).unwrap()),
        extra_roots: vec![fixture.ca()],
        lookup: Lookup::fixed::<_, &str>([]),
        ..OutboundConfig::default()
    }
}

#[tokio::test]
async fn the_proxy_sees_every_request_and_every_hop() {
    let fixture = FixtureCdn::start().await;
    let stub = ProxyStub::start(&fixture).await;
    let outbound = Outbound::new(&proxied(&fixture, &stub)).unwrap();
    let link = outbound.client(Purpose::Link);
    fixture.route("hop.example.test", "/a", [Answer::redirect(302, "/b")]);
    fixture.route(
        "hop.example.test",
        "/b",
        [Answer::redirect(301, "http://hop2.example.test/c")],
    );
    fixture.route("hop2.example.test", "/c", [Answer::text(200, "end")]);
    assert_eq!(
        text(link.get("http://hop.example.test/a").send().await).await,
        "end"
    );
    assert_eq!(
        stub.requests(),
        [
            "GET http://hop.example.test/a",
            "GET http://hop.example.test/b",
            "GET http://hop2.example.test/c",
        ]
    );
    // https goes through a CONNECT tunnel, then TLS with the destination.
    fixture.route("tls.example.test", "/x", [Answer::text(200, "tunneled")]);
    assert_eq!(
        text(link.get("https://tls.example.test/x").send().await).await,
        "tunneled"
    );
    assert!(
        stub.requests()
            .contains(&"CONNECT tls.example.test:443".to_owned())
    );
    assert!(fixture.hits_of("tls.example.test", "/x")[0].tls);
}

#[tokio::test]
async fn the_proxy_mode_keeps_every_check() {
    let fixture = FixtureCdn::start().await;
    let stub = ProxyStub::start(&fixture).await;
    let outbound = Outbound::new(&proxied(&fixture, &stub)).unwrap();
    let link = outbound.client(Purpose::Link);

    // The proxy's refusals.
    stub.deny("denied.example.test");
    let tunnel = link.get("https://denied.example.test/x").send().await;
    assert_eq!(refusal(tunnel), Refusal::Proxy);
    let plain = link.get("http://denied.example.test/x").send().await;
    assert_eq!(refusal(plain), Refusal::Proxy);

    // Our own checks still run first.
    let before = stub.requests().len();
    assert_eq!(
        refusal(link.get("http://127.0.0.1/").send().await),
        Refusal::Address
    );
    assert_eq!(
        refusal(link.get("http://localhost/").send().await),
        Refusal::Host
    );
    assert_eq!(
        refusal(link.get("http://x.example.test:8080/").send().await),
        Refusal::Port
    );
    assert_eq!(stub.requests().len(), before, "refused before the proxy");

    // Redirects: file:, another port, and past the fifth hop.
    fixture.route(
        "r.example.test",
        "/file",
        [Answer::redirect(302, "file:///etc/passwd")],
    );
    fixture.route(
        "r.example.test",
        "/port",
        [Answer::redirect(302, "http://r.example.test:8080/")],
    );
    assert_eq!(
        refusal(link.get("http://r.example.test/file").send().await),
        Refusal::Scheme
    );
    assert_eq!(
        refusal(link.get("http://r.example.test/port").send().await),
        Refusal::Port
    );
    for i in 0..6 {
        let next = format!("/hop/{}", i + 1);
        fixture.route(
            "r.example.test",
            &format!("/hop/{i}"),
            [Answer::redirect(302, &next)],
        );
    }
    let six = link.get("http://r.example.test/hop/0").send().await;
    assert!(
        matches!(six, Err(EgressError::TooManyRedirects(5))),
        "{six:?}"
    );
    assert!(
        !stub
            .requests()
            .iter()
            .any(|r| r.ends_with("/hop/6") || r.contains(":8080"))
    );

    // A capped body stops at the cap.
    fixture.route(
        "r.example.test",
        "/big",
        [Answer::large("text/plain", Vec::new(), 4096, true)],
    );
    let capped = link
        .get("http://r.example.test/big")
        .send()
        .await
        .unwrap()
        .read_capped(100)
        .await;
    assert!(
        matches!(capped, Err(EgressError::TooLarge { limit: 100 })),
        "{capped:?}"
    );
    assert_eq!(outbound.mode(), shelfy_server::outbound::Mode::Proxy);
}

#[tokio::test]
async fn the_operator_allowlist_is_exact_and_for_the_operator_only() {
    let fixture = FixtureCdn::start().await;
    let port = fixture.http_addr().port();
    let other_port = fixture.https_addr().port();
    let node = format!("http://127.0.0.1:{port}");
    let named = format!("http://ornith.tailnet.test:{port}");
    let config = OutboundConfig {
        allow_origins: OriginAllowlist::parse(&format!("{node}, {named}")).unwrap(),
        lookup: Lookup::fixed([("ornith.tailnet.test", vec![ip("127.0.0.1")])]),
        ..OutboundConfig::default()
    };
    let outbound = Outbound::new(&config).unwrap();
    let models = Answer::new(200, "application/json", "{\"data\":[]}");
    fixture.route("127.0.0.1", "/v1/models", [models.clone()]);
    fixture.route("ornith.tailnet.test", "/v1/models", [models]);
    let operator = outbound.client(Purpose::AiOperator);

    // The allowlisted origins pass, at a private address.
    for origin in [&node, &named] {
        let url = format!("{origin}/v1/models");
        let response = operator.get(&url).send().await.unwrap();
        assert_eq!(response.status(), 200, "{url}");
    }
    // The same address on another port, another private address, another
    // scheme: the strict rules.
    for (url, expected) in [
        (
            format!("http://127.0.0.1:{other_port}/v1/models"),
            Refusal::Port,
        ),
        ("http://127.0.0.1/v1/models".to_owned(), Refusal::Address),
        (format!("http://127.0.0.2:{port}/v1/models"), Refusal::Port),
        ("http://127.0.0.2/v1/models".to_owned(), Refusal::Address),
        (
            "http://100.101.102.104/v1/models".to_owned(),
            Refusal::Address,
        ),
        ("http://10.0.0.1/v1/models".to_owned(), Refusal::Address),
        (format!("https://127.0.0.1:{port}/v1/models"), Refusal::Port),
        (
            format!("http://ornith.tailnet.test:{other_port}/"),
            Refusal::Port,
        ),
    ] {
        assert_eq!(refusal(operator.get(&url).send().await), expected, "{url}");
    }
    // User-configured providers never use the allowlist.
    for purpose in [Purpose::Ai, Purpose::Link, Purpose::Cdn, Purpose::Feedback] {
        let url = format!("{node}/v1/models");
        let refused = outbound.client(purpose).get(&url).send().await;
        assert!(
            matches!(refused, Err(EgressError::Refused(_))),
            "{purpose:?}"
        );
    }
    // A redirect from the node is checked like any other.
    fixture.route(
        "127.0.0.1",
        "/redirect",
        [Answer::redirect(302, "http://10.0.0.1/")],
    );
    let redirected = operator
        .get(&format!("{node}/redirect"))
        .max_redirects(5)
        .send()
        .await;
    assert_eq!(refusal(redirected), Refusal::Address);
    assert_eq!(
        fixture.hits().len(),
        3,
        "the two allowlisted calls and the redirect"
    );
}

#[tokio::test]
async fn the_operator_origins_and_the_capture_service_bypass_the_proxy() {
    let fixture = FixtureCdn::start().await;
    let stub = ProxyStub::start(&fixture).await;
    let port = fixture.http_addr().port();
    let node = format!("http://ornith.tailnet.test:{port}");
    let capture = format!("http://shelfy-capture:{port}");
    let config = OutboundConfig {
        allow_origins: OriginAllowlist::parse(&node).unwrap(),
        capture: Some(Origin::parse(&capture).unwrap()),
        lookup: Lookup::fixed([
            ("ornith.tailnet.test", vec![ip("127.0.0.1")]),
            ("shelfy-capture", vec![ip("127.0.0.1")]),
        ]),
        ..proxied(&fixture, &stub)
    };
    let outbound = Outbound::new(&config).unwrap();
    fixture.route(
        "ornith.tailnet.test",
        "/v1/models",
        [Answer::text(200, "node")],
    );
    fixture.route("shelfy-capture", "/health", [Answer::text(200, "capture")]);
    fixture.route("public.example.test", "/x", [Answer::text(200, "public")]);

    let operator = outbound.client(Purpose::AiOperator);
    assert_eq!(
        text(operator.get(&format!("{node}/v1/models")).send().await).await,
        "node"
    );
    let internal = outbound.internal().expect("capture is configured");
    assert_eq!(internal.purpose(), Purpose::Capture);
    assert_eq!(
        text(internal.get(&format!("{capture}/health")).send().await).await,
        "capture"
    );
    assert!(stub.requests().is_empty(), "{:?}", stub.requests());
    // Public requests, the operator's included, go through the proxy.
    assert_eq!(
        text(operator.get("http://public.example.test/x").send().await).await,
        "public"
    );
    assert_eq!(stub.requests(), ["GET http://public.example.test/x"]);

    // The internal client reaches the capture origin only.
    let other_port = fixture.https_addr().port();
    for url in [
        "http://public.example.test/x".to_owned(),
        format!("http://shelfy-capture:{other_port}/health"),
        format!("https://shelfy-capture:{port}/health"),
        format!("http://ornith.tailnet.test:{port}/v1/models"),
    ] {
        assert_eq!(
            refusal(internal.get(&url).send().await),
            Refusal::Origin,
            "{url}"
        );
    }
    // And no other purpose reaches it.
    let link = outbound
        .client(Purpose::Link)
        .get(&format!("{capture}/health"))
        .send()
        .await;
    assert_eq!(refusal(link), Refusal::Port);
}

#[tokio::test]
async fn without_a_capture_url_there_is_no_internal_client() {
    let outbound = Outbound::new(&OutboundConfig::default()).unwrap();
    assert!(outbound.internal().is_none());
    let capture = outbound
        .client(Purpose::Capture)
        .get("http://shelfy-capture:8080/health")
        .send()
        .await;
    assert_eq!(refusal(capture), Refusal::Origin);
    assert_eq!(outbound.mode(), shelfy_server::outbound::Mode::Direct);
    assert!(outbound.proxy_url().is_none());
}

#[tokio::test]
async fn requests_are_counted_by_purpose_and_outcome() {
    let handle = metrics::install();
    let (fixture, outbound) = direct(|_| {}).await;
    fixture.route(PLAIN, "/ok", [Answer::text(200, "ok")]);
    fixture.route(PLAIN, "/missing", [Answer::status(404)]);
    fixture.route(PLAIN, "/broken", [Answer::status(503)]);
    let link = outbound.client(Purpose::Link);
    for path in ["/ok", "/missing", "/broken"] {
        let url = format!("http://plain.example.test{path}");
        link.get(&url).send().await.unwrap();
    }
    let _ = link.get("http://127.0.0.1/").send().await;
    handle.run_upkeep();
    let text = handle.render();
    for outcome in ["ok", "client_error", "server_error", "refused"] {
        let series =
            format!("shelfy_egress_requests_total{{purpose=\"link\",outcome=\"{outcome}\"}}");
        let value: f64 = text
            .lines()
            .find_map(|line| line.strip_prefix(&series))
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or_else(|| panic!("no {series} in\n{text}"));
        assert!(value >= 1.0, "{series}");
    }
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files
}

/// The patterns of the single-construction rule, spelled so that this file
/// does not match them itself.
struct Rules {
    /// Any use of the crate.
    any: Regex,
    /// Building a client, or a helper that builds one.
    build: Regex,
    /// Another HTTP client crate in server or media code.
    other_client: Regex,
}

impl Rules {
    fn new() -> Self {
        let krate = ["req", "west"].concat();
        Self {
            any: Regex::new(&format!(r"\b{krate}\b")).unwrap(),
            build: Regex::new(&format!(
                r"\bClient\s*::\s*(builder|new|default)\s*\(|\bClientBuilder\b|\b{krate}\s*::\s*(get|blocking)\b"
            ))
            .unwrap(),
            other_client: Regex::new(&[
                r"\b(", "ureq", "|", "isahc", "|", "surf", "|", "attohttpc", "|", "curl",
                r")\s*::|\bhyper(_util)?\s*::\s*client\b",
            ]
            .concat())
            .unwrap(),
        }
    }
}

#[test]
fn no_reqwest_client_is_built_outside_the_outbound_client() {
    let server = Path::new(env!("CARGO_MANIFEST_DIR"));
    let media = server.join("../media");
    let outbound_dir = server.join("src/outbound");
    let the_client = outbound_dir.join("client.rs");
    let rules = Rules::new();

    // The rules catch what they are for.
    let krate = ["req", "west"].concat();
    for bad in [
        format!("let c = {krate}::Client::builder().build();"),
        format!("let c = {krate}::Client::new();"),
        format!("use {krate}::ClientBuilder;"),
        format!("let r = {krate}::get(url).await;"),
    ] {
        assert!(
            rules.build.is_match(&bad) && rules.any.is_match(&bad),
            "{bad}"
        );
    }
    assert!(
        rules
            .other_client
            .is_match(&["let a = ", "ureq", "::agent();"].concat())
    );
    assert!(
        rules
            .other_client
            .is_match(&["hyper_util", "::client::legacy::Client"].concat())
    );

    let mut violations = Vec::new();
    let sources = [
        (server.join("src"), true),
        (server.join("tests"), false),
        (server.join("examples"), false),
        (media.join("src"), true),
        (media.join("tests"), false),
        (media.join("examples"), false),
    ];
    for (dir, production) in sources {
        for file in rust_files(&dir) {
            if file == the_client {
                continue;
            }
            let text = fs::read_to_string(&file).unwrap();
            let in_outbound = file.starts_with(&outbound_dir);
            if !in_outbound && rules.any.is_match(&text) {
                violations.push(format!("{}: uses the HTTP client crate", file.display()));
            }
            if rules.build.is_match(&text) && (in_outbound || rules.any.is_match(&text)) {
                violations.push(format!("{}: builds an HTTP client", file.display()));
            }
            if production && rules.other_client.is_match(&text) {
                violations.push(format!("{}: uses another HTTP client", file.display()));
            }
        }
    }
    let media_manifest = fs::read_to_string(media.join("Cargo.toml")).unwrap();
    if rules.any.is_match(&media_manifest) {
        violations.push("crates/media/Cargo.toml depends on the HTTP client crate".to_owned());
    }
    assert!(
        violations.is_empty(),
        "only crates/server/src/outbound/client.rs builds outbound HTTP clients:\n{}",
        violations.join("\n")
    );
    // And the client itself does build them there.
    let client = fs::read_to_string(&the_client).unwrap();
    assert!(rules.build.is_match(&client));
}
