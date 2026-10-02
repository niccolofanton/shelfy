//! Shared helpers of the integration tests: fresh databases, a synthetic
//! library generator, the schema-fixture dataset and a deterministic SQL dump.
//!
//! All data is synthetic. Its shapes follow the aggregate counts of the
//! reference library (plan Appendix C): mostly Instagram and X, three quarters
//! video posts, carousels of 2–20 slides, captions of a few hundred characters
//! on Instagram and short ones on X, dates spread over ten years.

#![allow(dead_code)] // each test binary uses a different subset

use std::fmt::Write as _;
use std::path::Path;

use rusqlite::types::Value;
use rusqlite::{Connection, params};
use shelfy_core::repo::Platform;
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::media::{self, NewMediaObject};
use shelfy_core::repo::posts::{self, AiLayer, NewMedia, NewPost};
use shelfy_core::schema::{self, Kind};
use shelfy_core::search::index;

/// 2026-10-02T00:00:00Z, the "now" of the tests.
pub const NOW: i64 = 1_790_899_200_000;
/// One day in milliseconds.
pub const DAY: i64 = 86_400_000;

/// A fresh, fully migrated library database in memory.
pub fn library() -> Connection {
    let mut conn = Connection::open_in_memory().expect("open in-memory db");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    schema::migrate(&mut conn, Kind::Library).expect("migrate library");
    conn
}

/// A deterministic xorshift64* generator, so synthetic data is reproducible.
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

    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + i64::try_from(self.below(u64::try_from(hi - lo).unwrap())).unwrap()
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[usize::try_from(self.below(items.len() as u64)).unwrap()]
    }
}

const WORDS: &[&str] = &[
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
    "colori",
    "palette",
    "garden",
    "giardino",
    "recipe",
    "ricetta",
    "pasta",
    "travel",
    "viaggio",
    "mountain",
    "montagna",
    "camera",
    "film",
    "photography",
    "fotografia",
    "brand",
    "logo",
    "illustration",
    "workspace",
    "desk",
    "keyboard",
    "headphones",
    "cuffie",
    "sneakers",
    "fashion",
    "moda",
    "bookshelf",
    "libreria",
    "concrete",
    "marble",
    "glass",
    "vetro",
    "texture",
    "pattern",
    "motion",
    "3d",
    "render",
    "interior",
    "loft",
    "kitchen",
    "tile",
    "piastrelle",
    "neon",
    "sunset",
    "tramonto",
];

const STOP: &[&str] = &[
    "di", "la", "il", "per", "con", "the", "and", "of", "a", "in",
];

fn sentence(rng: &mut Rng, min_chars: usize, max_chars: usize) -> String {
    let target =
        min_chars + usize::try_from(rng.below((max_chars - min_chars + 1) as u64)).unwrap();
    let mut s = String::new();
    while s.chars().count() < target {
        if !s.is_empty() {
            s.push(' ');
        }
        let w = if rng.chance(30) {
            rng.pick(STOP)
        } else {
            rng.pick(WORDS)
        };
        s.push_str(w);
    }
    s
}

fn digits(rng: &mut Rng, n: usize) -> String {
    let mut s = String::with_capacity(n);
    s.push(char::from(b'1' + u8::try_from(rng.below(9)).unwrap()));
    while s.len() < n {
        s.push(char::from(b'0' + u8::try_from(rng.below(10)).unwrap()));
    }
    s
}

fn hex(rng: &mut Rng, n: usize) -> String {
    (0..n)
        .map(|_| char::from(b"0123456789abcdef"[usize::try_from(rng.below(16)).unwrap()]))
        .collect()
}

/// `n` synthetic posts with distinct keys. About one post in ten shares its
/// timestamp with the previous one, to exercise keyset ties.
pub fn synthetic_posts(n: usize, seed: u64) -> Vec<NewPost> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(n);
    let mut last_ts = NOW;
    for i in 0..n {
        let roll = rng.below(100);
        let platform = match roll {
            0..=62 => Platform::Instagram,
            63..=95 => Platform::Twitter,
            96..=97 => Platform::Pinterest,
            98 => Platform::Web,
            _ => Platform::Manual,
        };
        let (key, native_id) = match platform {
            Platform::Instagram => {
                let pk = digits(&mut rng, 19);
                (format!("ig_{pk}"), pk)
            }
            Platform::Twitter => {
                let id = digits(&mut rng, 19);
                (format!("x_{id}"), id)
            }
            Platform::Pinterest => {
                let id = digits(&mut rng, 18);
                (format!("pin_{id}"), id)
            }
            Platform::Web => {
                let h = hex(&mut rng, 40);
                (format!("web_{}", &h[..20]), h)
            }
            Platform::Manual => {
                let u = format!("01J{}", hex(&mut rng, 23).to_uppercase());
                (format!("m_{u}"), u)
            }
        };
        let media_type = match platform {
            Platform::Instagram => match rng.below(100) {
                0..=74 => "video",
                75..=95 => "carousel",
                _ => "image",
            },
            Platform::Twitter => match rng.below(100) {
                0..=74 => "video",
                75..=85 => "image",
                86..=91 => "images",
                _ => "text",
            },
            Platform::Pinterest => "image",
            Platform::Web => "website",
            Platform::Manual => *rng.pick(&["image", "file", "video"]),
        };
        let slides: i64 = match media_type {
            "carousel" => rng.range(2, 21),
            "images" => rng.range(2, 5),
            "text" => 0,
            _ => 1,
        };
        let caption = match platform {
            Platform::Instagram => Some(sentence(&mut rng, 20, 700)),
            Platform::Twitter => Some(sentence(&mut rng, 10, 280)),
            Platform::Pinterest | Platform::Manual => Some(sentence(&mut rng, 0, 120)),
            Platform::Web => Some(sentence(&mut rng, 40, 400)),
        }
        .filter(|c| !c.is_empty());
        let posted_at = if i > 0 && rng.chance(10) {
            last_ts
        } else {
            NOW - rng.range(0, 3650) * DAY - rng.range(0, DAY)
        };
        last_ts = posted_at;
        let mut post = NewPost::new(
            key,
            platform,
            native_id,
            media_type,
            NOW - rng.range(0, 30) * DAY,
        );
        post.posted_at = (!rng.chance(2)).then_some(posted_at);
        post.author_username = Some(format!("author_{}", rng.below(n as u64 / 2 + 1)));
        post.author_name = rng
            .chance(80)
            .then(|| format!("Author {}", rng.below(1000)));
        post.caption = caption;
        post.cover_url = Some(format!(
            "https://cdn.example.test/{}.jpg",
            hex(&mut rng, 16)
        ));
        post.media = (0..slides)
            .map(|_| NewMedia {
                kind: if media_type == "video" || (media_type == "carousel" && rng.chance(50)) {
                    "video".into()
                } else {
                    "image".into()
                },
                source_url: Some(format!(
                    "https://cdn.example.test/{}.jpg",
                    hex(&mut rng, 16)
                )),
                width: Some(1080),
                height: Some(*rng.pick(&[1080, 1350, 1920])),
                ..NewMedia::default()
            })
            .collect();
        if platform == Platform::Web {
            post.web_url = Some(format!("https://site{}.example.test/", rng.below(1000)));
            post.web_domain = Some(format!("site{}.example.test", rng.below(1000)));
        }
        out.push(post);
    }
    out
}

/// A post with only the required fields.
pub fn bare_post(key: &str, platform: Platform, posted_at: i64) -> NewPost {
    let native = key.split_once('_').map_or(key, |(_, n)| n);
    let mut post = NewPost::new(key, platform, native, "image", NOW);
    post.posted_at = Some(posted_at);
    post
}

/// Inserts posts, returning their ids.
pub fn insert_all(conn: &Connection, posts: &[NewPost]) -> Vec<i64> {
    posts
        .iter()
        .map(|p| posts::insert(conn, p, NOW).expect("insert post"))
        .collect()
}

/// An object with a digest derived from `n`.
pub fn object(n: u8, role: &str, ext: &str) -> NewMediaObject {
    let mut sha256 = [0_u8; 32];
    sha256[0] = n;
    sha256[31] = n.wrapping_mul(7);
    NewMediaObject {
        sha256,
        ext: ext.into(),
        mime: match ext {
            "mp4" => "video/mp4",
            "webp" => "image/webp",
            "png" => "image/png",
            _ => "image/jpeg",
        }
        .into(),
        bytes: 1000 + i64::from(n),
        width: Some(1080),
        height: Some(1350),
        duration_ms: (ext == "mp4").then_some(12_000),
        role: role.into(),
        variants: i64::from(role == "image" || role == "poster"),
        origin: "server".into(),
    }
}

// ── Schema fixtures ──────────────────────────────────────────────────────────

/// Fills a library at the latest schema with a small dataset that touches every
/// table, for the committed schema fixture.
pub fn fixture_library(conn: &Connection) {
    let cover = media::upsert_object(conn, &object(1, "image", "jpg"), NOW).unwrap();
    let slide = media::upsert_object(conn, &object(2, "image", "jpg"), NOW).unwrap();
    let poster = media::upsert_object(conn, &object(3, "poster", "webp"), NOW).unwrap();
    let video = media::upsert_object(conn, &object(4, "video", "mp4"), NOW).unwrap();
    let hero = media::upsert_object(conn, &object(5, "screenshot", "webp"), NOW).unwrap();
    let favicon = media::upsert_object(conn, &object(6, "favicon", "png"), NOW).unwrap();
    let band = media::upsert_object(conn, &object(7, "band", "webp"), NOW).unwrap();

    conn.execute(
        "INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status, created_at)
         VALUES ('cuffie', 'headphones', 'Headphones', 'accepted', ?1),
                ('lampade', 'lampada', 'lampada', 'proposed', ?1)",
        [NOW],
    )
    .unwrap();

    let mut carousel = bare_post("ig_3101", Platform::Instagram, NOW - 40 * DAY);
    carousel.shortcode = Some("C0ffeeAbCdE".into());
    carousel.media_type = "carousel".into();
    carousel.author_username = Some("studio.example".into());
    carousel.author_name = Some("Studio Example".into());
    carousel.caption = Some("Lampada da tavolo in vetro soffiato, studio di luce".into());
    carousel.cover_object = Some(cover);
    carousel.thumbhash = Some(vec![0x1d, 0x08, 0x0a, 0x03, 0x82]);
    carousel.archive_state = Some("done".into());
    carousel.user_note = Some("per il soggiorno".into());
    carousel.user_tags = vec!["Lighting".into(), "cuffie".into()];
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
            video_url: Some("https://cdn.example.test/b.mp4".into()),
            video_url_expires_at: Some(NOW + DAY),
            duration_ms: Some(12_000),
            object_id: Some(poster),
            video_object_id: Some(video),
            ..NewMedia::default()
        },
    ];
    carousel.ai = Some(AiLayer {
        status: Some("done".into()),
        provider: Some("openai".into()),
        model: Some("model-a".into()),
        schema_version: Some(2),
        description: Some("A blown-glass table lamp on a wooden desk".into()),
        save_reason: Some("lighting ideas".into()),
        language: Some("it".into()),
        category: Some("interior".into()),
        content_type: Some("product".into()),
        tags: vec!["lampada".into(), "Glass".into(), "cuffie".into()],
        general_tags: Some(vec!["lampada".into()]),
        specific_tags: Some(vec!["glass".into()]),
        entities: vec!["Murano".into()],
        keywords: vec!["blown glass".into(), "desk lamp".into()],
        web: None,
        analyzed_at: Some(NOW - DAY),
    });
    let carousel_id = posts::insert(conn, &carousel, NOW).unwrap();

    let mut tweet = bare_post("x_1800000000000000001", Platform::Twitter, NOW - 3 * DAY);
    tweet.media_type = "text".into();
    tweet.author_username = Some("writer".into());
    tweet.caption = Some("Notes on typography and grid systems".into());
    let tweet_id = posts::insert(conn, &tweet, NOW).unwrap();

    let mut pin = bare_post("pin_900000000000000001", Platform::Pinterest, NOW - 9 * DAY);
    pin.caption = Some("Marble kitchen tiles".into());
    pin.media = vec![NewMedia {
        kind: "image".into(),
        source_url: Some("https://i.pinimg.example.test/1200x/x.jpg".into()),
        ..NewMedia::default()
    }];
    let pin_id = posts::insert(conn, &pin, NOW).unwrap();

    let mut site = NewPost::new(
        "web_00a1b2c3d4e5f6a7b8c9",
        Platform::Web,
        "00a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9",
        "website",
        NOW - 5 * DAY,
    );
    site.post_url = Some("https://studio.example.test/".into());
    site.author_username = Some("studio.example.test".into());
    site.caption = Some("Studio Example — product design".into());
    site.web_url = Some("http://studio.example.test".into());
    site.web_domain = Some("studio.example.test".into());
    site.web_final_url = Some("https://studio.example.test/".into());
    site.media = vec![NewMedia {
        kind: "page".into(),
        source_url: Some("https://studio.example.test/".into()),
        label: Some("Home".into()),
        object_id: Some(hero),
        ..NewMedia::default()
    }];
    let site_id = posts::insert(conn, &site, NOW).unwrap();
    conn.execute(
        "INSERT INTO web_captures (post_id, captured_at, requested_url, final_url, status, partial,
                                   engine, viewport, title, palette_json, fonts_json, tech_json,
                                   awards_json, meta_json, pages_json, traits_json, hero_object,
                                   favicon_object, ai_snapshot_json, created_at)
         VALUES (?1, ?2, 'http://studio.example.test', 'https://studio.example.test/', 'done', 0,
                 'playwright', '1440x900', 'Studio Example',
                 '[{\"hex\":\"#111111\",\"role\":\"text\"}]', '[{\"family\":\"Inter\"}]',
                 '[\"react\"]', '[]', '{\"description\":\"Product design studio\"}',
                 '[{\"url\":\"https://studio.example.test/\",\"title\":\"Home\",\"digest\":\"Selected work\"}]',
                 '{\"scroll\":\"smooth\"}', ?3, ?4, NULL, ?2)",
        params![site_id, NOW - 5 * DAY, hero, favicon],
    )
    .unwrap();
    let capture_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO web_capture_assets (capture_id, page_index, role, seq, object_id, css_top, css_height)
         VALUES (?1, 0, 'band', 0, ?2, 0, 900)",
        params![capture_id, band],
    )
    .unwrap();
    conn.execute(
        "UPDATE posts SET current_capture_id = ?2 WHERE id = ?1",
        params![site_id, capture_id],
    )
    .unwrap();
    index::reindex_post(conn, site_id).unwrap();

    let mut manual = NewPost::new(
        "m_01J9Z3B8K4QW6TFX0V7G2N5RCE",
        Platform::Manual,
        "01J9Z3B8K4QW6TFX0V7G2N5RCE",
        "file",
        NOW - DAY,
    );
    manual.user_note = Some("Brief from the client".into());
    manual.user_tags = vec!["work".into()];
    let manual_id = posts::insert(conn, &manual, NOW).unwrap();

    let mut trashed = bare_post("ig_3102", Platform::Instagram, NOW - 100 * DAY);
    trashed.caption = Some("Old post in the trash".into());
    let trashed_id = posts::insert(conn, &trashed, NOW).unwrap();
    posts::trash(conn, &[trashed_id], NOW).unwrap();

    let folder = collections::create(
        conn,
        &NewCollection {
            name: "Lighting".into(),
            color: Some("#FFAA00".into()),
            platform: Some(Platform::Instagram),
            external_id: Some("17900000000000001".into()),
            source_name: Some("lighting".into()),
        },
        NOW,
    )
    .unwrap();
    let manual_collection = collections::create(
        conn,
        &NewCollection {
            name: "Inspiration".into(),
            ..NewCollection::default()
        },
        NOW,
    )
    .unwrap();
    collections::add_posts(conn, &[carousel_id, pin_id], &[folder.id], NOW).unwrap();
    collections::add_posts(
        conn,
        &[tweet_id, site_id, manual_id],
        &[manual_collection.id],
        NOW,
    )
    .unwrap();

    conn.execute(
        "INSERT INTO tag_cluster (label, label_norm, status, run_id, created_at, updated_at)
         VALUES ('Lighting', 'lighting', 'accepted', 1, ?1, ?1)",
        [NOW],
    )
    .unwrap();
    let cluster = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO tag_cluster_membership (tag_norm, cluster_id) VALUES ('lampada', ?1), ('glass', ?1)",
        [cluster],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tag_embeddings (tag_norm, model, dim, vec) VALUES ('lampada', 'e5-small', 2, X'0000803F00000000')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO settings (key, value_json, updated_at) VALUES ('language', '\"it\"', ?1)",
        [NOW],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notifications (kind, code, params_json, target, created_at, read_at)
         VALUES ('info', 'sync.done', '{\"inserted\":3}', NULL, ?1, NULL)",
        [NOW],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sync_runs (id, platform, source_kind, source_key, started_at, finished_at, status,
                                scanned, inserted, updated, known, error_code, client_version)
         VALUES ('01J9Z3B8K4QW6TFX0V7G2N5RCD', 'instagram', 'saved', 'all', ?1, ?2, 'done',
                 10, 3, 1, 6, NULL, '0.1.0')",
        params![NOW - 60_000, NOW],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sync_sources (platform, source_key, collection_id, last_run_at, newest_native_id)
         VALUES ('instagram', 'folder:17900000000000001', ?1, ?2, '3101')",
        params![folder.id, NOW],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ai_cache (kind, key_hash, value_json, created_at) VALUES ('suggest', X'0102', '[]', ?1)",
        [NOW],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('created_by', 'fixture')",
        [],
    )
    .unwrap();
}

/// Fills a control database at the latest schema with one row in every table.
pub fn fixture_control(conn: &Connection) {
    let user = "01J9Z3B8K4QW6TFX0V7G2N5RCA";
    conn.execute_batch(&format!(
        "INSERT INTO users (id, email, display_name, role, status, quota_bytes, capture_daily_limit,
                            usage_bytes, usage_updated_at, disclaimer_version,
                            disclaimer_accepted_at, created_at, last_seen_at)
         VALUES ('{user}', 'owner@example.test', 'Owner', 'owner', 'active', 0, 20, 1024, {NOW},
                 '2026-10', {NOW}, {NOW}, {NOW});
         UPDATE users SET privacy_version = '1', privacy_accepted_at = {NOW},
                          usage_media_bytes = 768, usage_db_bytes = 256 WHERE id = '{user}';
         INSERT INTO invites (token_hash, email, role, created_by, created_at, expires_at)
         VALUES (X'aa01', 'member@example.test', 'member', '{user}', {NOW}, {NOW} + 604800000);
         INSERT INTO passkeys (user_id, cred_id, passkey_json, label, created_at)
         VALUES ('{user}', X'cc01', '{{}}', 'Laptop', {NOW});
         INSERT INTO sessions (id_hash, user_id, created_at, expires_at, last_seen_at, user_agent)
         VALUES (X'dd01', '{user}', {NOW}, {NOW} + 2592000000, {NOW}, 'test');
         INSERT INTO magic_links (token_hash, user_id, purpose, expires_at)
         VALUES (X'ee01', '{user}', 'login', {NOW} + 900000);
         INSERT INTO api_tokens (id, user_id, kind, token_hash, label, scopes, created_at)
         VALUES ('01J9Z3B8K4QW6TFX0V7G2N5RCB', '{user}', 'extension', X'ff01', 'Chrome',
                 'ingest tasks uploads lookup', {NOW});
         INSERT INTO api_tokens (id, user_id, kind, token_hash, label, scopes, created_at,
                                 expires_at)
         VALUES ('01J9Z3B8K4QW6TFX0V7G2N5RCD', '{user}', 'migrate', X'ff02', 'migration',
                 'migrate', {NOW}, {NOW} + 604800000);
         INSERT INTO pairing_codes (code_hash, user_id, kind, expires_at)
         VALUES (X'ab01', '{user}', 'extension', {NOW} + 60000);
         INSERT INTO provider_keys (user_id, provider_id, key_version, nonce, ciphertext, last4, created_at)
         VALUES ('{user}', 'openai', 1, X'000102', X'0a0b0c', 'abcd', {NOW});
         INSERT INTO jobs (user_id, kind, dedupe_key, state, priority, payload_json, attempts,
                           max_attempts, run_at, created_at, updated_at)
         VALUES ('{user}', 'archive.drain', 'archive.drain', 'queued', 100, '{{}}', 0, 5,
                 {NOW}, {NOW}, {NOW});
         INSERT INTO queue_state (user_id, kind, paused) VALUES ('{user}', 'ai.drain', 1);
         INSERT INTO uploads (id, user_id, purpose, length, upload_offset, meta_json, created_at, expires_at)
         VALUES ('01J9Z3B8K4QW6TFX0V7G2N5RCC', '{user}', 'bookmark', 2048, 1024, '{{}}', {NOW},
                 {NOW} + 86400000);
         INSERT INTO idempotency (user_id, key, status, body, created_at)
         VALUES ('{user}', 'req-1', 202, X'7b7d', {NOW});
         INSERT INTO usage_daily (user_id, day, ai_calls, ai_in_tokens, ai_out_tokens, captures,
                                  ingest_items, bytes_in)
         VALUES ('{user}', '2026-10-02', 3, 1200, 300, 1, 40, 65536);
         INSERT INTO audit_log (at, actor_user_id, action, target, ip_hash, meta_json)
         VALUES ({NOW}, '{user}', 'invite.create', 'member@example.test', X'0f', '{{}}');
         INSERT INTO feature_flags (key, value_json, updated_at) VALUES ('capture', 'true', {NOW});"
    ))
    .unwrap();
}

/// A deterministic SQL dump: schema in creation order (FTS shadow tables
/// left to `CREATE VIRTUAL TABLE`), then every table's rows in key order, then
/// the FTS rows rebuilt from the documents, then the header pragmas.
pub fn dump(conn: &Connection, kind: Kind, title: &str) -> String {
    let mut out = String::new();
    writeln!(out, "-- {title}").unwrap();
    writeln!(
        out,
        "-- Generated by `SHELFY_BLESS=1 cargo test -p shelfy-core --test schema`; do not edit."
    )
    .unwrap();
    out.push_str("PRAGMA foreign_keys = OFF;\nBEGIN;\n");

    let shadow: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_list WHERE schema = 'main' AND type = 'shadow'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let objects: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY rowid",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (_, name, sql) in &objects {
        if !shadow.contains(name) {
            writeln!(out, "{sql};").unwrap();
        }
    }

    for (kind_of, name, sql) in &objects {
        if kind_of != "table" || shadow.contains(name) || sql.starts_with("CREATE VIRTUAL") {
            continue;
        }
        let without_rowid: bool = conn
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                [name],
                |r| r.get(0),
            )
            .unwrap();
        let order = if without_rowid {
            let pk: Vec<String> = conn
                .prepare("SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")
                .unwrap()
                .query_map([name], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            pk.iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            "rowid".to_owned()
        };
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY {order}"))
            .unwrap();
        let ncols = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let values: Vec<String> = (0..ncols)
                .map(|i| literal(&row.get::<_, Value>(i).unwrap()))
                .collect();
            writeln!(
                out,
                "INSERT INTO \"{name}\" VALUES ({});",
                values.join(", ")
            )
            .unwrap();
        }
    }

    if kind == Kind::Library {
        // Only live posts are indexed (see `search::index`).
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM posts WHERE deleted_at IS NULL ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for id in ids {
            let Some(doc) = index::document(conn, id).unwrap() else {
                continue;
            };
            let cols = [
                &doc.tags,
                &doc.keywords,
                &doc.entities,
                &doc.description,
                &doc.note,
                &doc.caption,
                &doc.author,
                &doc.web_text,
            ];
            if cols.iter().all(|c| c.is_empty()) {
                continue;
            }
            let values: Vec<String> = cols
                .iter()
                .map(|c| literal(&Value::Text((*c).clone())))
                .collect();
            writeln!(
                out,
                "INSERT INTO posts_fts (rowid, tags, keywords, entities, description, note, caption, author, web_text) VALUES ({id}, {});",
                values.join(", ")
            )
            .unwrap();
        }
    }

    out.push_str("COMMIT;\n");
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let application_id: i64 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .unwrap();
    writeln!(out, "PRAGMA application_id = {application_id};").unwrap();
    writeln!(out, "PRAGMA user_version = {user_version};").unwrap();
    out
}

fn literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => format!("{f:?}"),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Blob(b) => {
            let mut s = String::from("X'");
            for byte in b {
                write!(s, "{byte:02X}").unwrap();
            }
            s.push('\'');
            s
        }
    }
}

/// Loads a dump into a fresh in-memory database (foreign keys back on).
pub fn load(sql: &str) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(sql).unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    conn
}

/// Path of a committed fixture.
pub fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/schema")
        .join(name)
}

/// Whether the tests should rewrite fixtures instead of checking them.
pub fn blessing() -> bool {
    std::env::var_os("SHELFY_BLESS").is_some_and(|v| v == "1")
}
