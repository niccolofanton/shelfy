//! Tests of the ingest merge rules beyond the golden files: the web-only
//! behaviors, the §4.2 duplicate policy, and the properties "merging twice
//! equals merging once".

use proptest::prelude::*;
use rusqlite::types::Value;
use rusqlite::{Connection, params};

use super::duplicates::{
    Duplicate, Layers, NOTE_SEPARATOR, join_notes, merge_duplicate, survivor, union_tags,
};
use super::merge::{
    AiFields, IncomingPost, UpsertOptions, derive_media, upsert_batch, upsert_post,
};
use crate::repo::media::{NewMediaObject, upsert_object};
use crate::repo::posts::{self, AiLayer, NewMedia, NewPost};
use crate::repo::{Platform, RepoError};
use crate::schema::{self, Kind};
use crate::search::index;

const NOW: i64 = 1_790_899_200_000;
const DAY: i64 = 86_400_000;

fn library() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    schema::migrate(&mut conn, Kind::Library).unwrap();
    conn
}

fn object(conn: &Connection, n: u8) -> i64 {
    let new = NewMediaObject {
        sha256: [n; 32],
        ext: "jpg".to_owned(),
        mime: "image/jpeg".to_owned(),
        bytes: 1_000,
        width: None,
        height: None,
        duration_ms: None,
        role: "image".to_owned(),
        variants: 0,
        origin: "server".to_owned(),
    };
    upsert_object(conn, &new, NOW).unwrap()
}

fn slide(kind: &str, url: &str) -> NewMedia {
    NewMedia {
        kind: kind.to_owned(),
        source_url: Some(url.to_owned()),
        ..NewMedia::default()
    }
}

fn ig(n: u64) -> IncomingPost {
    let mut post = IncomingPost::new(
        format!("ig_{n}"),
        Platform::Instagram,
        n.to_string(),
        "image",
    );
    post.caption = Some(format!("caption {n}"));
    post.cover_url = Some(format!("https://cdn.example/{n}.jpg"));
    post
}

fn upsert(conn: &Connection, post: &IncomingPost, now: i64) -> super::merge::UpsertedPost {
    upsert_post(conn, post, UpsertOptions::default(), now).unwrap()
}

fn column<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str, id: i64) -> T {
    conn.query_row(sql, [id], |r| r.get(0)).unwrap()
}

/// Every table of the library, row by row in a stable order, and the search
/// index: the state two runs must share.
fn dump(conn: &Connection) -> String {
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'posts_fts%'
               AND name NOT LIKE 'posts_infix%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut out = String::new();
    for table in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let n = stmt.column_count();
        let mut rows: Vec<String> = stmt
            .query_map([], |r| {
                let values: Vec<Value> =
                    (0..n).map(|i| r.get(i)).collect::<rusqlite::Result<_>>()?;
                Ok(format!("{values:?}"))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        out.push_str(&format!("{table}\n  {}\n", rows.join("\n  ")));
    }
    let indexed: Vec<i64> = conn
        .prepare("SELECT rowid FROM posts_fts ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for id in indexed {
        out.push_str(&format!(
            "fts {id} {:?}\n",
            index::document(conn, id).unwrap()
        ));
    }
    // The infix index's rows; their text derives from the rows above, and
    // `index::verify` checks it against them.
    let infix: Vec<i64> = conn
        .prepare("SELECT rowid FROM posts_infix ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    out.push_str(&format!("infix {infix:?}\n"));
    assert!(
        index::verify(conn).unwrap().is_empty(),
        "stale search index"
    );
    out
}

// ── merge: pure rules ────────────────────────────────────────────────────────

#[test]
fn slides_come_from_the_listed_media_or_the_cover() {
    let mut post = ig(1);
    post.media = vec![
        slide("image", "https://cdn.example/a.jpg"),
        NewMedia::default(),
        slide("video", ""),
        slide("video", "https://cdn.example/b.jpg"),
    ];
    let kinds: Vec<_> = derive_media(&post).into_iter().map(|m| m.kind).collect();
    assert_eq!(kinds, ["image", "video"]);

    // Listed but all without a URL: no slide, and no cover slide either.
    post.media = vec![NewMedia::default()];
    assert!(derive_media(&post).is_empty());

    post.media.clear();
    post.cover_url_expires_at = Some(NOW);
    let cover = derive_media(&post);
    assert_eq!(cover.len(), 1);
    assert_eq!(cover[0].kind, "image");
    assert_eq!(cover[0].source_url_expires_at, Some(NOW));
    post.media_type = "video".to_owned();
    assert_eq!(derive_media(&post)[0].kind, "video");
    post.media_type = "text".to_owned();
    assert!(derive_media(&post).is_empty());
    post.media_type = "image".to_owned();
    post.cover_url = Some(String::new());
    assert!(derive_media(&post).is_empty());
}

#[test]
fn an_analysis_is_tags_lists_or_a_description() {
    let some = |v: &[&str]| Some(Some(v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()));
    assert!(AiFields::default().is_empty());
    assert!(!AiFields::default().carries_analysis());
    let status_only = AiFields {
        status: Some(Some("done".to_owned())),
        model: Some(Some("m".to_owned())),
        ..AiFields::default()
    };
    assert!(!status_only.is_empty());
    assert!(!status_only.carries_analysis());
    for fields in [
        AiFields {
            tags: some(&[]),
            ..AiFields::default()
        },
        AiFields {
            keywords: some(&["k"]),
            ..AiFields::default()
        },
        AiFields {
            entities: some(&[]),
            ..AiFields::default()
        },
        AiFields {
            general_tags: some(&[]),
            ..AiFields::default()
        },
        AiFields {
            specific_tags: some(&[]),
            ..AiFields::default()
        },
        AiFields {
            description: Some(Some("d".to_owned())),
            ..AiFields::default()
        },
    ] {
        assert!(fields.carries_analysis(), "{fields:?}");
    }
    for fields in [
        AiFields {
            tags: Some(None),
            ..AiFields::default()
        },
        AiFields {
            description: Some(Some(String::new())),
            ..AiFields::default()
        },
        AiFields {
            description: Some(None),
            ..AiFields::default()
        },
    ] {
        assert!(!fields.carries_analysis(), "{fields:?}");
    }
}

// ── merge: the library ──────────────────────────────────────────────────────

#[test]
fn invalid_posts_are_refused() {
    let conn = library();
    let mut post = ig(1);
    post.key = "x_1".to_owned();
    assert!(matches!(
        upsert_post(&conn, &post, UpsertOptions::default(), NOW),
        Err(RepoError::Invalid { field: "key", .. })
    ));
    let mut post = ig(1);
    post.media_type = " ".to_owned();
    assert!(matches!(
        upsert_post(&conn, &post, UpsertOptions::default(), NOW),
        Err(RepoError::Invalid {
            field: "mediaType",
            ..
        })
    ));
    let mut post = ig(1);
    post.media = vec![slide("gif", "https://cdn.example/a.gif")];
    assert!(matches!(
        upsert_post(&conn, &post, UpsertOptions::default(), NOW),
        Err(RepoError::Invalid {
            field: "media.kind",
            ..
        })
    ));
    let mut post = ig(1);
    post.caption = Some("x".repeat(20_001));
    assert!(matches!(
        upsert_post(&conn, &post, UpsertOptions::default(), NOW),
        Err(RepoError::Invalid {
            field: "caption",
            ..
        })
    ));
}

#[test]
fn a_new_post_without_a_date_sorts_by_its_import() {
    let conn = library();
    let id = upsert(&conn, &ig(1), NOW).id;
    let (posted_at, sort_ts): (Option<i64>, i64) = conn
        .query_row(
            "SELECT posted_at, sort_ts FROM posts WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((posted_at, sort_ts), (None, NOW));

    // A later date is taken; a missing one never clears it.
    let mut dated = ig(1);
    dated.posted_at = Some(NOW - 30 * DAY);
    assert!(upsert(&conn, &dated, NOW + DAY).changed);
    assert!(!upsert(&conn, &ig(1), NOW + 2 * DAY).changed);
    let sort_ts: i64 = column(&conn, "SELECT sort_ts FROM posts WHERE id = ?1", id);
    assert_eq!(sort_ts, NOW - 30 * DAY);
}

#[test]
fn a_merge_that_changes_nothing_writes_nothing() {
    let conn = library();
    let id = upsert(&conn, &ig(1), NOW).id;
    let again = upsert(&conn, &ig(1), NOW + DAY);
    assert!(!again.inserted);
    assert!(!again.changed);
    let updated: i64 = column(&conn, "SELECT updated_at FROM posts WHERE id = ?1", id);
    assert_eq!(updated, NOW);

    let mut edited = ig(1);
    edited.caption = Some("a brand new caption".to_owned());
    assert!(upsert(&conn, &edited, NOW + 2 * DAY).changed);
    let updated: i64 = column(&conn, "SELECT updated_at FROM posts WHERE id = ?1", id);
    assert_eq!(updated, NOW + 2 * DAY);
    // The search index follows the caption.
    let doc = index::document(&conn, id).unwrap().unwrap();
    assert_eq!(doc.caption, "a brand new caption");
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM posts_fts WHERE posts_fts MATCH 'brand' AND rowid = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1);
}

#[test]
fn a_kept_video_freezes_the_post_like_an_archived_cover() {
    let conn = library();
    let video = object(&conn, 1);
    let mut post = ig(1);
    post.media_type = "video".to_owned();
    post.media = vec![NewMedia {
        video_object_id: Some(video),
        ..slide("video", "https://cdn.example/poster.jpg")
    }];
    let id = upsert(&conn, &post, NOW).id;
    let mut edited = post.clone();
    edited.caption = Some("edited".to_owned());
    edited.posted_at = Some(NOW - DAY);
    edited.media = vec![slide("image", "https://cdn.example/other.jpg")];
    assert!(!upsert(&conn, &edited, NOW + DAY).changed);
    let (caption, kind, url): (String, String, String) = conn
        .query_row(
            "SELECT p.caption, m.kind, m.source_url FROM posts p
             JOIN post_media m ON m.post_id = p.id WHERE p.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (caption.as_str(), kind.as_str(), url.as_str()),
        ("caption 1", "video", "https://cdn.example/poster.jpg")
    );
}

#[test]
fn slide_urls_refresh_until_their_bytes_are_archived() {
    let conn = library();
    let poster = object(&conn, 1);
    let mut post = ig(1);
    post.media_type = "carousel".to_owned();
    post.media = vec![
        NewMedia {
            object_id: Some(poster),
            video_url: Some("https://cdn.example/0.mp4?oe=1".to_owned()),
            ..slide("video", "https://cdn.example/0.jpg")
        },
        NewMedia {
            width: Some(1080),
            ..slide("image", "https://cdn.example/1.jpg")
        },
    ];
    let id = upsert(&conn, &post, NOW).id;
    conn.execute(
        "UPDATE post_media SET fetch_attempts = 3, fetch_error = 'http_403'
         WHERE post_id = ?1 AND position = 1",
        [id],
    )
    .unwrap();

    let mut fresh = post.clone();
    fresh.media = vec![
        NewMedia {
            video_url: Some("https://cdn.example/0.mp4?oe=2".to_owned()),
            ..slide("video", "https://cdn.example/0-new.jpg")
        },
        slide("image", "https://cdn.example/1-new.jpg"),
    ];
    assert!(upsert(&conn, &fresh, NOW + DAY).changed);
    /// `source_url`, `video_url`, `width`, `fetch_attempts`, `fetch_error`.
    type SlideRow = (String, Option<String>, Option<i64>, i64, Option<String>);
    let rows: Vec<SlideRow> = conn
        .prepare(
            "SELECT source_url, video_url, width, fetch_attempts, fetch_error FROM post_media
             WHERE post_id = ?1 ORDER BY position",
        )
        .unwrap()
        .query_map([id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    // Slide 0: its poster is archived, so its URL stays; the video is not
    // kept, so the fresh video URL is taken.
    assert_eq!(rows[0].0, "https://cdn.example/0.jpg");
    assert_eq!(rows[0].1.as_deref(), Some("https://cdn.example/0.mp4?oe=2"));
    // Slide 1: a new URL, a known width kept, a new chance to fetch.
    assert_eq!(
        rows[1],
        (
            "https://cdn.example/1-new.jpg".to_owned(),
            None,
            Some(1080),
            0,
            None
        )
    );

    // An import without a video URL keeps the stored one.
    let mut bare = fresh.clone();
    bare.media[0].video_url = None;
    assert!(!upsert(&conn, &bare, NOW + 2 * DAY).changed);
}

#[test]
fn a_trashed_post_stays_in_the_trash() {
    let conn = library();
    let id = upsert(&conn, &ig(1), NOW).id;
    posts::trash(&conn, &[id], NOW).unwrap();
    let mut edited = ig(1);
    edited.caption = Some("edited in the trash".to_owned());
    assert!(upsert(&conn, &edited, NOW + DAY).changed);
    let deleted: Option<i64> = column(&conn, "SELECT deleted_at FROM posts WHERE id = ?1", id);
    assert_eq!(deleted, Some(NOW));
    let indexed: i64 = conn
        .query_row(
            "SELECT count(*) FROM posts_fts WHERE rowid = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexed, 0);
}

#[test]
fn ai_fields_keep_web_only_columns_and_manual_tags() {
    let conn = library();
    let mut post = ig(1);
    post.ai = AiFields {
        status: Some(Some("done".to_owned())),
        provider: Some(Some("desktop-local".to_owned())),
        schema_version: Some(Some(1)),
        tags: Some(Some(vec!["Lamp".to_owned()])),
        keywords: Some(Some(Vec::new())),
        ..AiFields::default()
    };
    let id = upsert(&conn, &post, NOW).id;
    let (provider, schema, keywords, analyzed): (String, i64, Option<String>, i64) = conn
        .query_row(
            "SELECT ai_provider, ai_schema_version, ai_keywords_json, ai_analyzed_at
             FROM posts WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (provider.as_str(), schema, keywords, analyzed),
        ("desktop-local", 1, None, NOW)
    );

    // The user's own tag of the same name survives an AI overwrite.
    posts::update_user_content(
        &conn,
        id,
        &posts::UserContentPatch {
            note: None,
            tags: Some(vec!["lamp".to_owned()]),
        },
        NOW,
    )
    .unwrap();
    let mut import = ig(1);
    import.ai.tags = Some(Some(vec!["lamp".to_owned(), "desk".to_owned()]));
    let done = upsert_post(
        &conn,
        &import,
        UpsertOptions { overwrite_ai: true },
        NOW + DAY,
    )
    .unwrap();
    assert!(done.ai_applied && done.changed);
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT tag_norm, source FROM post_tags WHERE post_id = ?1 ORDER BY 1, 2")
        .unwrap()
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let rows: Vec<(&str, &str)> = rows.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(rows, [("desk", "ai"), ("lamp", "ai"), ("lamp", "manual")]);
}

#[test]
fn a_batch_reports_what_it_did() {
    let conn = library();
    let mut analyzed = ig(2);
    analyzed.ai.status = Some(Some("done".to_owned()));
    let summary = upsert_batch(
        &conn,
        &[ig(1), analyzed.clone(), ig(1)],
        UpsertOptions::default(),
        NOW,
    )
    .unwrap();
    assert_eq!(
        (
            summary.inserted,
            summary.merged,
            summary.changed,
            summary.ai_updated
        ),
        (2, 1, 0, 1)
    );
    let mut edited = ig(1);
    edited.caption = Some("edited".to_owned());
    let summary = upsert_batch(
        &conn,
        &[edited, analyzed],
        UpsertOptions::default(),
        NOW + DAY,
    )
    .unwrap();
    assert_eq!(
        (
            summary.inserted,
            summary.merged,
            summary.changed,
            summary.ai_updated
        ),
        (0, 2, 1, 0)
    );
    assert_eq!(summary.posts.len(), 2);
}

// ── merge: properties ───────────────────────────────────────────────────────

/// Canonical keys of the generated posts, with their platform and native id.
const KEYS: [(&str, Platform, &str); 5] = [
    ("ig_11", Platform::Instagram, "11"),
    ("ig_12", Platform::Instagram, "12"),
    ("x_13", Platform::Twitter, "13"),
    ("pin_14", Platform::Pinterest, "14"),
    ("x_15", Platform::Twitter, "15"),
];

/// Absent, null, or one of `values`.
fn field<T: Clone + std::fmt::Debug + 'static>(
    values: impl Strategy<Value = T> + 'static,
) -> impl Strategy<Value = Option<Option<T>>> {
    prop_oneof![
        Just(None),
        Just(Some(None)),
        values.prop_map(|v| Some(Some(v)))
    ]
}

fn text(values: &'static [&'static str]) -> impl Strategy<Value = Option<String>> {
    proptest::option::of(proptest::sample::select(values).prop_map(str::to_owned))
}

fn words(values: &'static [&'static str]) -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(
        proptest::sample::select(values).prop_map(str::to_owned),
        0..4,
    )
}

const TAGS: &[&str] = &[
    "Design",
    "design",
    " lamp ",
    "Mid Century",
    "mcm",
    "",
    "Città",
];

fn arb_ai() -> impl Strategy<Value = AiFields> {
    (
        field(proptest::sample::select(&["done", "error", "pending"][..]).prop_map(str::to_owned)),
        field(proptest::sample::select(&["", "A lamp", "Una lampada"][..]).prop_map(str::to_owned)),
        field(words(TAGS)),
        field(words(TAGS)),
        field(words(TAGS)),
        field(words(&["Artemide", "artemide", "Vitra"])),
        field(words(&["lamp", "desk"])),
        field(Just(NOW - 365 * DAY)),
        field(Just("m1".to_owned())),
    )
        .prop_map(
            |(status, description, tags, general, specific, entities, keywords, at, model)| {
                AiFields {
                    status,
                    description,
                    tags,
                    general_tags: general,
                    specific_tags: specific,
                    entities,
                    keywords,
                    analyzed_at: at,
                    model,
                    ..AiFields::default()
                }
            },
        )
}

/// A slide; object ids index into the four objects of [`seeded_library`].
fn arb_slide() -> impl Strategy<Value = NewMedia> {
    (
        proptest::sample::select(&["image", "video"][..]),
        text(&["https://cdn.example/a.jpg", "https://cdn.example/b.jpg", ""]),
        text(&["https://cdn.example/a.mp4"]),
        proptest::option::of(1..=4_i64),
        proptest::option::weighted(0.2, 1..=4_i64),
        proptest::option::of(Just(1080_i64)),
    )
        .prop_map(
            |(kind, source_url, video_url, object, video, width)| NewMedia {
                kind: kind.to_owned(),
                source_url,
                video_url,
                object_id: object,
                video_object_id: video,
                width,
                ..NewMedia::default()
            },
        )
}

fn arb_post() -> impl Strategy<Value = IncomingPost> {
    (
        0..KEYS.len(),
        text(&["first caption", "second caption", ""]),
        text(&["nora", ""]),
        proptest::sample::select(&["image", "video", "carousel", "text"][..]),
        proptest::option::of(proptest::sample::select(
            &[NOW - 10 * DAY, NOW - 20 * DAY][..],
        )),
        text(&["https://cdn.example/c.jpg", "https://cdn.example/d.jpg"]),
        proptest::option::weighted(0.3, 1..=4_i64),
        proptest::collection::vec(arb_slide(), 0..4),
        arb_ai(),
    )
        .prop_map(
            |(k, caption, author, media_type, posted_at, cover_url, cover, media, ai)| {
                let (key, platform, native_id) = KEYS[k];
                let mut post = IncomingPost::new(key, platform, native_id, media_type);
                post.caption = caption;
                post.author_username = author;
                post.posted_at = posted_at;
                post.cover_url = cover_url;
                post.cover_object = cover;
                post.media = media;
                post.ai = ai;
                post
            },
        )
}

/// A library with four media objects, two accepted aliases and, when given,
/// posts that the batch then merges into.
fn seeded_library(seed: &[IncomingPost]) -> Connection {
    let conn = library();
    for n in 1..=4 {
        object(&conn, n);
    }
    for (alias, canonical) in [("mid century", "mid-century"), ("mcm", "mid-century")] {
        conn.execute(
            "INSERT INTO tag_alias VALUES (?1, ?2, 'Mid-Century', 'accepted', ?3)",
            params![alias, canonical, NOW],
        )
        .unwrap();
    }
    upsert_batch(&conn, seed, UpsertOptions::default(), NOW - DAY).unwrap();
    conn
}

/// The first copy of each key of `batch`, in order.
fn first_copies(batch: Vec<IncomingPost>) -> Vec<IncomingPost> {
    let mut seen = std::collections::HashSet::new();
    batch
        .into_iter()
        .filter(|p| seen.insert(p.key.clone()))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Merging a batch twice leaves the library as merging it once does, even
    /// when the batch repeats a key with other values.
    ///
    /// Except with `overwrite_ai`: there a batch that repeats a key is not
    /// idempotent on the desktop either, and the port keeps the desktop's
    /// merge (see `a_key_twice_in_an_overwriting_batch_merges_as_on_the_desktop`),
    /// so such a batch keeps the first copy of each key here.
    #[test]
    fn merging_a_batch_twice_equals_merging_it_once(
        seed in proptest::collection::vec(arb_post(), 0..4),
        batch in proptest::collection::vec(arb_post(), 1..8),
        overwrite_ai in any::<bool>(),
    ) {
        let batch = if overwrite_ai { first_copies(batch) } else { batch };
        let options = UpsertOptions { overwrite_ai };
        let once = seeded_library(&seed);
        upsert_batch(&once, &batch, options, NOW).unwrap();
        let twice = seeded_library(&seed);
        upsert_batch(&twice, &batch, options, NOW).unwrap();
        upsert_batch(&twice, &batch, options, NOW).unwrap();
        prop_assert_eq!(dump(&once), dump(&twice));
    }

    /// When no key repeats, the second merge finds nothing to change.
    #[test]
    fn a_second_merge_changes_nothing(
        seed in proptest::collection::vec(arb_post(), 0..4),
        batch in proptest::collection::vec(arb_post(), 1..6),
        overwrite_ai in any::<bool>(),
    ) {
        let batch = first_copies(batch);
        let options = UpsertOptions { overwrite_ai };
        let conn = seeded_library(&seed);
        upsert_batch(&conn, &batch, options, NOW).unwrap();
        let before = dump(&conn);
        let again = upsert_batch(&conn, &batch, options, NOW).unwrap();
        prop_assert_eq!(again.changed, 0);
        prop_assert_eq!(dump(&conn), before);
    }
}

/// The input `merging_a_batch_twice_equals_merging_it_once` shrank to before
/// it left such batches out (F6): one key twice in a batch with
/// `overwrite_ai`. The first copy carries an analysis (an empty
/// specific-tier list) and writes a NULL model; the second has only a
/// status and a model. The first merge applies both copies (the post is
/// new, then still unanalyzed); the second applies only the first (the post
/// is analyzed now, and the second copy carries no analysis), so the model
/// ends NULL. The desktop's `bulkUpsert`, run on the same batches through
/// `openDesktopDb` (`scripts/golden/lib.ts`), returns the same counts and
/// ends the same way: the port keeps its merge, which leaves a library
/// alone unless the import carries an analysis.
#[test]
fn a_key_twice_in_an_overwriting_batch_merges_as_on_the_desktop() {
    let mut first = IncomingPost::new("ig_12", Platform::Instagram, "12", "image");
    first.ai.model = Some(None);
    first.ai.specific_tags = Some(Some(Vec::new()));
    let mut second = IncomingPost::new("ig_12", Platform::Instagram, "12", "image");
    second.ai.status = Some(Some("done".to_owned()));
    second.ai.model = Some(Some("m1".to_owned()));
    let batch = [first, second];
    let ai = |conn: &Connection| -> (Option<String>, Option<String>) {
        conn.query_row(
            "SELECT ai_status, ai_model FROM posts WHERE key = 'ig_12'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    let counts = |s: &super::merge::UpsertSummary| (s.inserted, s.merged, s.ai_updated);
    let done = |model: Option<&str>| (Some("done".to_owned()), model.map(str::to_owned));

    let overwrite = UpsertOptions { overwrite_ai: true };
    let conn = seeded_library(&[]);
    let once = upsert_batch(&conn, &batch, overwrite, NOW).unwrap();
    assert_eq!(counts(&once), (1, 1, 2));
    assert_eq!(ai(&conn), done(Some("m1")));
    let twice = upsert_batch(&conn, &batch, overwrite, NOW).unwrap();
    assert_eq!(counts(&twice), (0, 2, 1));
    assert_eq!(ai(&conn), done(None), "as on the desktop");

    // Without `overwrite_ai`, a merge never touches an analyzed post's AI
    // layer, and the same batch is idempotent.
    let conn = seeded_library(&[]);
    upsert_batch(&conn, &batch, UpsertOptions::default(), NOW).unwrap();
    let after_once = dump(&conn);
    let twice = upsert_batch(&conn, &batch, UpsertOptions::default(), NOW).unwrap();
    assert_eq!(counts(&twice), (0, 2, 0));
    assert_eq!(dump(&conn), after_once);
    assert_eq!(ai(&conn), done(Some("m1")));
}

// ── duplicates ──────────────────────────────────────────────────────────────

#[test]
fn files_then_ai_then_user_layer_decide() {
    let files = Layers {
        archived_files: 1,
        ..Layers::default()
    };
    let more_files = Layers {
        archived_files: 3,
        ..Layers::default()
    };
    let ai = Layers {
        ai: true,
        ..Layers::default()
    };
    let user = Layers {
        user: true,
        ..Layers::default()
    };
    let ai_and_user = Layers {
        ai: true,
        user: true,
        ..Layers::default()
    };
    assert_eq!(survivor(&[]), None);
    assert_eq!(survivor(&[ai, files]), Some(1));
    assert_eq!(survivor(&[user, ai]), Some(1));
    assert_eq!(survivor(&[Layers::default(), user]), Some(1));
    assert_eq!(survivor(&[ai, ai_and_user]), Some(1));
    assert_eq!(survivor(&[files, more_files]), Some(1));
    // More files only break ties: an analysis beats them.
    let files_and_ai = Layers {
        archived_files: 1,
        ai: true,
        user: false,
    };
    assert_eq!(survivor(&[more_files, files_and_ai]), Some(1));
    // A tie goes to the first row.
    assert_eq!(survivor(&[ai, ai]), Some(0));
}

#[test]
fn notes_are_joined_once_each() {
    assert_eq!(join_notes([]), None);
    assert_eq!(join_notes([" ", ""]), None);
    assert_eq!(join_notes(["a"]).as_deref(), Some("a"));
    let joined = join_notes(["a", " ", "b", "a"]).unwrap();
    assert_eq!(joined, format!("a{NOTE_SEPARATOR}b"));
    for again in ["a", "b"] {
        assert_eq!(join_notes([joined.as_str(), again]).unwrap(), joined);
    }
    // A note that only contains another is still added.
    assert_eq!(
        join_notes(["ab", "a"]).unwrap(),
        format!("ab{NOTE_SEPARATOR}a")
    );
    let three = join_notes(["a", "b", "c"]).unwrap();
    assert_eq!(join_notes([three.as_str(), "b"]).unwrap(), three);
}

#[test]
fn tags_are_united_without_case() {
    let first = vec!["Lamp".to_owned(), " desk ".to_owned()];
    let second = vec![
        "lamp".to_owned(),
        String::new(),
        "Città".to_owned(),
        "CITTÀ".to_owned(),
    ];
    assert_eq!(
        union_tags([first.as_slice(), second.as_slice()]),
        ["Lamp", "desk", "Città"]
    );
}

fn collection(conn: &Connection, name: &str) -> i64 {
    conn.execute(
        "INSERT INTO collections (name, created_at) VALUES (?1, ?2)",
        params![name, NOW],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn stored(conn: &Connection, post: &NewPost, collections: &[i64]) -> i64 {
    let id = posts::insert(conn, post, NOW).unwrap();
    for &c in collections {
        conn.execute(
            "INSERT OR IGNORE INTO post_collections (post_id, collection_id, added_at)
             VALUES (?1, ?2, ?3)",
            params![id, c, NOW],
        )
        .unwrap();
    }
    id
}

fn analysis(tag: &str) -> AiLayer {
    AiLayer {
        status: Some("done".to_owned()),
        description: Some(format!("about {tag}")),
        tags: vec![tag.to_owned()],
        ..AiLayer::default()
    }
}

#[test]
fn the_row_with_files_survives_and_takes_the_other_analysis() {
    let conn = library();
    let cover = object(&conn, 1);
    let spare = object(&conn, 2);
    let (a, b) = (collection(&conn, "A"), collection(&conn, "B"));
    let mut web = NewPost::new("ig_7", Platform::Instagram, "7", "image", NOW - DAY);
    web.cover_object = Some(cover);
    web.caption = Some("web caption".to_owned());
    web.user_note = Some("web note".to_owned());
    web.user_tags = vec!["Lamp".to_owned()];
    let id = stored(&conn, &web, &[a]);

    let mut desktop = NewPost::new("ig_7", Platform::Instagram, "7", "image", NOW - 9 * DAY);
    desktop.caption = Some("desktop caption".to_owned());
    desktop.posted_at = Some(NOW - 100 * DAY);
    desktop.ai = Some(analysis("lamp"));
    desktop.user_note = Some("desktop note".to_owned());
    desktop.user_tags = vec!["lamp".to_owned(), "desk".to_owned()];
    desktop.media = vec![NewMedia {
        object_id: Some(spare),
        ..slide("image", "https://cdn.example/7.jpg")
    }];
    // Equal files (one each), so the stored row's AI flag would decide; it
    // has none, and the other has one: the other survives.
    let other = Duplicate {
        post: desktop.clone(),
        collections: vec![a, b],
        captures: 0,
    };
    let done = merge_duplicate(&conn, id, &other, NOW).unwrap();
    assert!(done.replaced);
    assert!(done.notes_joined);
    assert_eq!((done.tags_added, done.collections_added), (1, 1));
    let row: (String, Option<i64>, i64, String, String, Option<i64>) = conn
        .query_row(
            "SELECT caption, cover_object, imported_at, user_note, user_tags_json, posted_at
             FROM posts WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, "desktop caption");
    assert_eq!(row.1, None);
    assert_eq!(row.2, NOW - 9 * DAY);
    assert_eq!(row.3, format!("desktop note{NOTE_SEPARATOR}web note"));
    assert_eq!(row.4, r#"["lamp","desk"]"#);
    assert_eq!(row.5, Some(NOW - 100 * DAY));
    // The stored cover lost its last reference.
    let stamped: Option<i64> = column(
        &conn,
        "SELECT unreferenced_since FROM media_objects WHERE id = ?1",
        cover,
    );
    assert_eq!(stamped, Some(NOW));
    let detail = posts::get(&conn, "ig_7").unwrap().unwrap();
    assert_eq!(detail.summary.ai_status.as_deref(), Some("done"));

    // Merging the same row again changes nothing.
    let snapshot = dump(&conn);
    let again = merge_duplicate(&conn, id, &other, NOW + DAY).unwrap();
    assert!(!again.changed(), "{again:?}");
    assert_eq!(dump(&conn), snapshot);
}

#[test]
fn the_stored_row_wins_ties_and_fills_its_gaps() {
    let conn = library();
    let cover = object(&conn, 1);
    let other_cover = object(&conn, 2);
    let mut web = NewPost::new("x_8", Platform::Twitter, "8", "image", NOW);
    web.cover_object = Some(cover);
    web.caption = Some("web caption".to_owned());
    let id = stored(&conn, &web, &[]);

    let mut desktop = NewPost::new("x_8", Platform::Twitter, "8", "image", NOW - DAY);
    desktop.cover_object = Some(other_cover);
    desktop.caption = Some("desktop caption".to_owned());
    desktop.posted_at = Some(NOW - 50 * DAY);
    // Not an analysis: an error status alone.
    desktop.ai = Some(AiLayer {
        status: Some("error".to_owned()),
        ..AiLayer::default()
    });
    let other = Duplicate {
        post: desktop,
        collections: Vec::new(),
        captures: 0,
    };
    let done = merge_duplicate(&conn, id, &other, NOW).unwrap();
    assert!(!done.replaced && !done.ai_filled && done.date_filled && done.changed());
    let (caption, cover_object, posted_at, ai_status): (String, i64, i64, Option<String>) = conn
        .query_row(
            "SELECT caption, cover_object, posted_at, ai_status FROM posts WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (caption.as_str(), cover_object, posted_at, ai_status),
        ("web caption", cover, NOW - 50 * DAY, None)
    );
    // The other row's cover was never referenced: it is the GC's.
    let stamped: Option<i64> = column(
        &conn,
        "SELECT unreferenced_since FROM media_objects WHERE id = ?1",
        other_cover,
    );
    assert_eq!(stamped, Some(NOW));

    // A row without files but with an analysis loses, and its analysis fills
    // the stored row; a known date stays.
    let mut later = other.clone();
    later.post.cover_object = None;
    later.post.ai = Some(analysis("poster"));
    later.post.posted_at = Some(NOW - 5 * DAY);
    let done = merge_duplicate(&conn, id, &later, NOW + DAY).unwrap();
    assert!(!done.replaced && done.ai_filled && !done.date_filled);
    let posted_at: i64 = column(&conn, "SELECT posted_at FROM posts WHERE id = ?1", id);
    assert_eq!(posted_at, NOW - 50 * DAY);
}

#[test]
fn a_duplicate_must_be_the_same_post() {
    let conn = library();
    let id = stored(
        &conn,
        &NewPost::new("ig_9", Platform::Instagram, "9", "image", NOW),
        &[],
    );
    let other = Duplicate {
        post: NewPost::new("ig_10", Platform::Instagram, "10", "image", NOW),
        collections: Vec::new(),
        captures: 0,
    };
    assert!(matches!(
        merge_duplicate(&conn, id, &other, NOW),
        Err(RepoError::Invalid { field: "key", .. })
    ));
    assert!(matches!(
        merge_duplicate(&conn, id + 1, &other, NOW),
        Err(RepoError::NotFound)
    ));
}

fn arb_row() -> impl Strategy<Value = (NewPost, Vec<i64>)> {
    (
        proptest::option::weighted(0.4, 1..=3_i64),
        proptest::collection::vec(arb_slide(), 0..3),
        proptest::option::of(proptest::sample::select(&["done", "error"][..])),
        text(&["note one", "note two", " "]),
        words(TAGS),
        proptest::option::of(Just(NOW - 40 * DAY)),
        proptest::collection::vec(1..=3_i64, 0..3),
        text(&["caption a", "caption b"]),
    )
        .prop_map(
            |(cover, media, status, note, tags, posted_at, folders, caption)| {
                let mut post = NewPost::new("ig_21", Platform::Instagram, "21", "image", NOW - DAY);
                post.cover_object = cover;
                post.media = media
                    .into_iter()
                    .filter(|m| m.source_url.is_some())
                    .collect();
                post.ai = status.map(|s| AiLayer {
                    status: Some(s.to_owned()),
                    tags: vec!["lamp".to_owned()],
                    ..AiLayer::default()
                });
                post.user_note = note;
                post.user_tags = tags;
                post.posted_at = posted_at;
                post.caption = caption;
                (post, folders)
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Merging the same duplicate twice leaves the post as merging it once.
    #[test]
    fn merging_a_duplicate_twice_equals_merging_it_once(
        stored_row in arb_row(),
        other_row in arb_row(),
        captures in 0..2_u64,
    ) {
        let conn = library();
        for n in 1..=4 {
            object(&conn, n);
        }
        for name in ["A", "B", "C"] {
            collection(&conn, name);
        }
        let id = stored(&conn, &stored_row.0, &stored_row.1);
        let other = Duplicate { post: other_row.0, collections: other_row.1, captures };
        merge_duplicate(&conn, id, &other, NOW).unwrap();
        // The caller copies the other row's captures.
        for _ in 0..captures {
            conn.execute(
                "INSERT INTO web_captures (post_id, captured_at, status, created_at)
                 VALUES (?1, ?2, 'done', ?2)",
                params![id, NOW],
            )
            .unwrap();
        }
        let once = dump(&conn);
        let again = merge_duplicate(&conn, id, &other, NOW).unwrap();
        prop_assert!(!again.changed(), "{:?}", again);
        prop_assert_eq!(dump(&conn), once);
    }
}
