//! The archive drain (P2-10) against the fixture CDN, on synthetic
//! libraries: covers and slides with their masters, renditions and
//! ThumbHash; expired URLs never fetched; gone and refused items; the
//! breaker handoff and its return; backoff and requeue on a test clock; a
//! crash mid-chunk; the asset types; the quota.
//!
//! The breakers' gauges are process-wide: the tests take turns ([`TURN`]).

mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use image::RgbImage;
use image::codecs::jpeg::JpegEncoder;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shelfy_core::ingest::archive::FETCH_TRIES;
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, NewMedia, NewPost};
use shelfy_media::refs::{self, ObjectMeta, Origin, Role};
use shelfy_media::store::{IngestLimits, MediaStore};
use shelfy_server::config::Config;
use shelfy_server::events::Delivery;
use shelfy_server::events::model::{EventTopic, JobState};
use shelfy_server::ids::now_ms;
use shelfy_server::jobs::archive::store::{self, Committed, StoreContext, Target};
use shelfy_server::jobs::archive::{self, KIND, select::Slot};
use shelfy_server::jobs::{Scheduler, kinds};
use shelfy_server::outbound::{BreakerConfig, BreakerState, HostGroup};
use shelfy_server::quota;
use support::cdn::{Answer, FixtureCdn};
use support::jobs::START;
use support::library::ALICE;
use support::{TestState, from_app, send};
use tokio_util::sync::CancellationToken;

/// One test at a time: the breaker gauges are shared.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const IG: &str = "scontent-mxp1-1.cdninstagram.com";
const X: &str = "pbs.twimg.com";
const PIN: &str = "i.pinimg.com";

/// A breaker that never opens, for the tests about other things.
const CALM: BreakerConfig = BreakerConfig {
    window: 1000,
    open_at_blocked: 1000,
    open_at_streak: 1000,
    open_for: Duration::from_secs(1),
};

struct Bench {
    t: TestState,
    fixture: FixtureCdn,
    scheduler: Option<Scheduler>,
}

/// A state whose outbound HTTP reaches the fixture CDN, without pacing,
/// with the breakers of `breaker`; Alice is a member without a quota.
async fn bench(breaker: BreakerConfig) -> Bench {
    let fixture = FixtureCdn::start().await;
    let t = TestState::with_config(|config: &mut Config| {
        config.outbound = outbound(&fixture, breaker);
    });
    t.add_user(ALICE);
    Bench {
        t,
        fixture,
        scheduler: None,
    }
}

fn outbound(
    fixture: &FixtureCdn,
    breaker: BreakerConfig,
) -> shelfy_server::outbound::OutboundConfig {
    let mut config = fixture.config(&[IG, X, PIN], &[], &[]);
    for group in HostGroup::ALL {
        let limits = config.limits.get_mut(group);
        limits.rate = 100.0;
        limits.jitter = None;
        limits.spread = Duration::ZERO;
    }
    config.breaker = breaker;
    config
}

impl Bench {
    fn start(&mut self) {
        if self.scheduler.is_none() {
            self.scheduler = Some(
                self.t
                    .state
                    .jobs()
                    .start(self.t.state.clone(), CancellationToken::new()),
            );
        }
    }

    /// Runs Alice's drain until it succeeds.
    async fn drain(&mut self) -> i64 {
        self.start();
        let job = archive::enqueue(self.t.state.jobs(), ALICE)
            .await
            .unwrap()
            .job;
        self.t
            .wait_job(ALICE, job.id, |job| job.state == JobState::Succeeded)
            .await;
        job.id
    }

    fn library(&self) -> Connection {
        Connection::open(self.t.data_dir().library_db(ALICE)).unwrap()
    }

    fn hits(&self, host: &str, url: &str) -> usize {
        self.fixture.hits_of(host, &target(url)).len()
    }
}

/// A JPEG of `width` × `height` that decodes.
fn jpeg(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let image = RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([seed, (x % 256) as u8, (y % 256) as u8])
    });
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&image)
        .unwrap();
    out
}

fn image(bytes: Vec<u8>) -> Answer {
    Answer::new(200, "image/jpeg", bytes)
}

/// An Instagram URL of `name` whose `oe` is `seconds` from `now_ms`.
fn ig_url(name: &str, now_ms: i64, seconds: i64) -> String {
    format!(
        "https://{IG}/v/t51.2885-15/{name}.jpg?stp=dst-jpg_e35&oe={:X}&oh=00_x",
        now_ms / 1000 + seconds
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

/// A `pending` post of Alice, inserted a minute ago.
fn post(key: &str, platform: Platform, media_type: &str) -> NewPost {
    post_at(key, platform, media_type, now_ms() - 60_000)
}

fn post_at(key: &str, platform: Platform, media_type: &str, imported_at: i64) -> NewPost {
    let native = key.split_once('_').unwrap().1;
    let mut post = NewPost::new(key, platform, native, media_type, imported_at);
    post.archive_state = Some("pending".to_owned());
    post
}

fn slide(kind: &str, url: &str) -> NewMedia {
    NewMedia {
        kind: kind.to_owned(),
        source_url: Some(url.to_owned()),
        ..NewMedia::default()
    }
}

async fn seed(t: &TestState, posts: Vec<NewPost>) {
    t.write(ALICE, move |tx| {
        for post in &posts {
            posts::insert(tx, post, post.imported_at)?;
        }
        Ok(())
    })
    .await;
}

fn states(conn: &Connection) -> BTreeMap<String, String> {
    conn.prepare("SELECT key, archive_state FROM posts")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn post_id(conn: &Connection, key: &str) -> i64 {
    conn.query_row("SELECT id FROM posts WHERE key = ?1", [key], |r| r.get(0))
        .unwrap()
}

/// `cover_object` and the ThumbHash's length of `key`.
fn cover(conn: &Connection, key: &str) -> (Option<i64>, Option<i64>) {
    conn.query_row(
        "SELECT cover_object, length(thumbhash) FROM posts WHERE key = ?1",
        [key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

/// The object of each slide of `key`, by position.
fn slide_objects(conn: &Connection, key: &str) -> Vec<Option<i64>> {
    conn.prepare(
        "SELECT object_id FROM post_media WHERE post_id = (SELECT id FROM posts WHERE key = ?1)
         ORDER BY position",
    )
    .unwrap()
    .query_map([key], |r| r.get(0))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// `fetch_attempts`, `fetch_next_at` and `fetch_error` of a slide.
fn fetch_state(conn: &Connection, key: &str, position: i64) -> (i64, Option<i64>, Option<String>) {
    conn.query_row(
        "SELECT fetch_attempts, fetch_next_at, fetch_error FROM post_media
         WHERE post_id = (SELECT id FROM posts WHERE key = ?1) AND position = ?2",
        params![key, position],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

#[derive(Debug)]
struct Object {
    ext: String,
    width: Option<i64>,
    height: Option<i64>,
    g480: bool,
    role: String,
    sha256: Vec<u8>,
}

fn object(conn: &Connection, id: i64) -> Object {
    conn.query_row(
        "SELECT ext, width, height, variants, role, sha256 FROM media_objects WHERE id = ?1",
        [id],
        |r| {
            Ok(Object {
                ext: r.get(0)?,
                width: r.get(1)?,
                height: r.get(2)?,
                g480: r.get::<_, i64>(3)? & 1 == 1,
                role: r.get(4)?,
                sha256: r.get(5)?,
            })
        },
    )
    .unwrap()
}

/// Objects no post or slide references.
fn orphans(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM media_objects o
         WHERE NOT EXISTS (SELECT 1 FROM posts WHERE cover_object = o.id)
           AND NOT EXISTS (SELECT 1 FROM post_media WHERE object_id = o.id OR video_object_id = o.id)",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// Whether the file of every object row exists.
fn files_exist(t: &TestState, conn: &Connection) -> bool {
    let media = MediaStore::new(t.data_dir().users_dir())
        .user(ALICE)
        .unwrap();
    conn.prepare("SELECT sha256, ext FROM media_objects")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .all(|(sha, ext)| {
            let digest = shelfy_media::Digest::from_slice(&sha).unwrap();
            let kind = shelfy_media::MediaKind::from_ext(&ext).unwrap();
            media.contains(&digest, kind)
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn covers_and_slides_are_stored_with_their_masters_and_renditions() {
    let _turn = TURN.lock().await;
    let mut b = bench(CALM).await;
    let now = now_ms();

    // ig_1: a carousel of five images; slide 1 is too large to keep as
    // served.
    let urls: Vec<String> = (0..5)
        .map(|i| ig_url(&format!("c{i}"), now, 3_600))
        .collect();
    for (i, url) in urls.iter().enumerate() {
        let bytes = if i == 1 {
            jpeg(2_400, 1_200, 1)
        } else {
            jpeg(1_080, 1_350, 10 + i as u8)
        };
        b.fixture.route(IG, &target(url), [image(bytes)]);
    }
    let mut carousel = post("ig_1", Platform::Instagram, "carousel");
    carousel.cover_url = Some(urls[0].clone());
    carousel.media = urls.iter().map(|url| slide("image", url)).collect();
    // x_2: a video; slide 0 carries its poster, which is the cover.
    let poster = format!("https://{X}/ext_tw_video_thumb/2/pu/img/p.jpg");
    b.fixture
        .route(X, &target(&poster), [image(jpeg(1_280, 720, 30))]);
    let mut video = post("x_2", Platform::Twitter, "video");
    video.cover_url = Some(poster.clone());
    video.media = vec![NewMedia {
        video_url: Some("https://video.twimg.com/ext_tw_video/2/pu/vid/v.mp4".to_owned()),
        ..slide("video", &poster)
    }];
    // pin_3: a pin served at 736 px; the archive asks for 1,200 px.
    let pin = format!("https://{PIN}/736x/ab/cd/pin3.jpg");
    b.fixture.route(
        PIN,
        "/1200x/ab/cd/pin3.jpg",
        [image(jpeg(1_200, 1_600, 40))],
    );
    let mut pin_post = post("pin_3", Platform::Pinterest, "image");
    pin_post.cover_url = Some(pin.clone());
    pin_post.media = vec![slide("image", &pin)];
    // x_4: a text tweet: its cover is the author's avatar, without slides.
    let avatar = format!("https://{X}/profile_images/4/a.jpg");
    b.fixture
        .route(X, &target(&avatar), [image(jpeg(400, 400, 50))]);
    let mut text = post("x_4", Platform::Twitter, "text");
    text.cover_url = Some(avatar.clone());
    seed(&b.t, vec![carousel, video, pin_post, text]).await;

    let mut events = b.t.state.events().subscribe(ALICE, None);
    b.drain().await;
    let conn = b.library();
    assert!(
        states(&conn).values().all(|state| state == "done"),
        "{:?}",
        states(&conn)
    );

    // The carousel: slide 0 is the cover; slide 1's master is a WebP at
    // 2,048 px; slides 1–3 have a g480, slide 4 none.
    let (cover_id, thumbhash) = cover(&conn, "ig_1");
    let thumbhash = thumbhash.unwrap();
    assert!((1..=25).contains(&thumbhash), "{thumbhash}");
    let slides: Vec<i64> = slide_objects(&conn, "ig_1")
        .into_iter()
        .map(Option::unwrap)
        .collect();
    assert_eq!(Some(slides[0]), cover_id);
    let first = object(&conn, slides[0]);
    assert_eq!((first.ext.as_str(), first.g480), ("jpg", true));
    assert_eq!((first.width, first.height), (Some(1_080), Some(1_350)));
    let large = object(&conn, slides[1]);
    assert_eq!(large.ext, "webp", "re-encoded: wider than 2,048 px");
    assert_eq!((large.width, large.height), (Some(2_048), Some(1_024)));
    assert!(large.g480);
    assert!(object(&conn, slides[2]).g480 && object(&conn, slides[3]).g480);
    let last = object(&conn, slides[4]);
    assert_eq!((last.ext.as_str(), last.g480), ("jpg", false));

    // The video's poster: a WebP at 1,080 px, shared with slide 0.
    let (poster_id, poster_hash) = cover(&conn, "x_2");
    let poster_object = object(&conn, poster_id.unwrap());
    assert_eq!(
        (poster_object.ext.as_str(), poster_object.role.as_str()),
        ("webp", "poster")
    );
    assert_eq!(
        (poster_object.width, poster_object.height),
        (Some(1_080), Some(608))
    );
    assert!(poster_object.g480 && poster_hash.is_some());
    assert_eq!(slide_objects(&conn, "x_2"), [poster_id]);

    // The pin at 1,200 px; the avatar without slides.
    assert_eq!(b.fixture.hits_of(PIN, "/1200x/ab/cd/pin3.jpg").len(), 1);
    assert_eq!(b.hits(PIN, &pin), 0, "the variant answered");
    let (avatar_id, avatar_hash) = cover(&conn, "x_4");
    assert!(avatar_id.is_some() && avatar_hash.is_some());
    let attempts: i64 = conn
        .query_row(
            "SELECT cover_fetch_attempts FROM posts WHERE key = 'x_4'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 0);

    // Every object is the server's, with its file; nothing is left over.
    let origins: Vec<String> = conn
        .prepare("SELECT DISTINCT origin FROM media_objects")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(origins, ["server"]);
    assert!(files_exist(&b.t, &conn));
    assert_eq!(orphans(&conn), 0);
    let tmp = MediaStore::new(b.t.data_dir().users_dir())
        .user(ALICE)
        .unwrap()
        .root()
        .join(".tmp");
    assert_eq!(std::fs::read_dir(tmp).map_or(0, Iterator::count), 0);

    // The quota counted what was stored.
    let stored: i64 = conn
        .query_row("SELECT sum(bytes) FROM media_objects", [], |r| r.get(0))
        .unwrap();
    let counted: i64 =
        b.t.control()
            .query_row(
                "SELECT usage_media_bytes FROM users WHERE id = ?1",
                [ALICE],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(counted, stored);

    // The streams heard about it.
    let mut archived = false;
    while let Ok(Delivery::Event(event)) =
        tokio::time::timeout(Duration::from_millis(500), events.next()).await
    {
        if event.topic == EventTopic::PostsChanged {
            let data: Value = serde_json::from_str(&event.data).unwrap();
            archived |= data["reason"] == "archive";
        }
    }
    assert!(archived, "posts.changed with reason archive");

    // A second drain finds nothing to do.
    let hits = b.fixture.hits().len();
    b.drain().await;
    assert_eq!(b.fixture.hits().len(), hits);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_url_is_never_fetched() {
    let _turn = TURN.lock().await;
    // One blocked answer would open the breaker: an expiry must not.
    let mut b = bench(BreakerConfig {
        open_at_streak: 1,
        open_at_blocked: 1,
        ..CALM
    })
    .await;
    let now = now_ms();
    // ig_5: expired by its column (its state is stale: still `pending`).
    let past = ig_url("e5", now, -60);
    // ig_6: expired by its `oe`, without a column.
    let oe_past = ig_url("e6", now, -60);
    // ig_7: valid by its `oe`, but the CDN says the signature expired.
    let refused = ig_url("e7", now, 3_600);
    for url in [&past, &oe_past] {
        b.fixture.route(IG, &target(url), [image(jpeg(64, 64, 1))]);
    }
    b.fixture.route(
        IG,
        &target(&refused),
        [Answer::text(403, "URL signature expired")],
    );
    let mut stale = post("ig_5", Platform::Instagram, "image");
    stale.cover_url = Some(past.clone());
    stale.cover_url_expires_at = Some(now - 60_000);
    stale.media = vec![NewMedia {
        source_url_expires_at: Some(now - 60_000),
        ..slide("image", &past)
    }];
    let mut by_oe = post("ig_6", Platform::Instagram, "image");
    by_oe.cover_url = Some(oe_past.clone());
    by_oe.media = vec![slide("image", &oe_past)];
    let mut by_answer = post("ig_7", Platform::Instagram, "image");
    by_answer.cover_url = Some(refused.clone());
    by_answer.media = vec![slide("image", &refused)];
    seed(&b.t, vec![stale, by_oe, by_answer]).await;

    b.drain().await;
    let conn = b.library();
    for key in ["ig_5", "ig_6", "ig_7"] {
        assert_eq!(
            states(&conn)[key],
            "client",
            "{key}: the extension refreshes it"
        );
    }
    assert_eq!(b.hits(IG, &past), 0, "never fetched");
    assert_eq!(b.hits(IG, &oe_past), 0, "never fetched");
    assert_eq!(b.hits(IG, &refused), 1);
    // The expiry is recorded, and spends no try; the breaker stays closed.
    let (expires_at, attempts): (Option<i64>, i64) = conn
        .query_row(
            "SELECT source_url_expires_at, fetch_attempts FROM post_media
             WHERE post_id = (SELECT id FROM posts WHERE key = 'ig_7')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(expires_at.is_some_and(|at| at <= now_ms()));
    assert_eq!(attempts, 0);
    assert_eq!(
        b.t.state
            .outbound()
            .cdn()
            .breaker_state(HostGroup::Instagram),
        BreakerState::Closed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gone_and_refused_items_fail_for_good() {
    let _turn = TURN.lock().await;
    let mut b = bench(CALM).await;
    // x_8: a slide whose both variants answer 404 (unrouted).
    let gone = format!("https://{X}/media/gone8.jpg");
    let mut gone_post = post("x_8", Platform::Twitter, "image");
    gone_post.media = vec![slide("image", &gone)];
    // x_9: a text tweet whose avatar answers 410.
    let avatar = format!("https://{X}/profile_images/9/a.jpg");
    b.fixture.route(X, &target(&avatar), [Answer::status(410)]);
    let mut text = post("x_9", Platform::Twitter, "text");
    text.cover_url = Some(avatar.clone());
    // x_10: a slide whose answer claims to be an image and is not one.
    let fake = format!("https://{X}/media/fake10.jpg");
    b.fixture.route(
        X,
        "/media/fake10?format=jpg&name=large",
        [Answer::new(200, "image/jpeg", "not a picture")],
    );
    let mut fake_post = post("x_10", Platform::Twitter, "image");
    fake_post.media = vec![slide("image", &fake)];
    seed(&b.t, vec![gone_post, text, fake_post]).await;

    b.drain().await;
    let conn = b.library();
    for key in ["x_8", "x_9", "x_10"] {
        assert_eq!(states(&conn)[key], "failed", "{key}");
    }
    assert_eq!(fetch_state(&conn, "x_8", 0), (1, None, Some("gone".into())));
    assert_eq!(
        fetch_state(&conn, "x_10", 0),
        (FETCH_TRIES, None, Some("not_image".into()))
    );
    let avatar_state: (i64, Option<String>) = conn
        .query_row(
            "SELECT cover_fetch_attempts, cover_fetch_error FROM posts WHERE key = 'x_9'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(avatar_state, (1, Some("gone".into())));
    let hits = b.fixture.hits().len();
    assert_eq!(hits, 2 + 1 + 1, "the 404 variant falls back once");

    // A later drain fetches none of them again.
    b.drain().await;
    assert_eq!(b.fixture.hits().len(), hits);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_breaker_hands_the_platform_over_and_back() {
    let _turn = TURN.lock().await;
    let mut b = bench(BreakerConfig {
        window: 50,
        open_at_blocked: 2,
        open_at_streak: 2,
        open_for: Duration::from_millis(1_500),
    })
    .await;
    let now = now_ms();
    let urls: Vec<String> = (11..15)
        .map(|n| ig_url(&format!("b{n}"), now, 86_400))
        .collect();
    let mut posts = Vec::new();
    for (i, url) in urls.iter().enumerate() {
        let n = 11 + i;
        // The first three are blocked once, then served.
        let mut answers = Vec::new();
        if i < 3 {
            answers.push(Answer::text(403, "Forbidden"));
        }
        answers.push(image(jpeg(320, 320, n as u8)));
        b.fixture.route(IG, &target(url), answers);
        let mut p = post(&format!("ig_{n}"), Platform::Instagram, "image");
        p.cover_url = Some(url.clone());
        p.media = vec![slide("image", url)];
        posts.push(p);
    }
    seed(&b.t, posts).await;

    // Two blocks open the breaker. A third fetch may already be admitted
    // while those answers are in flight; it may also see the open breaker.
    // The fourth item is never sent, and everything goes to the extension.
    b.drain().await;
    let cdn = b.t.state.outbound().cdn();
    assert!(matches!(
        cdn.breaker_state(HostGroup::Instagram),
        BreakerState::Open { .. }
    ));
    let conn = b.library();
    assert!(
        states(&conn).values().all(|state| state == "client"),
        "{:?}",
        states(&conn)
    );
    let hits = b.fixture.hits().len();
    assert!((2..=3).contains(&hits), "{hits} fetches before handoff");
    for url in &urls[..3] {
        assert!(b.hits(IG, url) <= 1, "no retry before handoff: {url}");
    }
    assert_eq!(b.hits(IG, &urls[3]), 0);
    assert_eq!(fetch_state(&conn, "ig_11", 0).2.as_deref(), Some("blocked"));
    let open = archive::sync_breakers(&b.t.state).await.unwrap();
    assert_eq!(open, [(Platform::Instagram, true)]);
    assert!(states(&conn).values().all(|state| state == "client"));

    // Half-open after its time: the items come back, the probe passes,
    // the breaker closes, and the rest follows.
    tokio::time::sleep(Duration::from_millis(1_600)).await;
    let back = archive::sync_breakers(&b.t.state).await.unwrap();
    assert_eq!(back, [(Platform::Instagram, false)]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while states(&conn).values().any(|state| state != "done") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            states(&conn)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        cdn.breaker_state(HostGroup::Instagram),
        BreakerState::Closed
    );
    for (i, url) in urls.iter().enumerate() {
        assert_eq!(b.hits(IG, url), if i < 3 { 2 } else { 1 }, "{url}");
    }

    // An extension upload still in flight for a slot the server filled
    // meanwhile changes nothing: the store finds the slot filled.
    let media = MediaStore::new(b.t.data_dir().users_dir())
        .user(ALICE)
        .unwrap();
    let staged = media
        .ingest(
            std::io::Cursor::new(jpeg(320, 320, 11)),
            IngestLimits::ARCHIVE_IMAGE,
        )
        .unwrap();
    let id = post_id(&conn, "ig_11");
    let target = store::target_of(&conn, "ig_11", Slot::Cover)
        .unwrap()
        .unwrap();
    assert_eq!(
        target,
        Target {
            post_id: id,
            key: "ig_11".into(),
            platform: Platform::Instagram,
            slot: Slot::Cover,
            poster: false,
            cover: true,
            grid: true,
        }
    );
    assert_eq!(
        store::target_of(&conn, "ig_11", Slot::Slide(3)).unwrap(),
        None
    );
    assert_eq!(store::target_of(&conn, "ig_99", Slot::Cover).unwrap(), None);
    let prepared = store::prepare(&media, staged, &target).unwrap();
    let reservation = quota::reserve(&b.t.state, ALICE, 1_000).await.unwrap();
    let before = cover(&conn, "ig_11");
    let committed =
        b.t.write(ALICE, move |tx| {
            let policy = shelfy_core::ingest::archive::ArchivePolicy::default();
            let cx = StoreContext {
                media: &media,
                origin: Origin::Extension,
                policy: &policy,
                now: now_ms(),
            };
            store::commit(tx, &cx, &target, prepared, reservation)
        })
        .await;
    assert_eq!(committed, Committed::Unwanted);
    assert_eq!(cover(&conn, "ig_11"), before);
}

#[tokio::test(start_paused = true)]
async fn failed_tries_back_off_and_the_drain_requeues_itself() {
    let _turn = TURN.lock().await;
    // No host resolves (the test state's default): every try fails
    // transiently, without a request.
    let t = TestState::with_jobs(kinds::registry());
    t.add_user(ALICE);
    let mut slow = post_at("x_20", Platform::Twitter, "image", START);
    slow.media = vec![slide("image", "https://pbs.twimg.com/media/t20.jpg")];
    seed(&t, vec![slow]).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let job = archive::enqueue(t.state.jobs(), ALICE).await.unwrap().job;
    let conn = Connection::open(t.data_dir().library_db(ALICE)).unwrap();
    let clock = *t.state.jobs().clock();

    let mut previous_next: Option<i64> = None;
    for attempt in 1..FETCH_TRIES {
        let next_at = loop {
            let (attempts, next_at, error) = fetch_state(&conn, "x_20", 0);
            if attempts == attempt {
                assert_eq!(error.as_deref(), Some("transient"));
                break next_at.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        // The try came no sooner than the backoff allowed.
        if let Some(previous) = previous_next {
            assert!(clock.now_ms() >= previous);
        }
        // 30 s × 2ⁿ⁻¹ with jitter in its upper half, from about now.
        let base = 30_000_i64 << (attempt - 1);
        let wait = next_at - clock.now_ms();
        assert!(
            wait > base / 2 - 2_000 && wait <= base,
            "try {attempt}: {wait} ms"
        );
        // The drain waits for the item, without using a try.
        let row = loop {
            let row = t.job(ALICE, job.id).await;
            if row.state == JobState::Queued && row.run_at == next_at {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(row.attempts, 0);
        previous_next = Some(next_at);
    }
    // The last try fails the item; the drain is done.
    let done = t
        .wait_job(ALICE, job.id, |job| job.state == JobState::Succeeded)
        .await;
    assert_eq!(done.attempts, 0);
    assert_eq!(
        fetch_state(&conn, "x_20", 0),
        (FETCH_TRIES, None, Some("transient".into()))
    );
    assert_eq!(states(&conn)["x_20"], "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_mid_chunk_leaves_no_orphan_rows() {
    let _turn = TURN.lock().await;
    let mut b = bench(CALM).await;
    let mut posts = Vec::new();
    for n in 30..36 {
        let url = format!("https://{X}/media/k{n}.jpg");
        let variant = format!("/media/k{n}?format=jpg&name=large");
        // One answers at once; the others hang until the crash.
        let answer = if n == 30 {
            image(jpeg(200, 200, n))
        } else {
            Answer::Hang
        };
        b.fixture.route(X, &variant, [answer]);
        let mut p = post(&format!("x_{n}"), Platform::Twitter, "image");
        p.media = vec![slide("image", &url)];
        posts.push(p);
    }
    seed(&b.t, posts).await;
    b.start();
    archive::enqueue(b.t.state.jobs(), ALICE).await.unwrap();
    let conn = b.library();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while states(&conn)["x_30"] != "done" {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    while b.fixture.hits().len() < 3 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The crash: nothing more is recorded.
    b.scheduler.take().unwrap().abort().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(orphans(&conn), 0);
    assert!(files_exist(&b.t, &conn));
    let stored: i64 = conn
        .query_row("SELECT count(*) FROM media_objects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, 1);

    // The next start finishes the work.
    for n in 31..36 {
        b.fixture.route(
            X,
            &format!("/media/k{n}?format=jpg&name=large"),
            [image(jpeg(200, 200, n))],
        );
    }
    let restarted = TestState::with_config(|config: &mut Config| {
        config.data_dir = b.t.data_dir();
        config.outbound = outbound(&b.fixture, CALM);
    });
    let _scheduler = restarted
        .state
        .jobs()
        .start(restarted.state.clone(), CancellationToken::new());
    let job: i64 = restarted
        .control()
        .query_row(
            "SELECT id FROM jobs WHERE kind = ?1 ORDER BY id DESC",
            [KIND],
            |r| r.get(0),
        )
        .unwrap();
    restarted
        .wait_job(ALICE, job, |job| job.state == JobState::Succeeded)
        .await;
    assert!(states(&conn).values().all(|state| state == "done"));
    assert_eq!(orphans(&conn), 0);
    assert!(files_exist(&restarted, &conn));
}

async fn put_asset_types(t: &TestState, image: bool) -> StatusCode {
    let body = json!({"archiveAssetTypes": {"thumbnail": true, "image": image, "video": false}});
    let request = from_app(
        Request::put("/api/v1/me/settings")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    );
    send(&t.app_as(ALICE), request).await.status()
}

/// The newest drain job of Alice, waited for.
async fn wait_latest_drain(t: &TestState) {
    let job: i64 = t
        .control()
        .query_row(
            "SELECT id FROM jobs WHERE kind = ?1 ORDER BY id DESC",
            [KIND],
            |r| r.get(0),
        )
        .unwrap();
    t.wait_job(ALICE, job, |job| job.state == JobState::Succeeded)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_asset_types_choose_what_is_archived() {
    let _turn = TURN.lock().await;
    let mut b = bench(CALM).await;
    let now = now_ms();
    let urls: Vec<String> = (0..3)
        .map(|i| ig_url(&format!("t{i}"), now, 86_400))
        .collect();
    for (i, url) in urls.iter().enumerate() {
        b.fixture
            .route(IG, &target(url), [image(jpeg(300, 300, 60 + i as u8))]);
    }
    let mut carousel = post("ig_40", Platform::Instagram, "carousel");
    carousel.cover_url = Some(urls[0].clone());
    carousel.media = urls.iter().map(|url| slide("image", url)).collect();
    seed(&b.t, vec![carousel]).await;

    // Covers only: the cover (and slide 0, which shares it) is enough.
    assert_eq!(put_asset_types(&b.t, false).await, StatusCode::OK);
    b.start();
    wait_latest_drain(&b.t).await;
    let conn = b.library();
    assert_eq!(states(&conn)["ig_40"], "done");
    let objects = slide_objects(&conn, "ig_40");
    assert!(objects[0].is_some() && objects[1].is_none() && objects[2].is_none());
    assert_eq!(b.hits(IG, &urls[1]) + b.hits(IG, &urls[2]), 0);

    // Images back on: the post has work again, and its drain is queued.
    assert_eq!(put_asset_types(&b.t, true).await, StatusCode::OK);
    assert_eq!(states(&conn)["ig_40"], "partial");
    wait_latest_drain(&b.t).await;
    assert_eq!(states(&conn)["ig_40"], "done");
    assert!(slide_objects(&conn, "ig_40").iter().all(Option::is_some));
    let first = object(&conn, objects[0].unwrap());
    let second = object(&conn, slide_objects(&conn, "ig_40")[1].unwrap());
    assert!(second.g480, "slide 1 of a multi-image post");
    assert_ne!(first.sha256, second.sha256);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_quota_leaves_the_post_link_only() {
    let _turn = TURN.lock().await;
    let mut b = bench(CALM).await;
    // A quota of one byte: no reservation fits.
    b.t.control()
        .execute("UPDATE users SET quota_bytes = 1 WHERE id = ?1", [ALICE])
        .unwrap();
    let url = format!("https://{X}/media/q50.jpg");
    b.fixture.route(
        X,
        "/media/q50?format=jpg&name=large",
        [image(jpeg(100, 100, 5))],
    );
    let mut p = post("x_50", Platform::Twitter, "image");
    p.media = vec![slide("image", &url)];
    seed(&b.t, vec![p]).await;
    b.drain().await;
    let conn = b.library();
    assert_eq!(states(&conn)["x_50"], "link_only");
    assert!(b.fixture.hits().is_empty(), "nothing was fetched");
    // `link_only` stays: a later drain leaves it alone.
    b.drain().await;
    assert_eq!(states(&b.library())["x_50"], "link_only");
}

/// The selection: one item for a cover fetched through its slide 0, none
/// for a post that waits for its hydration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cover_and_its_slide_0_are_one_item() {
    let _turn = TURN.lock().await;
    let b = bench(CALM).await;
    let now = now_ms();
    let mut p = post("ig_60", Platform::Instagram, "image");
    let url = ig_url("s60", now, 3_600);
    p.cover_url = Some(url.clone());
    p.media = vec![slide("image", &url)];
    // No media at all: pending for the link hydration, nothing to fetch.
    let bare = post("ig_61", Platform::Instagram, "image");
    seed(&b.t, vec![p, bare]).await;
    let selection = {
        let db = b.t.state.user_db(ALICE).await.unwrap();
        db.read(|conn| {
            archive::select::select(
                conn,
                shelfy_core::repo::settings::ArchiveAssetTypes::default(),
                now,
                25,
                &std::collections::HashSet::new(),
            )
        })
        .unwrap()
    };
    assert_eq!(selection.due.len(), 1, "the cover; slide 0 is its item");
    assert_eq!(selection.due[0].key, "ig_60");
    assert_eq!(selection.due[0].slot, Slot::Cover);
    assert_eq!(selection.pending, [1, 0, 0]);
}

/// A cover whose slide 0 already stores an object is linked to it, with the
/// `g480` and the ThumbHash it lacked, without a request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cover_links_to_the_object_its_slide_0_stores() {
    let _turn = TURN.lock().await;
    let mut b = bench(CALM).await;
    let media = MediaStore::new(b.t.data_dir().users_dir())
        .user(ALICE)
        .unwrap();
    let staged = media
        .ingest(
            std::io::Cursor::new(jpeg(600, 800, 70)),
            IngestLimits::ARCHIVE_IMAGE,
        )
        .unwrap();
    let object_id =
        b.t.write(ALICE, move |tx| {
            let meta = ObjectMeta::new(Role::Image, Origin::Migration);
            let (id, _) = refs::publish_and_record(tx, &media, staged, &[], &meta, now_ms())?;
            Ok(id)
        })
        .await;
    let url = format!("https://{X}/media/l70.jpg");
    let mut p = post("x_70", Platform::Twitter, "image");
    p.cover_url = Some(url.clone());
    p.media = vec![NewMedia {
        object_id: Some(object_id),
        ..slide("image", &url)
    }];
    seed(&b.t, vec![p]).await;

    b.drain().await;
    let conn = b.library();
    assert_eq!(states(&conn)["x_70"], "done");
    let (cover_id, thumbhash) = cover(&conn, "x_70");
    assert_eq!(cover_id, Some(object_id));
    assert!(thumbhash.is_some());
    assert!(object(&conn, object_id).g480);
    assert!(b.fixture.hits().is_empty(), "nothing fetched");
}

/// The breakers of a new process start closed: the watcher's start-up pass
/// takes back what an open breaker handed to the extension before a
/// restart, and starts the drain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_takes_back_what_an_open_breaker_handed_over() {
    let _turn = TURN.lock().await;
    let mut b = bench(CALM).await;
    let url = format!("https://{X}/media/r80.jpg");
    b.fixture.route(
        X,
        "/media/r80?format=jpg&name=large",
        [image(jpeg(100, 100, 8))],
    );
    let mut handed = post("x_80", Platform::Twitter, "image");
    handed.archive_state = Some("client".to_owned());
    handed.media = vec![slide("image", &url)];
    seed(&b.t, vec![handed]).await;
    b.start();
    let watcher = archive::spawn_watcher(b.t.state.clone(), CancellationToken::new());
    let conn = b.library();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while states(&conn)["x_80"] != "done" {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            states(&conn)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    watcher.abort();
}
