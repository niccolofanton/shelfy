//! The bundle of a synthetic desktop library: every §4.2 row kind, files
//! present, missing, duplicated and of an unsupported type.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde_json::Value;
use shelfy_core::ids::ig::MediaPk;
use shelfy_core::ids::web::legacy_post_id;
use shelfy_core::legacy::LegacyDb;
use shelfy_core::legacy::fixture::DESKTOP_SCHEMA_CURRENT;
use shelfy_migrate::bundle::{self, Bundle, BundleOptions, SUMMARY_META_KEY};
use shelfy_migrate::files::orphan_paths;
use shelfy_migrate::plan::{PlanOptions, plan_with_mapping};

/// 2026-10-02T00:00:00Z.
const NOW: i64 = 1_790_899_200_000;
/// Seconds of NOW, as the desktop stores times.
const NOW_S: i64 = NOW / 1000;
const PK: &str = "3191575067010950169";
/// An `oe` signature a day before / after NOW.
const EXPIRED: &str = "oe=6ABDA280";
const VALID: &str = "oe=6AC04580";

/// Bytes with a JPEG / PNG / WebP / MP4 / PDF / HEIC header, distinct per seed.
fn file(magic: &[u8], seed: u8) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend(std::iter::repeat_n(seed, 32));
    bytes
}
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\0\x10JFIF\0";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
const WEBP: &[u8] = b"RIFF\x24\0\0\0WEBPVP8 ";
const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isom";
const PDF: &[u8] = b"%PDF-1.7\n";
const HEIC: &[u8] = b"\0\0\0\x18ftypheic\0\0\0\0mif1";

struct Desktop {
    _dir: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    conn: Connection,
}

impl Desktop {
    fn new() -> Desktop {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Shelfy");
        for sub in ["thumbnails", "images", "videos", "web", "previews"] {
            fs::create_dir_all(root.join("assets").join(sub)).unwrap();
        }
        let db = root.join("shelfy.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(DESKTOP_SCHEMA_CURRENT).unwrap();
        Desktop {
            _dir: dir,
            root,
            db,
            conn,
        }
    }

    /// Writes an asset; returns the path the desktop stores (from another
    /// machine's userData, as a copied library has).
    fn asset(&self, relative: &str, bytes: &[u8]) -> String {
        fs::write(self.root.join("assets").join(relative), bytes).unwrap();
        stored(relative)
    }

    fn bundle(
        &self,
        with_videos: bool,
        out: &Path,
    ) -> (Bundle, shelfy_migrate::report::PlanReport) {
        let legacy = LegacyDb::open(&self.db).unwrap();
        let (report, mapping) = plan_with_mapping(
            &legacy,
            &PlanOptions {
                media_root: Some(self.root.clone()),
                redact: true,
                now_ms: NOW,
            },
        )
        .unwrap();
        assert!(report.verdict.pass, "{:?}", report.errors);
        let bundle = bundle::build(
            &legacy,
            &mapping,
            out,
            &BundleOptions {
                with_videos,
                snapshot: false,
                now_ms: NOW,
                settings: report.settings.clone(),
            },
        )
        .unwrap();
        (bundle, report)
    }
}

/// The site's palette, fonts, tech and awards, as the desktop stores them.
const SITE_PALETTE: &str = r##"[{"hex":"#0A0A0A","weight":0.62},{"hex":"#F2EFE9","weight":0.3}]"##;
const SITE_FONTS: &str = r#"[{"family":"Inter","weights":[400,600],"role":"body"}]"#;
const SITE_TECH: &str = r#"["Next.js","GSAP"]"#;
const SITE_AWARDS: &str = r#"[{"source":"awwwards","kind":"SOTD","date":"2024-03-01"}]"#;

/// The path of an asset in the stored form.
fn stored(relative: &str) -> String {
    format!("/Users/someone/Library/Application Support/Shelfy/assets/{relative}")
}

fn library() -> Desktop {
    let d = Desktop::new();
    let c = &d.conn;
    let shortcode = MediaPk::parse_decimal(PK).unwrap().to_shortcode();
    let cover = d.asset("thumbnails/instagram-1.jpg", &file(JPEG, 1));
    // Slide 0 is the same image as the cover: one object.
    let slide0 = d.asset("images/instagram-1-0.jpg", &file(JPEG, 1));
    let slide1 = d.asset("images/instagram-1-1.png", &file(PNG, 2));
    // A: a carousel with its cover, both slides, AI and a note.
    c.execute(
        "INSERT INTO posts (id, platform, shortcode, post_url, author_username, text, thumbnail_url,
           media_type, timestamp, thumbnail_path, image_path, imported_at, ai_description, ai_tags,
           ai_status, ai_model, ai_analyzed_at, ai_entities, user_note, thumb_blur)
         VALUES (?1, 'instagram', ?2, 'https://www.instagram.com/p/x/', 'someone', 'A caption',
           ?3, 'carousel', '2024-01-01T10:00:00.000Z', ?4, ?5, ?6, 'A lamp', '[\"Lamp\",\"Glass\"]',
           'done', 'local-model', ?6, '[\"Studio\"]', 'first note', 'data:image/jpeg;base64,AA')",
        params![
            format!("{PK}_25025320"),
            shortcode,
            format!("https://scontent.cdninstagram.com/a.jpg?{VALID}"),
            cover,
            slide0,
            NOW_S
        ],
    )
    .unwrap();
    for (position, local) in [(0, &slide0), (1, &slide1)] {
        c.execute(
            "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
             VALUES (?1, ?2, 'image', 'https://scontent.cdninstagram.com/s.jpg', ?3)",
            params![format!("{PK}_25025320"), position, local],
        )
        .unwrap();
    }
    for (norm, form, tier) in [("lamp", "Lamp", "general"), ("glass", "Glass", "specific")] {
        c.execute(
            "INSERT INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES (?1, ?2, ?3, ?4)",
            params![format!("{PK}_25025320"), norm, form, tier],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO post_entities (post_id, ent_norm, ent_form) VALUES (?1, 'studio', 'Studio')",
        [format!("{PK}_25025320")],
    )
    .unwrap();
    // A': the same post keyed by its pk: folded into A.
    c.execute(
        "INSERT INTO posts (id, platform, shortcode, media_type, imported_at, user_note, user_tags)
         VALUES (?1, 'instagram', ?2, 'carousel', ?3, 'second note', '[\"mine\"]')",
        params![PK, shortcode, NOW_S - 10],
    )
    .unwrap();

    // B: a video post with its poster and a kept video.
    let poster = d.asset("thumbnails/instagram-2.jpg", &file(JPEG, 3));
    let video = d.asset("videos/instagram-2.mp4", &file(MP4, 4));
    c.execute(
        "INSERT INTO posts (id, platform, media_type, thumbnail_url, thumbnail_path, video_path,
           timestamp, imported_at)
         VALUES ('2_1', 'instagram', 'video', ?1, ?2, ?3, '2024-02-01T00:00:00Z', ?4)",
        params![
            format!("https://scontent.cdninstagram.com/v.jpg?{EXPIRED}"),
            poster,
            video,
            NOW_S
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
         VALUES ('2_1', 0, 'video', 'https://scontent.cdninstagram.com/v.jpg', ?1)",
        [&video],
    )
    .unwrap();
    // B2, B3: no local cover; a still valid and an expired cover URL.
    for (id, sig) in [("3_1", VALID), ("4_1", EXPIRED)] {
        c.execute(
            "INSERT INTO posts (id, platform, media_type, thumbnail_url, imported_at)
             VALUES (?1, 'instagram', 'image', ?2, ?3)",
            params![
                id,
                format!("https://scontent.cdninstagram.com/c.jpg?{sig}"),
                NOW_S
            ],
        )
        .unwrap();
    }
    // G: the kept video is gone from disk (OI-6).
    let poster_g = d.asset("thumbnails/instagram-5.jpg", &file(JPEG, 5));
    c.execute(
        "INSERT INTO posts (id, platform, media_type, thumbnail_path, video_path, imported_at)
         VALUES ('5_1', 'instagram', 'video', ?1, ?2, ?3)",
        params![poster_g, stored("videos/instagram-5.mp4"), NOW_S],
    )
    .unwrap();
    c.execute(
        "INSERT INTO post_media (post_id, position, media_type, source_url)
         VALUES ('5_1', 0, 'video', 'https://scontent.cdninstagram.com/g.jpg')",
        [],
    )
    .unwrap();
    // A video post whose slide file is a still, not a video: it is the poster.
    let still = d.asset("images/instagram-8-0.jpg", &file(JPEG, 15));
    c.execute(
        "INSERT INTO posts (id, platform, media_type, image_path, imported_at)
         VALUES ('8_1', 'instagram', 'video', ?1, ?2)",
        params![still, NOW_S],
    )
    .unwrap();
    c.execute(
        "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
         VALUES ('8_1', 0, 'video', 'https://scontent.cdninstagram.com/8.jpg', ?1)",
        [&still],
    )
    .unwrap();
    // H: a cover of a type the store does not take.
    let heic = d.asset("thumbnails/instagram-6.heic", &file(HEIC, 6));
    c.execute(
        "INSERT INTO posts (id, platform, media_type, thumbnail_url, thumbnail_path, imported_at)
         VALUES ('6_1', 'instagram', 'image', 'https://scontent.cdninstagram.com/h.jpg', ?1, ?2)",
        params![heic, NOW_S],
    )
    .unwrap();

    // C: an X post with only a preview, a broken status URL and a pending slide.
    let preview = d.asset("previews/twitter-7.jpg", &file(JPEG, 7));
    c.execute(
        "INSERT INTO posts (id, platform, post_url, media_type, thumbnail_url, preview_path,
           imported_at)
         VALUES ('1800000000000000001', 'twitter', 'https://x.com//status/1800000000000000001',
           'image', 'https://pbs.twimg.com/media/a.jpg', ?1, ?2)",
        params![preview, NOW_S],
    )
    .unwrap();
    c.execute(
        "INSERT INTO post_media (post_id, position, media_type, source_url)
         VALUES ('1800000000000000001', 0, 'image', 'https://pbs.twimg.com/media/a.jpg')",
        [],
    )
    .unwrap();
    // D: a text tweet.
    c.execute(
        "INSERT INTO posts (id, platform, media_type, text, imported_at)
         VALUES ('1800000000000000002', 'twitter', 'text', 'Only words', ?1)",
        [NOW_S],
    )
    .unwrap();

    // E: a captured site, with an older version.
    let hero = d.asset("web/1-hero.webp", &file(WEBP, 8));
    let band = d.asset("web/1-c0.webp", &file(WEBP, 9));
    let favicon = d.asset("web/fav.webp", &file(WEBP, 10));
    let old_hero = d.asset("web/0-hero.webp", &file(WEBP, 11));
    let pages = serde_json::json!([{
        "url": "https://studio.example.test/", "title": "Home",
        "screenshotPath": hero, "hero": {"path": hero, "width": 2880},
        "chunks": [{"screenshotPath": band, "top": 900, "cssHeight": 1200}],
        "contentText": "Selected work"
    }])
    .to_string();
    let meta = serde_json::json!({
        "title": "Studio", "favicon": favicon, "traits": {"scroll": "smooth"},
        "capture": {"engine": "playwright", "viewport": {"width": 1440, "height": 900}, "skipped": []}
    })
    .to_string();
    let url = "https://studio.example.test/";
    c.execute(
        "INSERT INTO posts (id, platform, media_type, web_url, web_final_url, post_url, web_domain,
           thumbnail_path, web_pages_json, web_meta_json, web_captured_at, imported_at,
           web_palette_json, web_fonts_json, web_tech_json, web_awards_json)
         VALUES (?1, 'web', 'website', ?2, ?2, ?2, 'studio.example.test', ?3, ?4, ?5, ?6, ?6,
           ?7, ?8, ?9, ?10)",
        params![
            legacy_post_id(url),
            url,
            hero,
            pages,
            meta,
            NOW_S,
            SITE_PALETTE,
            SITE_FONTS,
            SITE_TECH,
            SITE_AWARDS
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
         VALUES (?1, 0, 'image', ?2, ?3)",
        params![legacy_post_id(url), url, hero],
    )
    .unwrap();
    let old_pages = serde_json::json!([{"url": url, "screenshotPath": old_hero}]).to_string();
    c.execute(
        "INSERT INTO web_snapshots (post_id, captured_at, title, web_pages_json, ai_description,
           created_at, web_palette_json, web_fonts_json, web_tech_json, web_awards_json)
         VALUES (?1, ?2, 'Studio, before', ?3, 'An older look', ?2, '[{\"hex\":\"#FFFFFF\"}]',
           'not json', NULL, '')",
        params![legacy_post_id(url), NOW_S - 86_400, old_pages],
    )
    .unwrap();

    // F: a manual PDF bookmark: the original's path is its slide's source.
    let original = d.asset("images/manual-brief.pdf", &file(PDF, 12));
    let manual_preview = d.asset("thumbnails/manual-brief.webp", &file(WEBP, 13));
    c.execute(
        "INSERT INTO posts (id, platform, media_type, thumbnail_path, imported_at)
         VALUES ('manual:4f6c', 'manual', 'file', ?1, ?2)",
        params![manual_preview, NOW_S],
    )
    .unwrap();
    c.execute(
        "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
         VALUES ('manual:4f6c', 0, 'file', ?1, ?2)",
        params![original, manual_preview],
    )
    .unwrap();

    // Collections: a folder linked twice (merged), a manual one with a bad color.
    c.execute_batch(
        "INSERT INTO collections (id, name, color, platform, external_id, ig_name, created_at)
           VALUES (1, 'Recipes', '#AABBCC', 'instagram', '17890', 'Recipes', 1700000000);
         INSERT INTO collections (id, name, color, platform, external_id, created_at)
           VALUES (2, 'Recipes again', '#112233', 'instagram', '17890', 1700000001);
         INSERT INTO collections (id, name, color, created_at) VALUES (3, ' ', 'red', 1700000002);",
    )
    .unwrap();
    for (post, collection) in [
        (format!("{PK}_25025320"), 1),
        (PK.to_owned(), 2),
        ("1800000000000000001".to_owned(), 3),
    ] {
        c.execute(
            "INSERT INTO post_collections (post_id, collection_id, added_at) VALUES (?1, ?2, ?3)",
            params![post, collection, NOW_S],
        )
        .unwrap();
    }
    // Aliases and clusters; one membership of a cluster that does not exist.
    c.execute_batch(
        "INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status)
           VALUES ('lamps', 'lamp', 'Lamp', 'accepted');
         INSERT INTO tag_cluster (id, label, label_norm, status, run_id, created_at, updated_at)
           VALUES (7, 'Lighting', 'lighting', 'accepted', 1700000000000, 1700000000, 1700000001);
         INSERT INTO tag_cluster_membership (tag_norm, cluster_id) VALUES ('lamp', 7);
         PRAGMA foreign_keys = OFF;
         INSERT INTO tag_cluster_membership (tag_norm, cluster_id) VALUES ('ghost', 99);",
    )
    .unwrap();
    // A file no row references (OI-11).
    d.asset("images/orphan.jpg", &file(JPEG, 14));
    d
}

fn rows<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> Vec<T> {
    conn.prepare(sql)
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn one<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn every_kind_of_desktop_row_lands_in_the_bundle() {
    let desktop = library();
    let out = tempfile::tempdir().unwrap();
    let (bundle, plan) = desktop.bundle(false, out.path());
    let s = &bundle.summary;

    // Posts: the IG duplicate folds into its kept row.
    assert_eq!(s.posts.read["instagram"], 8);
    assert_eq!(s.posts.written["instagram"], 7);
    assert_eq!(s.posts.merged, 1);
    assert_eq!(s.posts.written["twitter"], 2);
    assert_eq!(s.posts.written["web"], 1);
    assert_eq!(s.posts.written["manual"], 1);
    assert_eq!(plan.posts.by_platform["instagram"], 8);

    let db = Connection::open(&bundle.db_path).unwrap();
    let key = format!("ig_{PK}");
    let (note, tags): (String, String) = db
        .query_row(
            "SELECT user_note, user_tags_json FROM posts WHERE key = ?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(note, "first note\n\nsecond note");
    assert_eq!(tags, r#"["mine"]"#);
    let (provider, schema, status): (String, i64, String) = db
        .query_row(
            "SELECT ai_provider, ai_schema_version, ai_status FROM posts WHERE key = ?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (provider.as_str(), schema, status.as_str()),
        ("desktop-local", 1, "done")
    );
    // Tag rows verbatim, tiers included; the manual tag rebuilt from user_tags
    // is replaced by the desktop rows, which had none.
    let tag_rows: Vec<String> = rows(
        &db,
        "SELECT tag_norm || ':' || source || ':' || coalesce(tier, '-') FROM post_tags ORDER BY 1",
    );
    assert_eq!(tag_rows, ["glass:ai:specific", "lamp:ai:general"]);
    assert_eq!(one::<i64>(&db, "SELECT count(*) FROM post_entities"), 1);
    assert_eq!(s.rows.post_tags, 2);

    // Dates, URLs and repairs.
    assert_eq!(
        one::<String>(
            &db,
            "SELECT post_url FROM posts WHERE key = 'x_1800000000000000001'"
        ),
        "https://x.com/i/status/1800000000000000001"
    );
    assert_eq!(s.repairs.x_status_urls, 1);
    assert_eq!(
        one::<i64>(
            &db,
            &format!("SELECT posted_at FROM posts WHERE key = '{key}'")
        ),
        1_704_103_200_000
    );

    // Files: present, missing (the gone video), unsupported (HEIC), excluded
    // (the kept video without --with-videos); the cover and slide 0 are one object.
    assert_eq!(s.files.missing, 1);
    assert_eq!(s.files.missing_by_class["video"], 1);
    assert_eq!(s.files.unsupported, 1);
    assert_eq!(s.files.videos_excluded, 1);
    assert_eq!(s.repairs.videos_missing, 1);
    let objects: i64 = one(&db, "SELECT count(*) FROM media_objects");
    assert_eq!(objects as usize, bundle.objects.len());
    assert!(bundle.objects.iter().all(|o| o.ext != "mp4"));
    assert_eq!(
        rows::<String>(
            &db,
            "SELECT DISTINCT origin || '/' || variants FROM media_objects"
        ),
        ["migration/0"]
    );
    let roles: Vec<String> = rows(&db, "SELECT role FROM media_objects ORDER BY role");
    for role in [
        "band",
        "favicon",
        "file",
        "image",
        "poster",
        "preview",
        "screenshot",
    ] {
        assert!(roles.iter().any(|r| r == role), "{role} in {roles:?}");
    }
    let (cover, slide0): (i64, i64) = db
        .query_row(
            "SELECT p.cover_object, m.object_id FROM posts p
             JOIN post_media m ON m.post_id = p.id AND m.position = 0 WHERE p.key = ?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cover, slide0, "the cover and slide 0 are the same bytes");
    // A still stored as a video slide's file is its poster, not a kept video.
    let (cover, poster, kept): (i64, i64, Option<i64>) = db
        .query_row(
            "SELECT p.cover_object, m.object_id, m.video_object_id FROM posts p
             JOIN post_media m ON m.post_id = p.id AND m.position = 0 WHERE p.key = 'ig_8'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((poster, kept), (cover, None));
    assert_eq!(
        one::<i64>(
            &db,
            "SELECT count(*) FROM post_media WHERE video_object_id IS NOT NULL"
        ),
        0,
        "no kept video without --with-videos"
    );

    // Archive work per post (OI-6, OI-7).
    let states: Vec<String> = rows(
        &db,
        "SELECT key || '=' || archive_state FROM posts ORDER BY key",
    );
    for expected in [
        format!("ig_{PK}=done"),
        "ig_2=done".to_owned(),
        "ig_3=pending".to_owned(),
        "ig_4=client".to_owned(),
        "ig_5=done".to_owned(),
        "ig_6=pending".to_owned(),
        "ig_8=done".to_owned(),
        "x_1800000000000000001=partial".to_owned(),
        "x_1800000000000000002=done".to_owned(),
    ] {
        assert!(states.contains(&expected), "{expected} in {states:?}");
    }
    assert_eq!(
        (
            s.covers.ig_valid,
            s.covers.ig_expired,
            s.covers.ig_no_expiry
        ),
        (1, 1, 1)
    );
    assert_eq!(s.covers.none, 1, "the text tweet");

    // The site: its current version and the older one, paths stripped.
    assert_eq!(one::<i64>(&db, "SELECT count(*) FROM web_captures"), 2);
    let (current, hero, favicon, viewport, traits): (i64, i64, i64, String, String) = db
        .query_row(
            "SELECT p.current_capture_id, c.hero_object, c.favicon_object, c.viewport,
                    c.traits_json
             FROM posts p JOIN web_captures c ON c.id = p.current_capture_id
             WHERE p.platform = 'web'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert!(current > 0 && hero > 0 && favicon > 0);
    assert_eq!(viewport, "1440x900");
    assert_eq!(traits, r#"{"scroll":"smooth"}"#);
    for json in rows::<String>(
        &db,
        "SELECT coalesce(pages_json, '') || coalesce(meta_json, '') FROM web_captures",
    ) {
        assert!(!json.contains("/assets/"), "{json}");
    }
    let snapshot: String = one(
        &db,
        "SELECT ai_snapshot_json FROM web_captures WHERE ai_snapshot_json IS NOT NULL",
    );
    let snapshot: Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snapshot["description"], "An older look");
    assert_eq!(s.rows.web_captures, 2);
    // Palette, fonts, tech and awards: verbatim on the current version; the
    // older one's fonts are not JSON → NULL, counted on both sides (F12).
    let site: (String, String, String, String) = db
        .query_row(
            "SELECT c.palette_json, c.fonts_json, c.tech_json, c.awards_json
             FROM posts p JOIN web_captures c ON c.id = p.current_capture_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        site,
        (
            SITE_PALETTE.to_owned(),
            SITE_FONTS.to_owned(),
            SITE_TECH.to_owned(),
            SITE_AWARDS.to_owned()
        )
    );
    let older: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = db
        .query_row(
            "SELECT palette_json, fonts_json, tech_json, awards_json FROM web_captures
             WHERE ai_snapshot_json IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        older,
        (
            Some(r##"[{"hex":"#FFFFFF"}]"##.to_owned()),
            None,
            None,
            None
        )
    );
    assert_eq!(s.repairs.site_json_invalid, 1);
    assert_eq!(plan.web.site_json_invalid, 1);
    assert_eq!(
        one::<i64>(
            &db,
            "SELECT count(*) FROM web_capture_assets WHERE role = 'band' AND css_top = 900"
        ),
        1
    );

    // The manual bookmark: a new key, the desktop id kept in meta.
    let manual_key: String = one(&db, "SELECT key FROM posts WHERE platform = 'manual'");
    assert_eq!(
        one::<String>(
            &db,
            &format!("SELECT value FROM meta WHERE key = 'legacy_id:{manual_key}'")
        ),
        "manual:4f6c"
    );
    assert_eq!(
        one::<String>(
            &db,
            &format!(
                "SELECT o.ext FROM posts p JOIN post_media m ON m.post_id = p.id
                 JOIN media_objects o ON o.id = m.object_id WHERE p.key = '{manual_key}'"
            )
        ),
        "pdf"
    );

    // Collections: the folder linked twice is one; memberships follow the merges.
    assert_eq!(
        rows::<String>(
            &db,
            "SELECT name || ' ' || color FROM collections ORDER BY id"
        ),
        ["Recipes #aabbcc", "Untitled #3d5afe"]
    );
    assert_eq!(s.rows.collections_merged, 1);
    assert_eq!(one::<i64>(&db, "SELECT count(*) FROM post_collections"), 2);
    assert_eq!(s.repairs.collections_fixed, 1);
    assert_eq!(one::<i64>(&db, "SELECT count(*) FROM tag_alias"), 1);
    assert_eq!(
        one::<i64>(&db, "SELECT count(*) FROM tag_cluster_membership"),
        1
    );
    assert_eq!(s.repairs.orphan_rows, 1);

    // The summary travels in the bundle, and it is counts only.
    let stored: String = one(
        &db,
        &format!("SELECT value FROM meta WHERE key = '{SUMMARY_META_KEY}'"),
    );
    assert!(!stored.contains("/assets/") && !stored.contains("someone"));
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap()["posts"]["merged"],
        1
    );
    assert_eq!(
        bundle.db_sha256,
        bundle::sha256_file(&bundle.db_path).unwrap().0
    );

    // OI-11: the orphan file is listed for the owner.
    let legacy = LegacyDb::open(&desktop.db).unwrap();
    let (_, mapping) = plan_with_mapping(
        &legacy,
        &PlanOptions {
            media_root: Some(desktop.root.clone()),
            redact: true,
            now_ms: NOW,
        },
    )
    .unwrap();
    assert_eq!(
        orphan_paths(&desktop.root, &mapping.files.referenced()),
        ["images/orphan.jpg"]
    );
}

#[test]
fn with_videos_the_kept_videos_are_objects_too() {
    let desktop = library();
    let out = tempfile::tempdir().unwrap();
    let (bundle, _) = desktop.bundle(true, out.path());
    assert_eq!(bundle.summary.files.videos_excluded, 0);
    let db = Connection::open(&bundle.db_path).unwrap();
    let video: String = one(
        &db,
        "SELECT o.role || '.' || o.ext FROM post_media m
         JOIN media_objects o ON o.id = m.video_object_id",
    );
    assert_eq!(video, "video.mp4");
    assert!(bundle.objects.iter().any(|o| o.ext == "mp4"));
}

#[test]
fn building_twice_gives_the_same_objects() {
    let desktop = library();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (a, _) = desktop.bundle(false, first.path());
    let (b, _) = desktop.bundle(false, second.path());
    assert_eq!(a.objects.len(), b.objects.len());
    for (x, y) in a.objects.iter().zip(&b.objects) {
        assert_eq!((&x.sha256, x.ext, x.bytes), (&y.sha256, y.ext, y.bytes));
    }
    assert_eq!(a.db_sha256, b.db_sha256, "a deterministic database");
}

/// A library with one case of every row of plan §4.2, each asserted under
/// that row's name below.
fn section_4_2_library() -> Desktop {
    use shelfy_migrate::settings::fixture::{key, latin1, write};

    let d = Desktop::new();
    let c = &d.conn;
    let shortcode = MediaPk::parse_decimal(PK).unwrap().to_shortcode();
    // §4.2 "posts.id (IG)": the composite id, the bare pk and the shortcode
    // of one post. The composite row has the files, the pk row the analysis,
    // the shortcode row a user layer; they have folders and notes.
    let cover = d.asset("thumbnails/instagram-a.jpg", &file(JPEG, 21));
    c.execute(
        "INSERT INTO posts (id, platform, shortcode, media_type, thumbnail_path, imported_at,
           user_note, timestamp)
         VALUES (?1, 'instagram', ?2, 'image', ?3, ?4, 'same note', '')",
        params![format!("{PK}_7"), shortcode, cover, NOW_S],
    )
    .unwrap();
    c.execute(
        "INSERT INTO posts (id, platform, shortcode, media_type, imported_at, ai_status,
           ai_description, ai_tags, ai_model, user_note, timestamp)
         VALUES (?1, 'instagram', ?2, 'image', ?3, 'done', 'A chair', '[\"Chair\"]',
           'qwen2.5vl', 'other note', '2023-05-01T08:00:00Z')",
        params![PK, shortcode, NOW_S - 100],
    )
    .unwrap();
    c.execute(
        "INSERT INTO posts (id, platform, shortcode, media_type, imported_at, user_note,
           user_tags)
         VALUES (?1, 'instagram', ?1, 'image', ?2, 'same note', '[\"Mine\",\"mine\"]')",
        params![shortcode, NOW_S],
    )
    .unwrap();
    // §4.2 "post_tags": a manual tier and an untiered (NULL) row.
    c.execute_batch(&format!(
        "INSERT INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES ('{PK}', 'chair', 'Chair', NULL);
         INSERT INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES ('{shortcode}', 'mine', 'Mine', 'manual');
         INSERT INTO collections (id, name, color, platform, external_id, ig_name, created_at)
           VALUES (1, 'Saved', '#AABBCC', 'instagram', '42', 'All posts', 1700000000);
         INSERT INTO collections (id, name, color, created_at) VALUES (2, 'Mine', '#112233', 1700000000);
         INSERT INTO post_collections (post_id, collection_id, added_at) VALUES ('{PK}_7', 1, 1700000000);
         INSERT INTO post_collections (post_id, collection_id, added_at) VALUES ('{shortcode}', 2, 1700000000);"
    ))
    .unwrap();
    // §4.2 "posts.id X / Pinterest", and undated posts.
    c.execute_batch(
        "INSERT INTO posts (id, platform, media_type, imported_at, timestamp)
           VALUES ('1800000000000000005', 'twitter', 'text', 1700000000, 'not a date');
         INSERT INTO posts (id, platform, media_type, imported_at, post_url, timestamp)
           VALUES ('987654321', 'pinterest', 'image', 1700000000,
                   'https://www.pinterest.com/pin/987654321/', NULL);",
    )
    .unwrap();
    // §4.2 "web:<sha1>": http and https twins of one site.
    for (url, note) in [
        ("http://studio.example.test/work", "from http"),
        ("https://studio.example.test/work", "from https"),
    ] {
        c.execute(
            "INSERT INTO posts (id, platform, media_type, web_url, web_final_url, post_url,
               imported_at, user_note, web_pages_json, web_captured_at)
             VALUES (?1, 'web', 'website', ?2, ?2, ?2, 1700000000, ?3, '[]', 1700000000)",
            params![legacy_post_id(url), url, note],
        )
        .unwrap();
    }
    // §4.2 "ai_*": a manual AI edit of the desktop.
    c.execute(
        "INSERT INTO posts (id, platform, media_type, imported_at, ai_status, ai_description,
           ai_model)
         VALUES ('1800000000000000006', 'twitter', 'text', 1700000000, 'done', 'Mine', 'manuale')",
        [],
    )
    .unwrap();
    // §4.2 "jobs, downloads": dropped.
    c.execute_batch(&format!(
        "INSERT INTO jobs (kind, key, post_id, status) VALUES ('download', 'a', '{PK}', 'done');
         INSERT INTO downloads (post_id, asset_type, status) VALUES ('{PK}', 'image', 'done');"
    ))
    .unwrap();
    // §4.2 "localStorage settings".
    write(
        &d.root,
        &[
            (key("file://", "app:language"), latin1("en")),
            (
                key("file://", "download:assetTypes"),
                latin1(r#"{"thumbnail":true,"image":true,"video":false}"#),
            ),
        ],
    );
    d
}

#[test]
fn every_section_4_2_row_maps_as_the_plan_says() {
    let desktop = section_4_2_library();
    let out = tempfile::tempdir().unwrap();
    let (bundle, plan) = desktop.bundle(false, out.path());
    let db = Connection::open(&bundle.db_path).unwrap();
    let s = &bundle.summary;
    let key = format!("ig_{PK}");

    // posts.id (IG `<pk>_<owner>`, pk or shortcode) → ig_<pk>: one post; the
    // row with archived files is kept, takes the pk row's analysis (it had
    // none) and date, unites the folders and joins the notes, each once.
    assert_eq!(plan.duplicates.instagram.groups, 1);
    assert_eq!(plan.duplicates.instagram.rows_merged, 2);
    let (native, note, tags, ai, cover): (String, String, String, String, Option<i64>) = db
        .query_row(
            "SELECT native_id, user_note, user_tags_json, ai_description, cover_object
             FROM posts WHERE key = ?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(native, PK);
    assert_eq!(note, "same note\n\nother note");
    assert_eq!(tags, r#"["Mine"]"#);
    assert_eq!(ai, "A chair");
    assert!(cover.is_some(), "the kept row's file");
    assert_eq!(s.repairs.ai_from_duplicates, 1);
    assert_eq!(
        one::<i64>(
            &db,
            &format!(
                "SELECT count(*) FROM post_collections pc JOIN posts p ON p.id = pc.post_id
                 WHERE p.key = '{key}'"
            )
        ),
        2
    );
    // The earliest import of the group.
    let (imported, posted): (i64, i64) = db
        .query_row(
            "SELECT imported_at, posted_at FROM posts WHERE key = ?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(imported, (NOW_S - 100) * 1000);
    assert_eq!(posted, 1_682_928_000_000);

    // posts.id X / Pinterest → x_<id> / pin_<id>.
    for key in ["x_1800000000000000005", "pin_987654321"] {
        assert_eq!(
            one::<i64>(
                &db,
                &format!("SELECT count(*) FROM posts WHERE key = '{key}'")
            ),
            1,
            "{key}"
        );
    }

    // web:<sha1> → web_<sha1> of the scheme-less URL: the twins are one post.
    assert_eq!(plan.duplicates.web.groups, 1);
    let web: Vec<String> = rows(&db, "SELECT key FROM posts WHERE platform = 'web'");
    assert_eq!(web.len(), 1);
    assert!(web[0].starts_with("web_") && web[0].len() == 24, "{web:?}");
    assert_eq!(
        one::<String>(&db, "SELECT user_note FROM posts WHERE platform = 'web'"),
        "from http\n\nfrom https"
    );

    // timestamp (ISO, '', NULL, invalid) → posted_at ms or NULL; sort_ts
    // falls back to imported_at.
    let undated: Vec<String> = rows(
        &db,
        "SELECT key FROM posts WHERE posted_at IS NULL AND sort_ts = imported_at
           AND key IN ('x_1800000000000000005', 'pin_987654321') ORDER BY key",
    );
    assert_eq!(undated, ["pin_987654321", "x_1800000000000000005"]);
    assert_eq!(plan.posts.posted_at.invalid, 1);
    assert!(plan.posts.posted_at.null >= 1);
    assert!(plan.posts.posted_at.empty >= 1);

    // thumb_blur → thumbhash: the server computes it from the cover.
    assert_eq!(
        one::<i64>(
            &db,
            "SELECT count(*) FROM posts WHERE thumbhash IS NOT NULL"
        ),
        0
    );

    // ai_* → ai_* (desktop-local, schema 1); a manual edit (`manuale`) →
    // model `manual`, no provider, no schema version.
    let analyzed: (String, String, i64) = db
        .query_row(
            "SELECT ai_model, ai_provider, ai_schema_version FROM posts WHERE key = ?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        analyzed,
        ("qwen2.5vl".to_owned(), "desktop-local".to_owned(), 1)
    );
    let manual: (String, Option<String>, Option<i64>, String) = db
        .query_row(
            "SELECT ai_model, ai_provider, ai_schema_version, ai_status FROM posts
             WHERE key = 'x_1800000000000000006'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(manual, ("manual".to_owned(), None, None, "done".to_owned()));
    assert_eq!(s.repairs.manual_ai_edits, 1);

    // post_tags: tier manual → source manual; NULL → source ai, tier NULL.
    let tag_rows: Vec<String> = rows(
        &db,
        "SELECT tag_norm || ':' || source || ':' || coalesce(tier, '-') FROM post_tags ORDER BY 1",
    );
    assert_eq!(tag_rows, ["chair:ai:-", "mine:manual:-"]);

    // collections (+ platform, external_id, ig_name) → source_name.
    assert_eq!(
        rows::<String>(
            &db,
            "SELECT name || '/' || coalesce(platform, '-') || '/' || coalesce(source_name, '-')
             FROM collections ORDER BY id"
        ),
        ["Saved/instagram/All posts", "Mine/-/-"]
    );

    // jobs, downloads → dropped.
    let dropped = |table: &str| {
        plan.tables
            .iter()
            .find(|t| t.table == table)
            .map(|t| t.outcomes.get("dropped").copied().unwrap_or(0))
            .unwrap()
    };
    assert_eq!((dropped("jobs"), dropped("downloads")), (1, 1));

    // localStorage settings → settings: the language and the asset types.
    assert_eq!(
        rows::<String>(
            &db,
            "SELECT key || '=' || value_json FROM settings ORDER BY key"
        ),
        [
            r#"archiveAssetTypes={"thumbnail":true,"image":true,"video":false}"#,
            r#"language="en""#
        ]
    );
    assert_eq!(s.settings, ["language", "archiveAssetTypes"]);
    assert_eq!(plan.settings.as_ref().unwrap().source, "found");
    assert_eq!(plan.identity.ig_long_shortcodes, 0);

    // Every row is accounted for, and the plan passes.
    assert!(plan.verdict.pass, "{:?}", plan.errors);
}
