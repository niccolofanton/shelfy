//! Writes a synthetic desktop library (the desktop schema, real images under
//! `assets/`, and a localStorage with the settings) for trying `shelfy-migrate`
//! end to end without anyone's data:
//!
//! ```sh
//! cargo run -p shelfy-migrate --example synth_desktop -- <dir> [a|b] [posts]
//! ```
//!
//! Variant `a` (the default) is a library to install into an empty web
//! library; variant `b` shares half of `a`'s posts, with notes, a folder of
//! the same name and newer site versions, and adds its own: a library to
//! merge (`run --merge`). The library is `<dir>/shelfy.sqlite`, the media
//! root `<dir>`.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder as _, RgbImage};
use rusqlite::{Connection, params};
use shelfy_core::ids::ig::MediaPk;
use shelfy_core::ids::web::legacy_post_id;
use shelfy_core::legacy::fixture::DESKTOP_SCHEMA_CURRENT;
use shelfy_migrate::settings::fixture::{key, latin1, write};

/// 2026-10-01T00:00:00Z, in seconds.
const NOW_S: i64 = 1_790_812_800;

fn jpeg(width: u32, height: u32, seed: u32) -> Vec<u8> {
    // Smooth gradients with a little texture: photo-like sizes.
    let image = RgbImage::from_fn(width, height, |x, y| {
        let noise = (x.wrapping_mul(31) ^ y.wrapping_mul(17) ^ seed.wrapping_mul(97)) % 23;
        image::Rgb([
            ((x * 255 / width + seed * 13) % 256) as u8,
            ((y * 255 / height + seed * 29) % 256) as u8,
            (((x + y) / 4 + noise + seed * 7) % 256) as u8,
        ])
    });
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 82)
        .encode_image(&image)
        .expect("encode a JPEG");
    out
}

fn png(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let image = RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (seed % 256) as u8, (y % 256) as u8])
    });
    let mut out = Cursor::new(Vec::new());
    PngEncoder::new(&mut out)
        .write_image(image.as_raw(), width, height, ExtendedColorType::Rgb8)
        .expect("encode a PNG");
    out.into_inner()
}

struct Writer {
    root: PathBuf,
    conn: Connection,
}

impl Writer {
    /// Writes an asset; returns the path the desktop stores.
    fn asset(&self, relative: &str, bytes: &[u8]) -> String {
        fs::write(self.root.join("assets").join(relative), bytes).expect("write an asset");
        format!("/Users/synthetic/Library/Application Support/Shelfy/assets/{relative}")
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("usage: synth_desktop <dir> [a|b] [posts]"))?,
    );
    let variant = args.next().unwrap_or_else(|| "a".to_owned());
    let posts: u32 = args.next().map_or(Ok(40), |n| n.parse())?;
    anyhow::ensure!(
        matches!(variant.as_str(), "a" | "b"),
        "the variant is a or b"
    );
    anyhow::ensure!(
        !dir.join("shelfy.sqlite").exists(),
        "{} already has a library",
        dir.display()
    );
    for sub in ["thumbnails", "images", "videos", "web", "previews"] {
        fs::create_dir_all(dir.join("assets").join(sub))?;
    }
    let conn = Connection::open(dir.join("shelfy.sqlite"))?;
    conn.execute_batch(DESKTOP_SCHEMA_CURRENT)?;
    let w = Writer {
        root: dir.clone(),
        conn,
    };
    if variant == "a" {
        library_a(&w, posts)?;
    } else {
        library_b(&w, posts)?;
    }
    // The desktop app closed cleanly: a single file, no WAL.
    w.conn
        .pragma_update(None, "journal_mode", "DELETE")
        .map(|_| ())?;
    drop(w);
    println!(
        "wrote a synthetic desktop library ({variant}, {posts} Instagram posts) to {}",
        dir.display()
    );
    Ok(())
}

/// Instagram post `i`: its pk and shortcode.
fn ig(i: u32) -> (String, String) {
    let pk = (3_191_575_067_010_950_169_u64 + u64::from(i) * 7_919).to_string();
    let shortcode = MediaPk::parse_decimal(&pk)
        .expect("a pk")
        .to_shortcode();
    (pk, shortcode)
}

fn library_a(w: &Writer, posts: u32) -> anyhow::Result<()> {
    let c = &w.conn;
    c.execute_batch(
        "INSERT INTO collections (id, name, color, platform, external_id, ig_name, created_at)
           VALUES (1, 'Lamps', '#3d5afe', 'instagram', '17841', 'Lamps', 1700000000);
         INSERT INTO collections (id, name, color, created_at)
           VALUES (2, 'Studio picks', '#ff7043', 1700000000);",
    )?;
    for i in 0..posts {
        let (pk, shortcode) = ig(i);
        let id = format!("{pk}_25025320");
        let cover = w.asset(
            &format!("thumbnails/instagram-{i}.jpg"),
            &jpeg(1080, 1350, i),
        );
        let slide1 = w.asset(&format!("images/instagram-{i}-1.jpg"), &jpeg(1080, 1350, i + 500));
        let analyzed = i % 5 == 0;
        c.execute(
            "INSERT INTO posts (id, platform, shortcode, post_url, author_username, text,
               thumbnail_url, media_type, timestamp, thumbnail_path, image_path, imported_at,
               ai_description, ai_tags, ai_status, ai_model, ai_analyzed_at)
             VALUES (?1, 'instagram', ?2, ?3, 'synthetic', ?4,
               'https://scontent.cdninstagram.com/v/t51/x.jpg?oe=6A000000', 'carousel',
               '2025-03-01T10:00:00Z', ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?6)",
            params![
                id,
                shortcode,
                format!("https://www.instagram.com/p/{shortcode}/"),
                format!("Synthetic post {i}: lamps, glass and light"),
                cover,
                NOW_S - i64::from(i) * 60,
                analyzed.then_some("A synthetic lamp"),
                analyzed.then_some("[\"Lamp\",\"Glass\"]"),
                analyzed.then_some("done"),
                analyzed.then_some("qwen2.5vl"),
            ],
        )?;
        for (position, local) in [(0, Some(&cover)), (1, Some(&slide1)), (2, None)] {
            c.execute(
                "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
                 VALUES (?1, ?2, 'image', 'https://scontent.cdninstagram.com/v/t51/s.jpg', ?3)",
                params![id, position, local],
            )?;
        }
        if analyzed {
            c.execute(
                "INSERT INTO post_tags (post_id, tag_norm, tag_form, tier)
                 VALUES (?1, 'lamp', 'Lamp', 'general')",
                [&id],
            )?;
        }
        c.execute(
            "INSERT INTO post_collections (post_id, collection_id, added_at) VALUES (?1, ?2, ?3)",
            params![id, 1 + i64::from(i % 2), 1_700_000_000],
        )?;
    }
    // A manual AI edit, an X post with only its automatic preview, a text
    // tweet, a video post whose kept video is gone, a site.
    let poster = w.asset("thumbnails/instagram-video.jpg", &jpeg(720, 1280, 900));
    c.execute(
        "INSERT INTO posts (id, platform, media_type, thumbnail_path, video_path, imported_at,
           ai_description, ai_status, ai_model)
         VALUES ('9_1', 'instagram', 'video', ?1, '/Users/synthetic/Library/Application Support/Shelfy/assets/videos/gone.mp4',
           ?2, 'Written by hand', 'done', 'manuale')",
        params![poster, NOW_S],
    )?;
    c.execute(
        "INSERT INTO post_media (post_id, position, media_type, source_url)
         VALUES ('9_1', 0, 'video', 'https://scontent.cdninstagram.com/v/t51/v.mp4')",
        [],
    )?;
    let preview = w.asset("previews/twitter-1.jpg", &jpeg(640, 480, 901));
    c.execute(
        "INSERT INTO posts (id, platform, post_url, media_type, thumbnail_url, preview_path,
           imported_at)
         VALUES ('1800000000000000001', 'twitter', 'https://x.com/someone/status/1800000000000000001',
           'image', 'https://pbs.twimg.com/media/a.jpg', ?1, ?2)",
        params![preview, NOW_S],
    )?;
    c.execute(
        "INSERT INTO posts (id, platform, media_type, text, imported_at)
         VALUES ('1800000000000000002', 'twitter', 'text', 'Only words', ?1)",
        [NOW_S],
    )?;
    site(w, "https://studio.example.test/", "Home", NOW_S - 86_400, 902)?;
    write(
        &w.root,
        &[
            (key("file://", "app:language"), latin1("it")),
            (
                key("file://", "download:assetTypes"),
                latin1(r#"{"thumbnail":true,"image":true,"video":false}"#),
            ),
        ],
    );
    Ok(())
}

fn library_b(w: &Writer, posts: u32) -> anyhow::Result<()> {
    let c = &w.conn;
    c.execute_batch(
        "INSERT INTO collections (id, name, color, created_at) VALUES (1, 'studio picks', '#000000', 1700000000);
         INSERT INTO collections (id, name, color, created_at) VALUES (2, 'Chairs', '#26a69a', 1700000000);",
    )?;
    // Half of `a`'s posts, keyed by their bare pk, with notes.
    for i in 0..posts / 2 {
        let (pk, shortcode) = ig(i);
        c.execute(
            "INSERT INTO posts (id, platform, shortcode, media_type, imported_at, user_note)
             VALUES (?1, 'instagram', ?2, 'carousel', ?3, 'seen on the second machine')",
            params![pk, shortcode, NOW_S],
        )?;
        c.execute(
            "INSERT INTO post_collections (post_id, collection_id, added_at) VALUES (?1, 1, ?2)",
            params![pk, 1_700_000_000],
        )?;
    }
    // Its own posts.
    for i in 0..posts / 2 {
        let id = (1_800_000_000_000_000_100_u64 + u64::from(i)).to_string();
        let cover = w.asset(&format!("thumbnails/twitter-{i}.jpg"), &jpeg(1200, 800, 2000 + i));
        c.execute(
            "INSERT INTO posts (id, platform, media_type, thumbnail_path, image_path, imported_at,
               text)
             VALUES (?1, 'twitter', 'image', ?2, ?2, ?3, ?4)",
            params![id, cover, NOW_S, format!("A chair, number {i}")],
        )?;
        c.execute(
            "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
             VALUES (?1, 0, 'image', 'https://pbs.twimg.com/media/b.jpg', ?2)",
            params![id, cover],
        )?;
        c.execute(
            "INSERT INTO post_collections (post_id, collection_id, added_at) VALUES (?1, 2, ?2)",
            params![id, 1_700_000_000],
        )?;
    }
    // The same site, captured again later.
    site(
        w,
        "https://studio.example.test/",
        "Home, redesigned",
        NOW_S,
        903,
    )?;
    Ok(())
}

/// A captured site: its hero is its cover and its page.
fn site(w: &Writer, url: &str, title: &str, captured_s: i64, seed: u32) -> anyhow::Result<()> {
    let hero = w.asset(&format!("web/{seed}-hero.png"), &png(1440, 900, seed));
    let favicon = w.asset(&format!("web/{seed}-fav.png"), &png(32, 32, seed + 1));
    let pages = serde_json::json!([{
        "url": url, "title": title, "screenshotPath": hero, "hero": {"path": hero},
        "contentText": "Selected work"
    }])
    .to_string();
    let meta = serde_json::json!({"title": "Studio", "favicon": favicon}).to_string();
    w.conn.execute(
        "INSERT INTO posts (id, platform, media_type, web_url, web_final_url, post_url,
           thumbnail_path, web_pages_json, web_meta_json, web_captured_at, imported_at)
         VALUES (?1, 'web', 'website', ?2, ?2, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![legacy_post_id(url), url, hero, pages, meta, captured_s],
    )?;
    w.conn.execute(
        "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
         VALUES (?1, 0, 'image', ?2, ?3)",
        params![legacy_post_id(url), url, hero],
    )?;
    Ok(())
}
