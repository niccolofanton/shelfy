//! Users and libraries for the tests of the read API: a test-only stand-in for
//! authentication, a small fixture library with known answers, and a
//! synthetic library shaped like the reference one (plan Appendix C).
//!
//! All data is synthetic.

use axum::Router;
use axum::extract::Request;
use axum::middleware::{self, Next};
use rusqlite::{Connection, Transaction};
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::media::{self, NewMediaObject};
use shelfy_core::repo::posts::{self, AiLayer, NewMedia, NewPost};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_server::current_user::CurrentUser;

use super::TestState;

/// The user of most tests.
pub const ALICE: &str = "01J9Z3B8K4QW6TFX0V7G2N5RCA";
/// A second user, for isolation.
pub const BOB: &str = "01J9Z3B8K4QW6TFX0V7G2N5RCB";

/// 2026-10-02T00:00:00Z, the "now" of the fixtures.
pub const NOW: i64 = 1_790_899_200_000;
/// One day in milliseconds.
pub const DAY: i64 = 86_400_000;

impl TestState {
    /// The application as `user` sees it. A test-only layer stands in for
    /// authentication (T10): it inserts the user into every request, which is
    /// what the authentication layer does after checking a session.
    pub fn app_as(&self, user: &str) -> Router {
        let user = CurrentUser::new(user);
        self.app().layer(middleware::from_fn(
            move |mut request: Request, next: Next| {
                request.extensions_mut().insert(user.clone());
                next.run(request)
            },
        ))
    }

    /// Runs `f` in a write transaction on `user`'s library (created on first
    /// use) and returns its result.
    pub async fn write<T>(
        &self,
        user: &str,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, RepoError>,
    ) -> T {
        let db = self.state.user_db(user).await.expect("open the library");
        db.write(f).expect("write to the library")
    }
}

/// An object whose digest is derived from `n`; `g480` sets the rendition bit.
pub fn object(n: u8, ext: &str, g480: bool) -> NewMediaObject {
    let mut sha256 = [0_u8; 32];
    sha256[0] = n;
    sha256[31] = n.wrapping_mul(7);
    NewMediaObject {
        sha256,
        ext: ext.into(),
        mime: match ext {
            "mp4" => "video/mp4",
            "webp" => "image/webp",
            _ => "image/jpeg",
        }
        .into(),
        bytes: 10_000 + i64::from(n),
        width: Some(1080),
        height: Some(1350),
        duration_ms: (ext == "mp4").then_some(12_000),
        role: if ext == "mp4" { "video" } else { "image" }.into(),
        variants: i64::from(g480),
        origin: "server".into(),
    }
}

/// Lowercase hex of the digest of [`object`]`(n, …)`.
pub fn object_sha(n: u8) -> String {
    let mut sha256 = [0_u8; 32];
    sha256[0] = n;
    sha256[31] = n.wrapping_mul(7);
    sha256.iter().map(|b| format!("{b:02x}")).collect()
}

/// The ThumbHash bytes of the fixture's carousel.
pub const THUMBHASH: [u8; 5] = [0x1d, 0x08, 0x0a, 0x03, 0x82];

/// Ids of the fixture's collections.
#[derive(Clone, Copy, Debug)]
pub struct Fixture {
    /// "Lighting", an Instagram folder holding `ig_1001`.
    pub lighting: i64,
    /// "Inspiration", a manual collection holding `pin_3001`.
    pub inspiration: i64,
}

/// The fixture library, newest first, trash excluded.
pub const FIXTURE_NEWEST: [&str; 7] = [
    "web_00a1b2c3d4e5f6a7b8c9",
    "pin_3001",
    "m_01J9Z3B8K4QW6TFX0V7G2N5RCE",
    "x_2001",
    "ig_1001",
    "ig_1002",
    "x_2002",
];

/// The trashed post of the fixture.
pub const FIXTURE_TRASHED: &str = "ig_1003";

fn post(key: &str, platform: Platform, media_type: &str, days_ago: i64) -> NewPost {
    let native = key.split_once('_').map_or(key, |(_, n)| n);
    let mut post = NewPost::new(key, platform, native, media_type, NOW - DAY);
    post.posted_at = Some(NOW - days_ago * DAY);
    post
}

/// Fills a library with eight posts that exercise every filter: one per
/// platform and media type, a stored carousel with an AI layer, tags, two
/// collections and a trashed post.
///
/// # Errors
///
/// Repository errors.
pub fn fixture(conn: &Connection) -> Result<Fixture, RepoError> {
    let cover = media::upsert_object(conn, &object(1, "jpg", true), NOW)?;
    let slide = media::upsert_object(conn, &object(2, "jpg", false), NOW)?;
    let poster = media::upsert_object(conn, &object(3, "webp", true), NOW)?;

    let mut site = post("web_00a1b2c3d4e5f6a7b8c9", Platform::Web, "website", 1);
    site.native_id = "00a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9".into();
    site.caption = Some("Studio Example — product design".into());
    site.web_url = Some("http://studio.example.test".into());
    site.web_domain = Some("studio.example.test".into());
    site.web_final_url = Some("https://studio.example.test/".into());
    posts::insert(conn, &site, NOW)?;

    let mut pin = post("pin_3001", Platform::Pinterest, "image", 2);
    pin.caption = Some("Wooden chair design".into());
    let pin_id = posts::insert(conn, &pin, NOW)?;

    let mut manual = post("m_01J9Z3B8K4QW6TFX0V7G2N5RCE", Platform::Manual, "file", 3);
    manual.user_note = Some("Brief from the client".into());
    manual.user_tags = vec!["work".into()];
    posts::insert(conn, &manual, NOW)?;

    let mut tweet = post("x_2001", Platform::Twitter, "text", 5);
    tweet.caption = Some("Notes on typography and grid systems".into());
    tweet.user_tags = vec!["design".into()];
    posts::insert(conn, &tweet, NOW)?;

    let mut carousel = post("ig_1001", Platform::Instagram, "carousel", 10);
    carousel.shortcode = Some("C0ffeeAbCdE".into());
    carousel.post_url = Some("https://www.instagram.com/p/C0ffeeAbCdE/".into());
    carousel.author_username = Some("studio.example".into());
    carousel.caption = Some("Lampada in vetro soffiato".into());
    carousel.cover_object = Some(cover);
    carousel.thumbhash = Some(THUMBHASH.to_vec());
    carousel.archive_state = Some("done".into());
    carousel.user_tags = vec!["Lighting".into()];
    carousel.media = vec![
        NewMedia {
            kind: "image".into(),
            source_url: Some("https://cdn.example.test/a.jpg".into()),
            width: Some(1080),
            height: Some(1350),
            object_id: Some(slide),
            ..NewMedia::default()
        },
        NewMedia {
            kind: "video".into(),
            source_url: Some("https://cdn.example.test/b.jpg".into()),
            duration_ms: Some(12_000),
            object_id: Some(poster),
            ..NewMedia::default()
        },
    ];
    carousel.ai = Some(AiLayer {
        status: Some("done".into()),
        model: Some("model-a".into()),
        description: Some("A blown-glass table lamp".into()),
        category: Some("interior".into()),
        content_type: Some("product".into()),
        tags: vec!["glass".into(), "lamp".into()],
        entities: vec!["Murano".into()],
        keywords: vec!["blown glass".into()],
        analyzed_at: Some(NOW - DAY),
        ..AiLayer::default()
    });
    let carousel_id = posts::insert(conn, &carousel, NOW)?;

    let mut video = post("ig_1002", Platform::Instagram, "video", 20);
    video.caption = Some("Pasta fresca, ricetta della nonna".into());
    video.media = vec![NewMedia {
        kind: "video".into(),
        source_url: Some("https://cdn.example.test/c.jpg".into()),
        ..NewMedia::default()
    }];
    posts::insert(conn, &video, NOW)?;

    let mut images = post("x_2002", Platform::Twitter, "images", 30);
    images.caption = Some("Marble kitchen tiles".into());
    images.ai = Some(AiLayer {
        status: Some("done".into()),
        tags: vec!["kitchen".into()],
        ..AiLayer::default()
    });
    posts::insert(conn, &images, NOW)?;

    let mut old = post(FIXTURE_TRASHED, Platform::Instagram, "image", 100);
    old.caption = Some("Old lamp in the trash".into());
    let old_id = posts::insert(conn, &old, NOW)?;
    posts::trash(conn, &[old_id], NOW)?;

    let lighting = collections::create(
        conn,
        &NewCollection {
            name: "Lighting".into(),
            platform: Some(Platform::Instagram),
            external_id: Some("17900000000000001".into()),
            source_name: Some("lighting".into()),
            ..NewCollection::default()
        },
        NOW,
    )?;
    let inspiration = collections::create(
        conn,
        &NewCollection {
            name: "Inspiration".into(),
            color: Some("#FFAA00".into()),
            ..NewCollection::default()
        },
        NOW,
    )?;
    collections::add_posts(conn, &[carousel_id], &[lighting.id], NOW)?;
    collections::add_posts(conn, &[pin_id], &[inspiration.id], NOW)?;
    Ok(Fixture {
        lighting: lighting.id,
        inspiration: inspiration.id,
    })
}

/// Bob's library: two posts and a collection, none of them Alice's.
///
/// # Errors
///
/// Repository errors.
pub fn bob_library(conn: &Connection) -> Result<(), RepoError> {
    let mut lamp = post("ig_9001", Platform::Instagram, "image", 4);
    lamp.caption = Some("Bob's lamp in vetro".into());
    let id = posts::insert(conn, &lamp, NOW)?;
    let mut tweet = post("x_9002", Platform::Twitter, "text", 6);
    tweet.caption = Some("Bob writes about typography".into());
    posts::insert(conn, &tweet, NOW)?;
    let folder = collections::create(
        conn,
        &NewCollection {
            name: "Bob's folder".into(),
            ..NewCollection::default()
        },
        NOW,
    )?;
    collections::add_posts(conn, &[id], &[folder.id], NOW)?;
    Ok(())
}

/// A deterministic xorshift64* generator.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[usize::try_from(self.below(items.len() as u64)).expect("small index")]
    }
}

/// Words of the synthetic captions, Italian and English like the reference
/// library.
pub const WORDS: &[&str] = &[
    "lampada",
    "design",
    "arredo",
    "cucina",
    "minimal",
    "light",
    "studio",
    "poster",
    "typography",
    "ceramica",
    "vintage",
    "architecture",
    "sedia",
    "chair",
    "wood",
    "legno",
    "palette",
    "garden",
    "recipe",
    "ricetta",
    "travel",
    "mountain",
    "camera",
    "photography",
    "brand",
    "logo",
    "illustration",
    "workspace",
    "keyboard",
    "headphones",
    "sneakers",
    "fashion",
    "concrete",
    "marble",
    "glass",
    "vetro",
    "texture",
    "motion",
    "render",
    "interior",
    "kitchen",
    "neon",
    "sunset",
];

const STOPWORDS: &[&str] = &["di", "la", "per", "con", "the", "and", "of", "a", "in"];

fn sentence(rng: &mut Rng, words: u64) -> String {
    (0..words)
        .map(|_| {
            if rng.chance(30) {
                *rng.pick(STOPWORDS)
            } else {
                *rng.pick(WORDS)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `n` synthetic posts with distinct keys, in the platform and media mix of
/// the reference library (Appendix C): mostly Instagram and X, three quarters
/// videos, carousels of 2–10 slides, captions of a few words to a few hundred
/// characters, dates over ten years. About one post in ten shares its time
/// with the previous one, which exercises keyset ties. One post in three has
/// a stored cover with a ThumbHash.
pub fn synthetic_posts(n: usize, seed: u64, covers: &[i64]) -> Vec<NewPost> {
    let mut rng = Rng::new(seed);
    let mut last_ts = NOW;
    (0..n)
        .map(|i| {
            let (platform, prefix) = match rng.below(100) {
                0..=62 => (Platform::Instagram, "ig"),
                63..=95 => (Platform::Twitter, "x"),
                96..=97 => (Platform::Pinterest, "pin"),
                98 => (Platform::Web, "web"),
                _ => (Platform::Manual, "m"),
            };
            let media_type = match platform {
                Platform::Instagram => *rng.pick(&["video", "video", "video", "carousel", "image"]),
                Platform::Twitter => {
                    *rng.pick(&["video", "video", "video", "image", "images", "text"])
                }
                Platform::Pinterest => "image",
                Platform::Web => "website",
                Platform::Manual => "file",
            };
            let slides = match media_type {
                "carousel" => 2 + rng.below(9),
                "images" => 2 + rng.below(3),
                "text" | "website" | "file" => 0,
                _ => 1,
            };
            let native = format!("{}{i:06}", 100_000 + rng.below(900_000));
            let posted_at = if i > 0 && rng.chance(10) {
                last_ts
            } else {
                NOW - i64::try_from(rng.below(3650)).unwrap() * DAY
                    - i64::try_from(rng.below(86_400)).unwrap() * 1000
            };
            last_ts = posted_at;
            let mut post = NewPost::new(
                format!("{prefix}_{native}"),
                platform,
                native,
                media_type,
                NOW - i64::try_from(rng.below(30)).unwrap() * DAY,
            );
            post.posted_at = (!rng.chance(2)).then_some(posted_at);
            post.author_username = Some(format!("author_{}", rng.below(500)));
            post.author_name = Some(format!("Author {}", rng.below(500)));
            let words = match platform {
                Platform::Instagram => 4 + rng.below(80),
                _ => 2 + rng.below(30),
            };
            post.caption = Some(sentence(&mut rng, words));
            post.cover_url = Some(format!("https://cdn.example.test/{i}.jpg"));
            if !covers.is_empty() && rng.chance(33) {
                post.cover_object = Some(*rng.pick(covers));
                post.thumbhash = Some(vec![0x1d, 0x08, 0x0a, 0x03, 0x82, 0x77]);
                post.archive_state = Some("done".into());
            }
            if rng.chance(20) {
                post.user_tags = vec![(*rng.pick(WORDS)).to_owned()];
            }
            post.media = (0..slides)
                .map(|s| NewMedia {
                    kind: if media_type == "video" || (media_type == "carousel" && rng.chance(50)) {
                        "video".into()
                    } else {
                        "image".into()
                    },
                    source_url: Some(format!("https://cdn.example.test/{i}-{s}.jpg")),
                    width: Some(1080),
                    height: Some(1350),
                    ..NewMedia::default()
                })
                .collect();
            if platform == Platform::Web {
                post.web_url = Some(format!("https://site{i}.example.test/"));
                post.web_domain = Some(format!("site{i}.example.test"));
            }
            post
        })
        .collect()
}

/// Inserts `n` synthetic posts (and a few stored covers to share) and
/// returns their keys.
///
/// # Errors
///
/// Repository errors.
pub fn synthetic_library(conn: &Connection, n: usize, seed: u64) -> Result<Vec<String>, RepoError> {
    let covers = (10..20_u8)
        .map(|k| media::upsert_object(conn, &object(k, "jpg", k % 2 == 0), NOW))
        .collect::<Result<Vec<_>, _>>()?;
    synthetic_posts(n, seed, &covers)
        .iter()
        .map(|p| posts::insert(conn, p, NOW).map(|_| p.key.clone()))
        .collect()
}
