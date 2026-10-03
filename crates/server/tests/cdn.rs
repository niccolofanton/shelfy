//! The archive's CDN fetcher (P2-04) against the fixture CDN: every
//! outcome, the browser-like request, `oe` expiry without a request, the
//! URL variants, redirects to private addresses, the 15 MB cap, the host
//! limits and the breaker, and their metrics.
//!
//! The breaker gauges are process-wide: the tests of this binary take
//! turns ([`TURN`]).

mod support;

use std::net::IpAddr;
use std::time::Duration;

use shelfy_media::MediaKind;
use shelfy_media::store::{MediaStore, UserMedia};
use shelfy_server::outbound::{
    BreakerConfig, BreakerState, FetchOutcome, FetchRequest, HostGroup, Outbound, OutboundConfig,
    Rejection,
};
use shelfy_server::telemetry::metrics;
use support::cdn::{Answer, FixtureCdn, jpeg};
use tempfile::TempDir;
use tokio::time::Instant;

/// One test at a time: the breaker gauges are shared.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const USER: &str = "01J9Z3B8K4QW6TFX0V7G2N5RCA";
/// 2026-10-02T00:00:00Z, the fetches' clock.
const NOW_MS: i64 = 1_790_899_200_000;
const IG: &str = "scontent-mxp1-1.cdninstagram.com";
const IG_OTHER: &str = "scontent-fco2-1.cdninstagram.com";
const IG_EVIL: &str = "scontent-evil.cdninstagram.com";
const X: &str = "pbs.twimg.com";
const PIN: &str = "i.pinimg.com";
const MIB: u64 = 1024 * 1024;

struct Bench {
    fixture: FixtureCdn,
    outbound: Outbound,
    _dir: TempDir,
    media: UserMedia,
}

/// The fixture with the CDN hosts on its https listener, `IG_EVIL`
/// resolving to the loopback, no jitter and 100 requests a second, then
/// `edit`.
async fn bench(edit: impl FnOnce(&mut OutboundConfig)) -> Bench {
    let fixture = FixtureCdn::start().await;
    let loopback: &[IpAddr] = &["127.0.0.1".parse().unwrap()];
    let mut config = fixture.config(&[IG, IG_OTHER, X, PIN], &[], &[(IG_EVIL, loopback)]);
    for group in HostGroup::ALL {
        let limits = config.limits.get_mut(group);
        limits.rate = 100.0;
        limits.jitter = None;
        limits.spread = Duration::ZERO;
    }
    edit(&mut config);
    let outbound = Outbound::new(&config).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let media = MediaStore::new(dir.path()).user(USER).unwrap();
    Bench {
        fixture,
        outbound,
        _dir: dir,
        media,
    }
}

/// A bench whose breakers never open, for tests about other things.
async fn calm_bench(edit: impl FnOnce(&mut OutboundConfig)) -> Bench {
    bench(|config| {
        config.breaker = BreakerConfig {
            window: 1000,
            open_at_blocked: 1000,
            open_at_streak: 1000,
            open_for: Duration::from_secs(1),
        };
        edit(config);
    })
    .await
}

impl Bench {
    async fn fetch(&self, url: &str) -> FetchOutcome {
        self.fetch_expiring(url, None).await
    }

    async fn fetch_expiring(&self, url: &str, expires_at_ms: Option<i64>) -> FetchOutcome {
        self.outbound
            .cdn()
            .fetch(FetchRequest {
                url,
                media: &self.media,
                expires_at_ms,
                now_ms: NOW_MS,
            })
            .await
    }

    /// Files left in the store's temporary directory.
    fn temp_files(&self) -> usize {
        std::fs::read_dir(self.media.root().join(".tmp")).map_or(0, |dir| dir.count())
    }
}

/// An Instagram URL whose `oe` is `seconds` from [`NOW_MS`].
fn ig_url(path: &str, seconds: i64) -> String {
    format!(
        "https://{IG}{path}?stp=dst-jpg_e35&_nc_ht={IG}&oe={:X}&oh=00_x",
        NOW_MS / 1000 + seconds
    )
}

/// The path and query of `url`, as the fixture logs it.
fn target(url: &str) -> String {
    let url = url::Url::parse(url).unwrap();
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    }
}

fn stored(outcome: FetchOutcome) -> shelfy_media::store::StagedObject {
    match outcome {
        FetchOutcome::Stored(object) => object,
        other => panic!("not stored: {other:?}"),
    }
}

#[tokio::test]
async fn an_image_is_stored_from_a_browser_like_request() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|_| {}).await;
    let url = ig_url("/v/t51.2885-15/cover_n.jpg", 3600);
    b.fixture.route(IG, &target(&url), [Answer::jpeg(5_000)]);
    let object = stored(b.fetch(&url).await);
    assert_eq!(object.kind(), MediaKind::Jpeg);
    assert_eq!(object.size(), 5_000);
    assert!(object.path().starts_with(b.media.root()));

    let hits = b.fixture.hits();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert!(hit.tls, "https only");
    assert_eq!(hit.method, "GET");
    let user_agent = hit.header("user-agent").unwrap();
    assert!(user_agent.contains("Chrome/141.0.0.0") && user_agent.starts_with("Mozilla/5.0"));
    assert!(
        hit.header("accept")
            .unwrap()
            .starts_with("image/avif,image/webp")
    );
    assert_eq!(hit.header("accept-language"), Some("en-US,en;q=0.9"));
    assert_eq!(hit.header("referer"), Some("https://www.instagram.com/"));
    assert_eq!(hit.header("sec-fetch-dest"), Some("image"));
    assert_eq!(hit.header("sec-fetch-mode"), Some("no-cors"));
    assert_eq!(hit.header("sec-fetch-site"), Some("cross-site"));
    for absent in ["cookie", "authorization", "accept-encoding"] {
        assert_eq!(hit.header(absent), None, "{absent}");
    }
    // The other groups send their platform's Referer.
    b.fixture
        .route(X, "/media/abc?format=jpg&name=large", [Answer::jpeg(100)]);
    b.fixture
        .route(PIN, "/1200x/ab/cd/ef/abc.jpg", [Answer::jpeg(100)]);
    stored(b.fetch("https://pbs.twimg.com/media/abc.jpg").await);
    stored(b.fetch("https://i.pinimg.com/736x/ab/cd/ef/abc.jpg").await);
    let referers: Vec<String> = b.fixture.hits()[1..]
        .iter()
        .map(|hit| hit.header("referer").unwrap().to_owned())
        .collect();
    assert_eq!(referers, ["https://x.com/", "https://www.pinterest.com/"]);
}

#[tokio::test]
async fn an_expired_url_is_never_fetched() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|_| {}).await;
    let expired = ig_url("/v/old_n.jpg", -60);
    b.fixture.route(IG, &target(&expired), [Answer::jpeg(100)]);
    assert!(matches!(b.fetch(&expired).await, FetchOutcome::Expired));
    let now = ig_url("/v/old_n.jpg", 0);
    assert!(
        matches!(b.fetch(&now).await, FetchOutcome::Expired),
        "oe == now"
    );
    // The caller's expiry wins, for any group.
    let x = "https://pbs.twimg.com/media/abc.jpg";
    let known = b.fetch_expiring(x, Some(NOW_MS - 1)).await;
    assert!(matches!(known, FetchOutcome::Expired));
    assert!(b.fixture.hits().is_empty(), "no request for an expired URL");

    // Still valid: fetched.
    let fresh = ig_url("/v/fresh_n.jpg", 120);
    b.fixture.route(IG, &target(&fresh), [Answer::jpeg(100)]);
    stored(b.fetch(&fresh).await);
    assert_eq!(b.fixture.hits().len(), 1);
}

#[tokio::test]
async fn every_answer_has_its_outcome() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|config| config.cdn_timeout = Duration::from_millis(400)).await;
    let ok = ig_url("/v/x_n.jpg", 3600);
    let at = |path: &str| ig_url(path, 3600);
    let cases: Vec<(&str, Answer)> = vec![
        ("/gone404", Answer::status(404)),
        ("/gone410", Answer::status(410)),
        ("/forbidden", Answer::text(403, "Forbidden")),
        ("/expired", Answer::text(403, "URL signature expired")),
        ("/timestamp", Answer::text(403, "Bad URL timestamp")),
        (
            "/limited",
            Answer::status(429).with_header("retry-after", "120"),
        ),
        (
            "/unavailable",
            Answer::status(503).with_header("retry-after", "30"),
        ),
        ("/broken", Answer::status(500)),
        ("/closed", Answer::Close),
        ("/hang", Answer::Hang),
        (
            "/challenge",
            Answer::new(
                200,
                "text/html",
                "<!doctype html><title>Just a moment</title>",
            ),
        ),
        (
            "/garbage",
            Answer::new(200, "image/jpeg", "definitely not an image"),
        ),
        (
            "/video",
            Answer::new(
                200,
                "video/mp4",
                b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2".to_vec(),
            ),
        ),
        ("/empty", Answer::new(200, "image/jpeg", Vec::new())),
        ("/bad", Answer::status(400)),
    ];
    for (path, answer) in &cases {
        b.fixture.route(IG, &target(&at(path)), [answer.clone()]);
    }
    let outcome = |path: &'static str| {
        let b = &b;
        let url = at(path);
        async move { b.fetch(&url).await }
    };
    assert!(matches!(outcome("/gone404").await, FetchOutcome::Gone));
    assert!(matches!(outcome("/gone410").await, FetchOutcome::Gone));
    assert!(matches!(
        outcome("/forbidden").await,
        FetchOutcome::Blocked {
            status: 403,
            retry_after: None
        }
    ));
    assert!(matches!(outcome("/expired").await, FetchOutcome::Expired));
    assert!(matches!(outcome("/timestamp").await, FetchOutcome::Expired));
    let limited = outcome("/limited").await;
    assert!(
        matches!(limited, FetchOutcome::Blocked { status: 429, retry_after: Some(wait) } if wait == Duration::from_secs(120)),
        "{limited:?}"
    );
    let unavailable = outcome("/unavailable").await;
    assert!(
        matches!(unavailable, FetchOutcome::Transient { retry_after: Some(wait) } if wait == Duration::from_secs(30)),
        "{unavailable:?}"
    );
    for path in ["/broken", "/closed", "/hang", "/empty"] {
        let outcome = outcome(path).await;
        assert!(
            matches!(outcome, FetchOutcome::Transient { .. }),
            "{path}: {outcome:?}"
        );
    }
    assert!(matches!(
        outcome("/challenge").await,
        FetchOutcome::Blocked { status: 200, .. }
    ));
    for path in ["/garbage", "/video"] {
        let outcome = outcome(path).await;
        assert!(
            matches!(outcome, FetchOutcome::Rejected(Rejection::NotImage)),
            "{path}: {outcome:?}"
        );
    }
    assert!(matches!(
        outcome("/bad").await,
        FetchOutcome::Rejected(Rejection::Status(400))
    ));
    assert_eq!(b.temp_files(), 0, "a refused body leaves no file");

    // URLs that are not the CDN's: nothing is sent.
    let before = b.fixture.hits().len();
    for url in [
        "http://scontent-mxp1-1.cdninstagram.com/v/x_n.jpg",
        "https://example.test/x.jpg",
        "https://www.instagram.com/p/C0ffee/",
        "https://127.0.0.1/x.jpg",
        "not a url",
    ] {
        let outcome = b.fetch(url).await;
        assert!(
            matches!(outcome, FetchOutcome::Rejected(Rejection::Url)),
            "{url}: {outcome:?}"
        );
    }
    assert_eq!(b.fixture.hits().len(), before);
    b.fixture.route(IG, &target(&ok), [Answer::jpeg(64)]);
    stored(b.fetch(&ok).await);
}

#[tokio::test]
async fn variants_ask_for_bounded_sizes_and_fall_back() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|_| {}).await;
    // X: name=large.
    b.fixture
        .route(X, "/media/GxYz?format=jpg&name=large", [Answer::jpeg(300)]);
    stored(b.fetch("https://pbs.twimg.com/media/GxYz.jpg").await);
    // Pinterest: /1200x/, falling back to the served size on 404 and 403.
    b.fixture
        .route(PIN, "/1200x/ab/cd/ef/one.jpg", [Answer::status(404)]);
    b.fixture
        .route(PIN, "/736x/ab/cd/ef/one.jpg", [Answer::jpeg(200)]);
    stored(b.fetch("https://i.pinimg.com/736x/ab/cd/ef/one.jpg").await);
    b.fixture.route(
        PIN,
        "/1200x/ab/cd/ef/two.jpg",
        [Answer::text(403, "Forbidden")],
    );
    b.fixture
        .route(PIN, "/originals/ab/cd/ef/two.jpg", [Answer::jpeg(200)]);
    stored(
        b.fetch("https://i.pinimg.com/originals/ab/cd/ef/two.jpg")
            .await,
    );
    // Both gone: the served size decides.
    b.fixture
        .route(PIN, "/1200x/ab/cd/ef/three.jpg", [Answer::status(404)]);
    b.fixture
        .route(PIN, "/236x/ab/cd/ef/three.jpg", [Answer::status(404)]);
    let gone = b
        .fetch("https://i.pinimg.com/236x/ab/cd/ef/three.jpg")
        .await;
    assert!(matches!(gone, FetchOutcome::Gone));
    let targets: Vec<String> = b.fixture.hits().into_iter().map(|hit| hit.target).collect();
    assert_eq!(
        targets,
        [
            "/media/GxYz?format=jpg&name=large",
            "/1200x/ab/cd/ef/one.jpg",
            "/736x/ab/cd/ef/one.jpg",
            "/1200x/ab/cd/ef/two.jpg",
            "/originals/ab/cd/ef/two.jpg",
            "/1200x/ab/cd/ef/three.jpg",
            "/236x/ab/cd/ef/three.jpg",
        ]
    );
}

#[tokio::test]
async fn redirects_stay_in_the_group_and_off_private_addresses() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|_| {}).await;
    let moved = ig_url("/v/moved_n.jpg", 3600);
    let elsewhere = format!("https://{IG_OTHER}/v/moved_n.jpg");
    b.fixture
        .route(IG, &target(&moved), [Answer::redirect(302, &elsewhere)]);
    b.fixture
        .route(IG_OTHER, "/v/moved_n.jpg", [Answer::jpeg(100)]);
    stored(b.fetch(&moved).await);

    for (path, location) in [
        ("/to/loopback", "https://127.0.0.1/x.jpg".to_owned()),
        (
            "/to/metadata",
            "https://169.254.169.254/latest/meta-data/".to_owned(),
        ),
        ("/to/evil", format!("https://{IG_EVIL}/x.jpg")),
        ("/to/off-group", "https://example.test/x.jpg".to_owned()),
        ("/to/http", format!("http://{IG_OTHER}/x.jpg")),
        ("/to/port", format!("https://{IG_OTHER}:8443/x.jpg")),
    ] {
        let url = ig_url(path, 3600);
        b.fixture
            .route(IG, &target(&url), [Answer::redirect(302, &location)]);
        let outcome = b.fetch(&url).await;
        assert!(
            matches!(outcome, FetchOutcome::Rejected(Rejection::Refused)),
            "{path}: {outcome:?}"
        );
    }
    for i in 0..6 {
        let url = format!("https://{IG}/hop/{i}");
        let next = format!("/hop/{}", i + 1);
        b.fixture
            .route(IG, &target(&url), [Answer::redirect(302, &next)]);
    }
    let looped = b.fetch(&format!("https://{IG}/hop/0")).await;
    assert!(
        matches!(looped, FetchOutcome::Rejected(Rejection::Redirects)),
        "{looped:?}"
    );
    let stray: Vec<String> = b
        .fixture
        .hits()
        .into_iter()
        .filter(|hit| hit.host != IG && hit.target != "/v/moved_n.jpg")
        .map(|hit| format!("{}{}", hit.host, hit.target))
        .collect();
    assert!(stray.is_empty(), "{stray:?}");
}

#[tokio::test]
async fn a_cdn_name_resolving_to_the_loopback_is_never_fetched() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|_| {}).await;
    let url = format!("https://{IG_EVIL}/v/x_n.jpg");
    let outcome = b.fetch(&url).await;
    assert!(
        matches!(outcome, FetchOutcome::Rejected(Rejection::Refused)),
        "{outcome:?}"
    );
    assert!(b.fixture.hits().is_empty());
}

#[tokio::test]
async fn the_15_mb_cap_holds() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|_| {}).await;
    let cap = 15 * MIB;
    let head = jpeg(64);
    b.fixture.route(
        IG,
        "/declared.jpg",
        [Answer::large("image/jpeg", head.clone(), cap + 1, false)],
    );
    b.fixture.route(
        IG,
        "/streamed.jpg",
        [Answer::large("image/jpeg", head.clone(), cap + 1, true)],
    );
    b.fixture.route(
        IG,
        "/exact.jpg",
        [Answer::large("image/jpeg", head, cap, true)],
    );
    for path in ["/declared.jpg", "/streamed.jpg"] {
        let outcome = b.fetch(&format!("https://{IG}{path}")).await;
        assert!(
            matches!(outcome, FetchOutcome::Rejected(Rejection::TooLarge)),
            "{path}: {outcome:?}"
        );
    }
    assert_eq!(b.temp_files(), 0, "the partial download is gone");
    let exact = stored(b.fetch(&format!("https://{IG}/exact.jpg")).await);
    assert_eq!(exact.size(), cap);
    drop(exact);
    assert_eq!(b.temp_files(), 0, "a dropped staged object is removed");
}

#[tokio::test]
async fn requests_keep_the_group_rate_and_concurrency() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|config| {
        config.limits.get_mut(HostGroup::Instagram).rate = 10.0;
        config.limits.get_mut(HostGroup::X).concurrency = 2;
    })
    .await;
    let cdn_task = |url: String| {
        let outbound = b.outbound.clone();
        let media = b.media.clone();
        tokio::spawn(async move {
            let request = FetchRequest {
                url: &url,
                media: &media,
                expires_at_ms: None,
                now_ms: NOW_MS,
            };
            outbound.cdn().fetch(request).await
        })
    };

    // Instagram at 10 a second: requests start at least 100 ms apart.
    let started = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..6 {
        let path = format!("/rate/{i}.jpg");
        b.fixture.route(IG, &path, [Answer::jpeg(100)]);
        tasks.push(cdn_task(format!("https://{IG}{path}")));
    }
    for task in tasks {
        stored(task.await.unwrap());
    }
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    let mut arrivals: Vec<Instant> = b.fixture.hits().iter().map(|hit| hit.at).collect();
    arrivals.sort();
    for pair in arrivals.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(gap >= Duration::from_millis(80), "{gap:?}");
    }

    // X with 2 fetches at once: never more in flight.
    let mut tasks = Vec::new();
    for i in 0..6 {
        let variant = format!("/media/slow{i}?format=jpg&name=large");
        b.fixture.route(
            X,
            &variant,
            [Answer::jpeg(100).delayed(Duration::from_millis(150))],
        );
        tasks.push(cdn_task(format!("https://{X}/media/slow{i}.jpg")));
    }
    for task in tasks {
        stored(task.await.unwrap());
    }
    assert_eq!(b.fixture.peak_in_flight(), 2);
    assert_eq!(
        b.outbound.limits().free_slots(HostGroup::X),
        2,
        "every slot back"
    );
}

/// The value of the series `name{labels}` in the exposition `text`.
fn series(text: &str, name: &str, labels: &str) -> Option<f64> {
    let prefix = format!("{name}{{{labels}}} ");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|value| value.trim().parse().ok())
}

#[tokio::test]
async fn the_breaker_opens_hands_over_and_probes() {
    let _turn = TURN.lock().await;
    let handle = metrics::install();
    let b = bench(|config| {
        config.breaker = BreakerConfig {
            window: 10,
            open_at_blocked: 3,
            open_at_streak: 3,
            open_for: Duration::from_millis(300),
        };
    })
    .await;
    let cdn = b.outbound.cdn();
    let mut changes = cdn.subscribe();
    changes.mark_unchanged();
    let gauge = |group: &str| {
        handle.run_upkeep();
        series(
            &handle.render(),
            "shelfy_breaker_open",
            &format!("host_group=\"{group}\""),
        )
    };
    assert_eq!(gauge("instagram"), Some(0.0), "0 from the start");
    assert_eq!(gauge("x_web"), Some(0.0));

    let url = format!("https://{IG}/blocked.jpg");
    b.fixture
        .route(IG, "/blocked.jpg", [Answer::text(403, "Forbidden")]);
    // Gone and expired answers never count.
    b.fixture.route(IG, "/gone.jpg", [Answer::status(404)]);
    b.fixture.route(
        IG,
        "/expired.jpg",
        [Answer::text(403, "URL signature expired")],
    );
    for _ in 0..5 {
        assert!(matches!(
            b.fetch(&format!("https://{IG}/gone.jpg")).await,
            FetchOutcome::Gone
        ));
        assert!(matches!(
            b.fetch(&format!("https://{IG}/expired.jpg")).await,
            FetchOutcome::Expired
        ));
    }
    assert_eq!(
        cdn.breaker_state(HostGroup::Instagram),
        BreakerState::Closed
    );
    for _ in 0..3 {
        assert!(matches!(
            b.fetch(&url).await,
            FetchOutcome::Blocked { status: 403, .. }
        ));
    }
    assert!(matches!(
        cdn.breaker_state(HostGroup::Instagram),
        BreakerState::Open { .. }
    ));
    assert!(changes.has_changed().unwrap());
    assert_eq!(gauge("instagram"), Some(1.0));
    // Open: nothing is sent.
    let sent = b.fixture.hits_of(IG, "/blocked.jpg").len();
    assert!(matches!(b.fetch(&url).await, FetchOutcome::BreakerOpen));
    assert_eq!(b.fixture.hits_of(IG, "/blocked.jpg").len(), sent);
    // Other groups are not affected.
    b.fixture
        .route(X, "/media/ok?format=jpg&name=large", [Answer::jpeg(100)]);
    stored(b.fetch("https://pbs.twimg.com/media/ok.jpg").await);
    // An expired URL is still settled without a request.
    let expired = ig_url("/v/old.jpg", -1);
    assert!(matches!(b.fetch(&expired).await, FetchOutcome::Expired));

    // After the wait, one probe: still blocked, so open again.
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        cdn.breaker_state(HostGroup::Instagram),
        BreakerState::HalfOpen
    );
    assert!(matches!(b.fetch(&url).await, FetchOutcome::Blocked { .. }));
    assert!(matches!(
        cdn.breaker_state(HostGroup::Instagram),
        BreakerState::Open { .. }
    ));
    assert!(matches!(b.fetch(&url).await, FetchOutcome::BreakerOpen));
    assert_eq!(
        b.fixture.hits_of(IG, "/blocked.jpg").len(),
        sent + 1,
        "one probe"
    );

    // The CDN serves again: the next probe closes it.
    tokio::time::sleep(Duration::from_millis(350)).await;
    b.fixture.route(IG, "/blocked.jpg", [Answer::jpeg(100)]);
    changes.mark_unchanged();
    stored(b.fetch(&url).await);
    assert_eq!(
        cdn.breaker_state(HostGroup::Instagram),
        BreakerState::Closed
    );
    assert!(changes.has_changed().unwrap());
    assert_eq!(gauge("instagram"), Some(0.0));

    let text = {
        handle.run_upkeep();
        handle.render()
    };
    for (labels, at_least) in [
        ("host_group=\"instagram\",outcome=\"blocked\"", 4.0),
        ("host_group=\"instagram\",outcome=\"breaker_open\"", 2.0),
        ("host_group=\"instagram\",outcome=\"gone\"", 5.0),
        ("host_group=\"instagram\",outcome=\"expired\"", 6.0),
        ("host_group=\"instagram\",outcome=\"stored\"", 1.0),
        ("host_group=\"x\",outcome=\"stored\"", 1.0),
    ] {
        let value = series(&text, "shelfy_media_fetch_total", labels)
            .unwrap_or_else(|| panic!("no {labels} in\n{text}"));
        assert!(value >= at_least, "{labels}: {value}");
    }
}

#[tokio::test]
async fn a_dropped_probe_frees_the_probe_slot() {
    let _turn = TURN.lock().await;
    let b = bench(|config| {
        config.breaker = BreakerConfig {
            window: 10,
            open_at_blocked: 1,
            open_at_streak: 1,
            open_for: Duration::from_millis(100),
        };
    })
    .await;
    let url = format!("https://{PIN}/1200x/a.jpg");
    b.fixture.route(PIN, "/1200x/a.jpg", [Answer::status(429)]);
    assert!(matches!(
        b.fetch(&url).await,
        FetchOutcome::Blocked { status: 429, .. }
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    // The probe hangs and is dropped.
    b.fixture.route(PIN, "/1200x/a.jpg", [Answer::Hang]);
    let probe = tokio::time::timeout(Duration::from_millis(200), b.fetch(&url)).await;
    assert!(probe.is_err(), "the probe was cut short");
    assert_eq!(
        b.outbound.cdn().breaker_state(HostGroup::Pinterest),
        BreakerState::HalfOpen
    );
    // The next fetch probes, and the CDN serves it.
    b.fixture.route(PIN, "/1200x/a.jpg", [Answer::jpeg(100)]);
    stored(b.fetch(&url).await);
    assert_eq!(
        b.outbound.cdn().breaker_state(HostGroup::Pinterest),
        BreakerState::Closed
    );
}

#[tokio::test]
async fn hydration_groups_share_the_limits_and_breakers() {
    let _turn = TURN.lock().await;
    let b = calm_bench(|_| {}).await;
    let limits = b.outbound.limits();
    // SPIKE-9's paces: 1 request per 3 s to www.instagram.com, 1 a second to
    // the other hydration hosts (the bench speeds them up; the defaults
    // are checked in the unit tests).
    assert_eq!(limits.period(HostGroup::XWeb), Duration::from_millis(10));
    let slot = limits.acquire(HostGroup::InstagramWeb).await;
    assert_eq!(
        limits.free_slots(HostGroup::InstagramWeb),
        0,
        "one at a time"
    );
    drop(slot);
    let breaker = b.outbound.breakers().get(HostGroup::InstagramWeb);
    breaker.trip(Instant::now());
    assert!(matches!(
        breaker.state(Instant::now()),
        BreakerState::Open { .. }
    ));
    assert_eq!(
        b.outbound.cdn().breaker_state(HostGroup::Instagram),
        BreakerState::Closed,
        "the CDN group has its own breaker"
    );
}
