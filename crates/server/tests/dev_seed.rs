//! Seeds a synthetic library for local runs of the web app. It is not part of
//! the shipped binary, and `cargo test` skips it: it runs only on request.
//!
//! ```sh
//! SHELFY_SEED_DATA_DIR=/path/to/data SHELFY_SEED_EMAIL=owner@example.test \
//!   cargo test -p shelfy-server --test dev_seed -- --ignored
//! ```
//!
//! It creates the owner (or finds it) in the data directory the server will
//! use, then fills the owner's empty library with the fixture library of the
//! read API tests and `SHELFY_SEED_POSTS` synthetic posts (default 3,000,
//! shaped like the reference library):
//!
//! - a little over half the posts have a stored cover and stored slides: real
//!   PNG pictures in the media store, with their `g480` rendition and
//!   ThumbHash, served by `/media`;
//! - every cover and slide also has a remote URL, a small inline SVG picture
//!   (`data:` URL), so posts without stored media show a picture with no
//!   network access;
//! - a third of the posts carry an AI layer;
//! - seven folders (Instagram folders, Pinterest boards and manual ones) hold
//!   about a tenth of the posts.
//!
//! The fixture's own objects have no files, so their pictures are missing on
//! purpose. `admin synth` (P1-05) replaces this seed. All data is synthetic.

mod support;

use std::path::PathBuf;

use rusqlite::Connection;
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::posts::{self, AiLayer, NewPost};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_media::Rendition;
use shelfy_media::refs::{self, ObjectMeta, Origin, Role};
use shelfy_media::render;
use shelfy_media::store::{IngestLimits, MediaStore, UserMedia};
use shelfy_server::admin::owner::create_owner;
use shelfy_server::config::{Config, DataDir};
use shelfy_server::state::AppState;
use support::library::{self, NOW, Rng, WORDS};

const CATEGORIES: &[&str] = &[
    "design",
    "interior",
    "food",
    "travel",
    "technology",
    "fashion",
];
const CONTENT_TYPES: &[&str] = &["inspiration", "tutorial", "product", "reference", "recipe"];
/// Folders: name, color, platform and the platform's folder or board id.
const FOLDERS: &[(&str, &str, Option<Platform>, Option<&str>)] = &[
    (
        "Recipes",
        "#e91e63",
        Some(Platform::Instagram),
        Some("17900000000000101"),
    ),
    (
        "Interiors",
        "#4caf50",
        Some(Platform::Instagram),
        Some("17900000000000102"),
    ),
    (
        "Type & grids",
        "#2196f3",
        Some(Platform::Instagram),
        Some("17900000000000103"),
    ),
    (
        "Chairs",
        "#ff9800",
        Some(Platform::Pinterest),
        Some("880000000000000001"),
    ),
    (
        "Palettes",
        "#9c27b0",
        Some(Platform::Pinterest),
        Some("880000000000000002"),
    ),
    ("Client briefs", "#00bcd4", None, None),
    ("Travel 2027", "#8bc34a", None, None),
];
/// Distinct stored pictures, shared by the posts like real reposts are.
const PICTURES: u64 = 24;
/// Size of the stored pictures (4:5, like an Instagram portrait).
const PICTURE_SIZE: (u32, u32) = (360, 450);

#[tokio::test]
#[ignore = "writes into SHELFY_SEED_DATA_DIR; run it on request for local runs of the web app"]
async fn seed_dev_library() {
    let dir = PathBuf::from(env("SHELFY_SEED_DATA_DIR").expect("set SHELFY_SEED_DATA_DIR"));
    let email = env("SHELFY_SEED_EMAIL").expect("set SHELFY_SEED_EMAIL");
    let count = env("SHELFY_SEED_POSTS").map_or(3_000, |n| n.parse().expect("a post count"));

    std::fs::create_dir_all(&dir).expect("create the data directory");
    let data = DataDir::new(&dir).expect("a data directory");
    let owner = create_owner(&data, &email).expect("create or find the owner");
    let media = MediaStore::new(data.users_dir())
        .user(owner.user_id())
        .expect("the owner's media store");
    let state = tokio::task::spawn_blocking(move || AppState::open(Config::with_data_dir(data)))
        .await
        .expect("open task")
        .expect("open the state");
    let db = state
        .user_db(owner.user_id())
        .await
        .expect("open the library");
    let inserted = tokio::task::spawn_blocking(move || {
        db.write(|tx| -> Result<usize, RepoError> {
            let existing: i64 = tx.query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))?;
            assert_eq!(
                existing, 0,
                "the library is not empty: seed a fresh data directory"
            );
            library::fixture(tx)?;
            let pictures = store_pictures(tx, &media)?;
            seed(tx, count, &pictures)
        })
    })
    .await
    .expect("seed task")
    .expect("write the library");
    println!(
        "seeded {} posts for {email} (user {}) in {}",
        inserted + library::FIXTURE_NEWEST.len() + 1,
        owner.user_id(),
        dir.display()
    );
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// A stored picture: its `media_objects` id and its ThumbHash.
struct Picture {
    id: i64,
    thumbhash: Vec<u8>,
}

/// Stores [`PICTURES`] PNG pictures with their `g480` rendition.
fn store_pictures(conn: &Connection, media: &UserMedia) -> Result<Vec<Picture>, RepoError> {
    (0..PICTURES)
        .map(|n| {
            let (width, height) = PICTURE_SIZE;
            let png = gradient_png(width, height, n * 360 / PICTURES);
            let rendered = render::render_bytes(&png, Rendition::G480.spec()).expect("render");
            let staged = media
                .ingest(png.as_slice(), IngestLimits::ARCHIVE_IMAGE)
                .expect("stage a picture");
            let meta = ObjectMeta {
                width: Some(rendered.source_width),
                height: Some(rendered.source_height),
                ..ObjectMeta::new(Role::Image, Origin::Server)
            };
            let renditions = [(Rendition::G480, rendered.webp.as_slice())];
            let (id, _) = refs::publish_and_record(conn, media, staged, &renditions, &meta, NOW)?;
            Ok(Picture {
                id,
                thumbhash: rendered.thumbhash,
            })
        })
        .collect()
}

/// Inserts `count` synthetic posts with pictures and AI layers, and files
/// about a tenth of them into [`FOLDERS`].
fn seed(conn: &Connection, count: usize, pictures: &[Picture]) -> Result<usize, RepoError> {
    let mut rng = Rng::new(0x5EED);
    let mut posts = library::synthetic_posts(count, 0x7121, &[]);
    for (i, post) in posts.iter_mut().enumerate() {
        decorate(post, i, &mut rng, pictures);
    }
    let folders = FOLDERS
        .iter()
        .map(|&(name, color, platform, external_id)| {
            collections::create(
                conn,
                &NewCollection {
                    name: name.into(),
                    color: Some(color.into()),
                    platform,
                    external_id: external_id.map(Into::into),
                    source_name: platform.map(|_| name.to_lowercase()),
                },
                NOW,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    for post in &posts {
        let id = posts::insert(conn, post, NOW)?;
        if rng.chance(10) {
            // A platform's folders only hold that platform's posts.
            let fitting: Vec<i64> = folders
                .iter()
                .filter(|f| f.platform.is_none_or(|p| p == post.platform))
                .map(|f| f.id)
                .collect();
            if let Some(&folder) = fitting.get(usize::try_from(rng.below(7)).expect("small")) {
                collections::add_posts(conn, &[id], &[folder], NOW)?;
            }
        }
    }
    Ok(posts.len())
}

/// Gives a synthetic post pictures, stored ones for a little over half of the
/// posts, and an AI layer for a third of them. Text posts and files get no
/// picture (the generator's placeholder cover URLs do not resolve).
fn decorate(post: &mut NewPost, i: usize, rng: &mut Rng, pictures: &[Picture]) {
    let hue = rng.below(360);
    let label = post.key.clone();
    let pictured = !matches!(post.media_type.as_str(), "text" | "file");
    post.cover_url = pictured.then(|| svg_data_url(hue, &label));
    let stored = pictured && rng.chance(55);
    let first = usize::try_from(rng.below(PICTURES)).expect("small index");
    if stored {
        let cover = &pictures[first];
        post.cover_object = Some(cover.id);
        post.thumbhash = Some(cover.thumbhash.clone());
        post.archive_state = Some("done".into());
    }
    for (s, slide) in post.media.iter_mut().enumerate() {
        let shifted = (hue + 37 * s as u64) % 360;
        slide.source_url = Some(svg_data_url(shifted, &format!("{label} · {}", s + 1)));
        if stored {
            slide.object_id = Some(pictures[(first + s) % pictures.len()].id);
        }
    }
    if i.is_multiple_of(3) {
        let tags: Vec<String> = (0..2 + rng.below(4))
            .map(|_| (*rng.pick(WORDS)).to_owned())
            .collect();
        post.ai = Some(AiLayer {
            status: Some("done".into()),
            model: Some("synthetic".into()),
            description: Some(format!(
                "A {} about {} and {}.",
                rng.pick(&["photo", "video", "post", "board"]),
                tags[0],
                tags[1]
            )),
            save_reason: Some(format!("Reference for {}.", rng.pick(WORDS))),
            category: Some((*rng.pick(CATEGORIES)).into()),
            content_type: Some((*rng.pick(CONTENT_TYPES)).into()),
            keywords: vec![(*rng.pick(WORDS)).to_owned()],
            entities: vec![format!("Studio {}", rng.below(40))],
            tags,
            analyzed_at: Some(NOW - library::DAY),
            ..AiLayer::default()
        });
    }
}

/// A 4:5 gradient picture with a label, as an SVG `data:` URL.
fn svg_data_url(hue: u64, label: &str) -> String {
    let end = (hue + 50) % 360;
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='540' height='675' viewBox='0 0 540 675'>\
         <defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'>\
         <stop offset='0' stop-color='hsl({hue},65%,58%)'/>\
         <stop offset='1' stop-color='hsl({end},70%,28%)'/></linearGradient></defs>\
         <rect width='540' height='675' fill='url(#g)'/>\
         <text x='36' y='630' font-family='Helvetica,Arial,sans-serif' font-size='34' \
         fill='rgba(255,255,255,0.85)'>{label}</text></svg>"
    );
    let mut url = String::from("data:image/svg+xml;charset=utf-8,");
    for c in svg.chars() {
        match c {
            '%' => url.push_str("%25"),
            '#' => url.push_str("%23"),
            '<' => url.push_str("%3C"),
            '>' => url.push_str("%3E"),
            '"' => url.push_str("%22"),
            c => url.push(c),
        }
    }
    url
}

/// A `width` × `height` RGB picture: a diagonal gradient from `hue` with a
/// lighter disc, encoded as a PNG with stored (uncompressed) deflate blocks.
fn gradient_png(width: u32, height: u32, hue: u64) -> Vec<u8> {
    let (w, h) = (f64::from(width), f64::from(height));
    let hue = hue as f64;
    let mut raw = Vec::with_capacity((height * (1 + width * 3)) as usize);
    for y in 0..height {
        raw.push(0); // filter: none
        for x in 0..width {
            let (fx, fy) = (f64::from(x), f64::from(y));
            let t = (fx / w + fy / h) / 2.0;
            let (dx, dy) = (fx - w * 0.62, fy - h * 0.38);
            let disc = (dx * dx + dy * dy).sqrt() < w * 0.22;
            let lightness = if disc { 0.78 } else { 0.62 - 0.36 * t };
            raw.extend_from_slice(&hsl_to_rgb((hue + 50.0 * t) % 360.0, 0.62, lightness));
        }
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB, deflate, no filter, no interlace
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    chunk(&mut png, b"IEND", &[]);
    png
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let byte = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [byte(r), byte(g), byte(b)]
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&u32::try_from(data.len()).expect("chunk size").to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

/// A zlib stream of `data` in stored deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = data.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        out.push(u8::from(i + 1 == blocks.len()));
        let len = u16::try_from(block.len()).expect("block size");
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1_u32, 0_u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
