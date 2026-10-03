//! Tests of the sanitizer's own rules (the desktop's are in the golden set
//! `sanitize`) and its bounds: arbitrary JSON never panics, the output stays
//! within the limits, and the merge accepts every post it returns.

use proptest::prelude::*;
use rusqlite::Connection;
use serde_json::{Value, json};

use super::*;
use crate::ingest::hosts::MAX_URL_LEN;
use crate::ingest::merge::{UpsertOptions, upsert_batch};
use crate::repo::posts::CAPTION_MAX_CHARS;
use crate::schema::{self, Kind};

/// 2026-10-02T00:00:00Z.
const NOW: i64 = 1_790_899_200_000;
const PK: &str = "3191575067010950169";
const SHORTCODE: &str = "CxKwJ0fLmQZ";
const IG_IMAGE: &str =
    "https://scontent-mxp1-1.cdninstagram.com/v/t51/1_n.jpg?stp=dst&oe=6A1B2C3D&_nc_sid=1";
const IG_POSTER: &str = "https://scontent-mxp1-1.cdninstagram.com/v/t51/2_n.jpg?oe=6A1B2C3E";
const IG_VIDEO: &str =
    "https://instagram.fmxp1-1.fna.fbcdn.net/o1/v/t16/f2/m86/AQx.mp4?efg=1&oe=6A1B2C3F";

fn library() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    schema::migrate(&mut conn, Kind::Library).unwrap();
    conn
}

fn sanitize(platform: Platform, items: &[Value]) -> SanitizedBatch {
    sanitize_batch(platform, items, NOW).unwrap()
}

/// The one post of a one-item batch.
fn one(platform: Platform, item: Value) -> IncomingPost {
    let batch = sanitize(platform, &[item]);
    assert!(batch.rejected.is_empty(), "{:?}", batch.rejected);
    batch.posts.into_iter().next().unwrap()
}

/// The rejection code of a one-item batch.
fn rejected(platform: Platform, item: Value) -> RejectCode {
    let batch = sanitize(platform, &[item]);
    assert!(batch.posts.is_empty(), "{:?}", batch.posts);
    batch.rejected[0].code
}

#[test]
fn an_instagram_item_becomes_a_post() {
    let post = one(
        Platform::Instagram,
        json!({
            "id": format!("{PK}_25025320"),
            "platform": "web",
            "shortcode": SHORTCODE,
            "postUrl": format!("https://www.instagram.com/p/{SHORTCODE}/"),
            "profileUrl": "https://www.instagram.com/someone/",
            "authorUsername": "someone",
            "authorName": "",
            "text": "A lamp\nin glass",
            "timestamp": "2023-09-14T09:56:58.000Z",
            "thumbnailUrl": IG_IMAGE,
            "mediaType": "carousel",
            "media": [
                {"type": "image", "url": IG_IMAGE},
                {"type": "video", "url": IG_POSTER, "videoUrl": IG_VIDEO},
            ],
        }),
    );
    assert_eq!(post.key, format!("ig_{PK}"));
    assert_eq!(post.native_id, PK);
    assert_eq!(
        post.platform,
        Platform::Instagram,
        "the batch platform wins"
    );
    assert_eq!(post.shortcode.as_deref(), Some(SHORTCODE));
    assert_eq!(
        post.post_url,
        Some(format!("https://www.instagram.com/p/{SHORTCODE}/"))
    );
    assert_eq!(
        post.profile_url.as_deref(),
        Some("https://www.instagram.com/someone/")
    );
    assert_eq!(post.author_username.as_deref(), Some("someone"));
    assert_eq!(post.author_name, None, "blank is none");
    assert_eq!(post.caption.as_deref(), Some("A lamp\nin glass"));
    assert_eq!(post.posted_at, Some(1_694_685_418_000));
    assert_eq!(post.media_type, "carousel");
    assert_eq!(post.cover_url.as_deref(), Some(IG_IMAGE));
    assert_eq!(post.cover_url_expires_at, Some(0x6A1B_2C3D * 1_000));
    assert_eq!(post.media.len(), 2);
    let image = &post.media[0];
    assert_eq!(image.kind, "image");
    assert_eq!(image.source_url.as_deref(), Some(IG_IMAGE));
    assert_eq!(image.source_url_expires_at, Some(0x6A1B_2C3D * 1_000));
    assert_eq!(image.video_url, None);
    let video = &post.media[1];
    assert_eq!(video.kind, "video");
    assert_eq!(video.source_url.as_deref(), Some(IG_POSTER));
    assert_eq!(video.source_url_expires_at, Some(0x6A1B_2C3E * 1_000));
    assert_eq!(video.video_url.as_deref(), Some(IG_VIDEO));
    assert_eq!(video.video_url_expires_at, Some(0x6A1B_2C3F * 1_000));
    assert_eq!(post.archive_state, None, "the caller derives the state");
    assert!(post.ai.is_empty());
}

#[test]
fn instagram_keys_come_from_the_id_then_the_shortcode() {
    let key = |item: Value| one(Platform::Instagram, item).key;
    let expected = format!("ig_{PK}");
    assert_eq!(key(json!({"id": format!("{PK}_1")})), expected);
    assert_eq!(key(json!({"id": PK})), expected);
    assert_eq!(key(json!({"id": SHORTCODE})), expected);
    assert_eq!(
        key(json!({"id": SHORTCODE, "shortcode": SHORTCODE})),
        expected
    );
    // An id that is none of the three: the item's shortcode.
    assert_eq!(
        key(json!({"id": "not an id", "shortcode": SHORTCODE})),
        expected
    );
    // A number is the pk.
    assert_eq!(key(json!({"id": 64})), "ig_64");
    assert_eq!(
        key(json!({"id": 3_191_575_067_010_950_169_u64})),
        expected,
        "every digit is kept"
    );

    for bad in [
        json!({"id": "not an id"}),
        json!({"id": "not an id", "shortcode": "bad shortcode"}),
        json!({"id": "0_1"}),
        json!({"id": "AAAA"}),
        json!({"id": ""}),
        json!({"shortcode": SHORTCODE}),
        // A pk so long that its key would not fit the merge's 200 bytes.
        json!({"id": "9".repeat(250)}),
    ] {
        assert_eq!(
            rejected(Platform::Instagram, bad.clone()),
            RejectCode::BadId,
            "{bad}"
        );
    }
}

#[test]
fn x_and_pinterest_keys_fall_back_to_an_allowed_post_url() {
    let tweet = one(
        Platform::Twitter,
        json!({"id": "1700000000000000001", "mediaType": "text"}),
    );
    assert_eq!(tweet.key, "x_1700000000000000001");
    let tweet = one(
        Platform::Twitter,
        json!({"id": "odd", "postUrl": "https://x.com/someone/status/42"}),
    );
    assert_eq!(tweet.key, "x_42");
    // A post URL off the allowlist is no evidence.
    assert_eq!(
        rejected(
            Platform::Twitter,
            json!({"id": "odd", "postUrl": "https://evil.example/someone/status/42"})
        ),
        RejectCode::BadId
    );

    let pin = one(Platform::Pinterest, json!({"id": "987654321012345678"}));
    assert_eq!(pin.key, "pin_987654321012345678");
    let pin = one(
        Platform::Pinterest,
        json!({"id": "a b", "postUrl": "https://it.pinterest.com/pin/123456/"}),
    );
    assert_eq!(pin.key, "pin_123456");
    assert_eq!(
        rejected(
            Platform::Pinterest,
            json!({"id": "a b", "postUrl": "https://pinterest.evil.io/pin/123456/"})
        ),
        RejectCode::BadId
    );
}

#[test]
fn ids_must_be_strings_or_integers() {
    for bad in [
        json!(null),
        json!(true),
        json!(1.5),
        json!(-0.0),
        json!([123]),
        json!({"id": 123}),
        json!(""),
        json!("1".repeat(MAX_ID_LEN + 1)),
    ] {
        assert_eq!(
            rejected(Platform::Twitter, json!({ "id": bad })),
            RejectCode::BadId,
            "{bad}"
        );
    }
    // 256 UTF-16 code units is the limit: 128 astral characters fit.
    let item = json!({ "id": "😀".repeat(128) });
    assert_eq!(
        clean_item(Platform::Pinterest, &item).unwrap().id.len(),
        512
    );
    let item = json!({ "id": "😀".repeat(129) });
    assert_eq!(
        clean_item(Platform::Pinterest, &item),
        Err(RejectCode::BadId)
    );
}

#[test]
fn rejections_carry_their_index_and_code() {
    let batch = sanitize(
        Platform::Twitter,
        &[
            json!(null),
            json!({"id": "1"}),
            json!("text"),
            json!([{"id": "2"}]),
            json!({"id": "x"}),
            json!(7),
            json!({"id": 3}),
        ],
    );
    assert_eq!(batch.indices, [1, 6]);
    let keys: Vec<&str> = batch.posts.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["x_1", "x_3"]);
    assert_eq!(
        batch.rejected,
        [
            Rejected {
                index: 0,
                code: RejectCode::BadItem
            },
            Rejected {
                index: 2,
                code: RejectCode::BadItem
            },
            Rejected {
                index: 3,
                code: RejectCode::BadItem
            },
            Rejected {
                index: 4,
                code: RejectCode::BadId
            },
            Rejected {
                index: 5,
                code: RejectCode::BadItem
            },
        ]
    );
    assert_eq!(RejectCode::BadId.as_str(), "bad_id");
    assert_eq!(
        serde_json::to_string(&batch.rejected[0]).unwrap(),
        r#"{"index":0,"code":"bad_item"}"#
    );
}

#[test]
fn a_batch_over_the_limit_is_refused_not_cut() {
    let item = json!({"id": "1"});
    let full = vec![item.clone(); MAX_BATCH_ITEMS];
    assert_eq!(
        sanitize(Platform::Twitter, &full).posts.len(),
        MAX_BATCH_ITEMS
    );
    let over = vec![item; MAX_BATCH_ITEMS + 1];
    assert_eq!(
        sanitize_batch(Platform::Twitter, &over, NOW),
        Err(SanitizeError::TooManyItems {
            max: MAX_BATCH_ITEMS,
            len: MAX_BATCH_ITEMS + 1
        })
    );
    assert_eq!(
        sanitize_batch(Platform::Web, &[], NOW),
        Err(SanitizeError::Platform(Platform::Web))
    );
    assert_eq!(
        sanitize_batch(Platform::Manual, &[], NOW),
        Err(SanitizeError::Platform(Platform::Manual))
    );
    assert_eq!(
        sanitize(Platform::Pinterest, &[]),
        SanitizedBatch::default()
    );
}

#[test]
fn urls_off_the_allowlist_are_dropped() {
    let post = one(
        Platform::Twitter,
        json!({
            "id": "1",
            "postUrl": "javascript:alert(1)",
            "profileUrl": "https://evil.example/someone",
            "thumbnailUrl": "https://evil.example/a.jpg",
            "media": [
                {"type": "image", "url": "https://evil.example/a.jpg"},
                {"type": "image", "url": "https://pbs.twimg.com.evil.io/a.jpg"},
                {"type": "image", "url": "https://user:pw@pbs.twimg.com/media/a.jpg"},
                {"type": "image", "url": "https://pbs.twimg.com:8080/media/a.jpg"},
                {"type": "image", "url": " https://pbs.twimg.com/media/a.jpg"},
                {"type": "image", "url": "https://scontent.cdninstagram.com/a.jpg"},
                {"type": "image", "url": "https://pbs.twimg.com/media/ok.jpg"},
            ],
        }),
    );
    assert_eq!(post.post_url, None);
    assert_eq!(post.profile_url, None);
    assert_eq!(post.cover_url, None);
    let urls: Vec<_> = post.media.iter().map(|m| m.source_url.as_deref()).collect();
    assert_eq!(urls, [Some("https://pbs.twimg.com/media/ok.jpg")]);
    assert_eq!(post.media_type, "image", "derived from the one slide");
}

#[test]
fn x_status_urls_without_an_author_are_repaired() {
    let post = one(
        Platform::Twitter,
        json!({"id": "1800000000000000001", "postUrl": "https://x.com//status/1800000000000000001"}),
    );
    assert_eq!(
        post.post_url.as_deref(),
        Some("https://x.com/i/status/1800000000000000001")
    );
    let post = one(
        Platform::Twitter,
        json!({"id": "1", "postUrl": "https://x.com/someone/status/1"}),
    );
    assert_eq!(
        post.post_url.as_deref(),
        Some("https://x.com/someone/status/1")
    );
}

#[test]
fn video_entries_keep_their_poster_and_their_direct_video() {
    // X: the poster is the url, the MP4 the videoUrl.
    let poster = "https://pbs.twimg.com/ext_tw_video_thumb/1/pu/img/a.jpg";
    let mp4 = "https://video.twimg.com/ext_tw_video/1/pu/vid/720x1280/a.mp4?tag=12";
    let post = one(
        Platform::Twitter,
        json!({"id": "1", "mediaType": "video", "thumbnailUrl": poster,
               "media": [{"type": "video", "url": poster, "videoUrl": mp4}]}),
    );
    assert_eq!(post.media[0].source_url.as_deref(), Some(poster));
    assert_eq!(post.media[0].video_url.as_deref(), Some(mp4));
    assert_eq!(post.media[0].video_url_expires_at, None);

    // Pinterest keeps the MP4 itself as the url: the cover is the poster.
    let cover = "https://i.pinimg.com/originals/aa/bb/cc/x.jpg";
    let pin_mp4 = "https://v1.pinimg.com/videos/mc/720p/aa/bb/cc/x.mp4";
    let hls = "https://v1.pinimg.com/videos/mc/hls/aa/bb/cc/x.m3u8";
    let post = one(
        Platform::Pinterest,
        json!({"id": "1", "mediaType": "video", "thumbnailUrl": cover,
               "media": [{"type": "video", "url": pin_mp4}]}),
    );
    assert_eq!(post.media[0].source_url.as_deref(), Some(cover));
    assert_eq!(post.media[0].video_url.as_deref(), Some(pin_mp4));
    // P2-05's parser repeats it as videoUrl.
    let post = one(
        Platform::Pinterest,
        json!({"id": "1", "thumbnailUrl": cover,
               "media": [{"type": "video", "url": pin_mp4, "videoUrl": pin_mp4}]}),
    );
    assert_eq!(post.media[0].video_url.as_deref(), Some(pin_mp4));
    // An HLS playlist is a video, not a poster, and not a direct video.
    let post = one(
        Platform::Pinterest,
        json!({"id": "1", "thumbnailUrl": cover,
               "media": [{"type": "video", "url": hls}, {"type": "video", "url": cover, "videoUrl": hls}]}),
    );
    assert_eq!(post.media[0].source_url.as_deref(), Some(cover));
    assert_eq!(post.media[0].video_url, None);
    assert_eq!(post.media[1].source_url.as_deref(), Some(cover));
    assert_eq!(
        post.media[1].video_url, None,
        "a manifest is no direct video"
    );
    assert_eq!(post.media_type, "carousel");
    // A video pin whose cover is the MP4 too has no cover and no poster.
    let post = one(
        Platform::Pinterest,
        json!({"id": "1", "mediaType": "video", "thumbnailUrl": pin_mp4,
               "media": [{"type": "video", "url": pin_mp4}]}),
    );
    assert_eq!(post.cover_url, None);
    assert_eq!(post.media[0].source_url, None);
    assert_eq!(post.media[0].video_url.as_deref(), Some(pin_mp4));
    // An image entry is never a video; a videoUrl off the allowlist is dropped.
    let post = one(
        Platform::Twitter,
        json!({"id": "1", "media": [
            {"type": "image", "url": poster, "videoUrl": mp4},
            {"type": "video", "url": poster, "videoUrl": "https://evil.example/a.mp4"},
        ]}),
    );
    assert_eq!(post.media[0].kind, "image");
    assert_eq!(post.media[0].video_url, None);
    assert_eq!(post.media[1].video_url, None);
}

#[test]
fn media_types_are_known_or_derived() {
    let image = "https://pbs.twimg.com/media/a.jpg";
    let media_type = |declared: Value, media: Value| {
        one(
            Platform::Twitter,
            json!({"id": "1", "mediaType": declared, "media": media}),
        )
        .media_type
    };
    for known in ["image", "images", "carousel", "video", "text"] {
        assert_eq!(media_type(json!(known), json!([])), known);
    }
    assert_eq!(media_type(json!("website"), json!([])), "text");
    assert_eq!(media_type(json!(""), json!([{"url": image}])), "image");
    assert_eq!(
        media_type(json!("Image"), json!([{"type": "video", "url": image}])),
        "video"
    );
    assert_eq!(
        media_type(json!(7), json!([{"url": image}, {"url": image}])),
        "carousel"
    );
}

#[test]
fn dates_must_be_iso_and_plausible() {
    let date = |timestamp: Value| {
        one(
            Platform::Twitter,
            json!({"id": "1", "timestamp": timestamp}),
        )
        .posted_at
    };
    assert_eq!(
        date(json!("2024-01-01T10:00:00.000Z")),
        Some(1_704_103_200_000)
    );
    assert_eq!(
        date(json!(" 2024-01-01T10:00:00Z ")),
        Some(1_704_103_200_000)
    );
    assert_eq!(date(json!("2000-01-01T00:00:00Z")), Some(MIN_POSTED_AT_MS));
    assert_eq!(date(json!("1999-12-31T23:59:59Z")), None);
    assert_eq!(
        date(json!("2026-10-02T23:00:00Z")),
        Some(NOW + 23 * 3_600_000)
    );
    assert_eq!(
        date(json!("2026-10-03T00:00:01Z")),
        None,
        "over a day ahead"
    );
    assert_eq!(date(json!("9999-12-31T23:59:59Z")), None);
    assert_eq!(date(json!("Fri, 01 Aug 2025 19:57:38 +0000")), None);
    assert_eq!(date(json!("2024-01-01T10:00:00")), None, "no zone");
    assert_eq!(date(json!("")), None);
    assert_eq!(date(json!(1_704_103_200)), None);
}

#[test]
fn shortcodes_are_kept_on_instagram_only_and_when_valid() {
    let shortcode =
        |platform, value: &str| one(platform, json!({"id": "123", "shortcode": value})).shortcode;
    assert_eq!(
        shortcode(Platform::Instagram, SHORTCODE).as_deref(),
        Some(SHORTCODE)
    );
    assert_eq!(shortcode(Platform::Instagram, "not valid"), None);
    assert_eq!(shortcode(Platform::Instagram, &"B".repeat(65)), None);
    assert_eq!(shortcode(Platform::Twitter, SHORTCODE), None);
    assert_eq!(shortcode(Platform::Pinterest, SHORTCODE), None);
}

#[test]
fn text_fields_are_clamped_and_kept() {
    let post = one(
        Platform::Twitter,
        json!({
            "id": "1",
            "authorUsername": "  ",
            "authorName": "n".repeat(MAX_STRING_LEN + 10),
            "text": format!("{}😀b", "a".repeat(MAX_TEXT_LEN - 1)),
        }),
    );
    assert_eq!(post.author_username, None);
    assert_eq!(post.author_name.unwrap().len(), MAX_STRING_LEN);
    // The emoji would end past the limit: the cut falls before it.
    assert_eq!(post.caption.unwrap(), "a".repeat(MAX_TEXT_LEN - 1));
    // No text is an empty caption, as the desktop and the migration store it.
    assert_eq!(
        one(Platform::Twitter, json!({"id": "1"}))
            .caption
            .as_deref(),
        Some("")
    );
    assert_eq!(clamp("abc", 2), "ab");
    assert_eq!(clamp("a😀", 2), "a");
    assert_eq!(clamp("a😀", 3), "a😀");
    assert_eq!(clamp("é😀é", 3), "é😀");
}

#[test]
fn media_are_capped_after_the_filter() {
    let mut media: Vec<Value> = vec![json!({"url": "ftp://pbs.twimg.com/a.jpg"}); 5];
    media.extend((0..70).map(|n| json!({"url": format!("https://pbs.twimg.com/media/{n}.jpg")})));
    let post = one(Platform::Twitter, json!({"id": "1", "media": media}));
    assert_eq!(post.media.len(), MAX_MEDIA);
    assert_eq!(
        post.media[0].source_url.as_deref(),
        Some("https://pbs.twimg.com/media/0.jpg")
    );
    assert_eq!(
        post.media[MAX_MEDIA - 1].source_url.as_deref(),
        Some("https://pbs.twimg.com/media/59.jpg")
    );
}

#[test]
fn every_post_passes_the_merge() {
    let items = [
        json!({"id": format!("{PK}_1"), "thumbnailUrl": IG_IMAGE, "mediaType": "", "media": []}),
        json!({"id": "42", "text": "x".repeat(30_000), "mediaType": "carousel"}),
        json!({"id": "123", "mediaType": "video", "media": [{"type": "video", "url": IG_VIDEO}]}),
        // The same post by its shortcode: merged in order by the merge.
        json!({"id": SHORTCODE, "text": "again"}),
    ];
    let batch = sanitize(Platform::Instagram, &items);
    assert_eq!(batch.posts.len(), 4);
    let mut conn = library();
    let tx = conn.transaction().unwrap();
    let summary = upsert_batch(&tx, &batch.posts, UpsertOptions::default(), NOW).unwrap();
    assert_eq!((summary.inserted, summary.merged), (3, 1));
}

// ── Nesting ──────────────────────────────────────────────────────────────────

/// `depth` arrays inside each other around `leaf`.
fn nested(depth: usize, leaf: Value) -> Value {
    let mut value = leaf;
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    value
}

#[test]
fn deep_values_are_not_walked() {
    // The sanitizer reads known fields only: depth costs nothing. (Built by
    // hand: `json!` would serialize the deep values recursively.)
    let deep = || nested(1_000, json!({"id": "1"}));
    let mut entry = Map::new();
    entry.insert("url".into(), deep());
    entry.insert("type".into(), deep());
    let mut item = Map::new();
    item.insert("id".into(), json!("1"));
    item.insert(
        "media".into(),
        Value::Array(vec![deep(), Value::Object(entry)]),
    );
    item.insert("text".into(), deep());
    item.insert("thumbnailUrl".into(), deep());
    item.insert("extra".into(), deep());
    let post = one(Platform::Twitter, Value::Object(item));
    assert!(post.media.is_empty());
    assert_eq!(post.caption.as_deref(), Some(""));
    let batch = sanitize(Platform::Twitter, &[deep()]);
    assert_eq!(batch.rejected[0].code, RejectCode::BadItem);
}

#[test]
fn request_bodies_nest_at_most_128_levels() {
    // The ingest route parses the body with serde_json, whose recursion limit
    // refuses deeper input before the sanitizer sees it.
    let body = |depth: usize| {
        format!(
            r#"[{{"id":"1","extra":{}1{}}}]"#,
            "[".repeat(depth),
            "]".repeat(depth)
        )
    };
    let items: Vec<Value> = serde_json::from_str(&body(120)).unwrap();
    assert_eq!(sanitize(Platform::Twitter, &items).posts.len(), 1);
    assert!(serde_json::from_str::<Vec<Value>>(&body(10_000)).is_err());
}

// ── Properties ───────────────────────────────────────────────────────────────

/// Keys the parsers write, so generated objects reach every branch.
fn key() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::sample::select(vec![
            "id", "platform", "shortcode", "postUrl", "profileUrl", "authorUsername",
            "authorName", "text", "timestamp", "thumbnailUrl", "mediaType", "media",
            "type", "url", "videoUrl",
        ])
        .prop_map(str::to_owned),
        1 => "[a-z]{1,8}",
    ]
}

/// URLs on and off the allowlists, with and without an `oe`.
fn url() -> impl Strategy<Value = String> {
    let scheme = prop::sample::select(vec!["https", "http", "HTTPS", "ftp", "javascript", ""]);
    let host = prop::sample::select(vec![
        "www.instagram.com",
        "scontent.cdninstagram.com",
        "instagram.fmxp1-1.fna.fbcdn.net",
        "x.com",
        "pbs.twimg.com",
        "video.twimg.com",
        "www.pinterest.com",
        "it.pinterest.co.uk",
        "i.pinimg.com",
        "v1.pinimg.com",
        "evil.example",
        "pbs.twimg.com.evil.io",
        "user@pbs.twimg.com",
        "pbs.twimg.com:8443",
        "127.0.0.1",
        "[::1]",
        "",
    ]);
    let path = prop::sample::select(vec![
        "/media/a.jpg",
        "/p/CxKwJ0fLmQZ/",
        "/someone/status/1700000000000000001",
        "/pin/123456/",
        "/videos/x.mp4",
        "/videos/x.m3u8",
        "/",
        "",
        "/a b",
    ]);
    let query = prop_oneof![
        Just(String::new()),
        "[0-9a-fA-F]{0,14}".prop_map(|oe| format!("?oe={oe}")),
        "[ -~]{0,20}".prop_map(|q| format!("?{q}")),
    ];
    (scheme, host, path, query).prop_map(|(s, h, p, q)| format!("{s}://{h}{p}{q}"))
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_map(|f| json!(f)),
        ".{0,24}".prop_map(Value::String),
        url().prop_map(Value::String),
        // Ids the parsers write, and their broken forms.
        "[0-9]{1,25}(_[0-9]{1,12})?".prop_map(Value::String),
        "[A-Za-z0-9_-]{1,70}".prop_map(Value::String),
        // Dates, plausible or not.
        "(19|20|99)[0-9]{2}-0[1-9]-[0-2][0-9]T[0-2][0-9]:[0-5][0-9]:[0-5][0-9](\\.[0-9]{3})?(Z|\\+01:00)?"
            .prop_map(Value::String),
        // Long text: clamping, astral characters at the cut.
        (0usize..4, "[a😀é]{0,3}").prop_map(|(n, tail)| {
            Value::String(format!("{}{tail}", "x".repeat([0, 4_095, 4_097, 20_001][n])))
        }),
    ]
}

fn value() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(5, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((key(), inner), 0..10)
                .prop_map(|fields| Value::Object(fields.into_iter().collect())),
        ]
    })
}

/// The hosts of a platform's allowlist, as its parser writes them.
fn hosts_of(platform: Platform) -> Vec<&'static str> {
    match platform {
        Platform::Instagram => vec![
            "www.instagram.com",
            "scontent-mxp1-1.cdninstagram.com",
            "instagram.fmxp1-1.fna.fbcdn.net",
        ],
        Platform::Twitter => vec!["x.com", "twitter.com", "pbs.twimg.com", "video.twimg.com"],
        _ => vec![
            "www.pinterest.com",
            "it.pinterest.co.uk",
            "i.pinimg.com",
            "v1.pinimg.com",
        ],
    }
}

/// URLs the platform allows, mostly; any URL sometimes.
fn platform_url(platform: Platform) -> impl Strategy<Value = String> {
    let allowed = (
        prop::sample::select(hosts_of(platform)),
        prop::sample::select(vec![
            "/media/a.jpg",
            "/v/t51/1_n.jpg",
            "/p/CxKwJ0fLmQZ/",
            "//status/1700000000000000001",
            "/someone/status/1700000000000000001",
            "/pin/123456/",
            "/videos/mc/720p/x.mp4",
            "/videos/mc/hls/x.m3u8",
            "/ext_tw_video/1/pu/vid/x.MP4",
        ]),
        prop_oneof![
            Just(String::new()),
            (1_600_000_000_i64..2_100_000_000).prop_map(|oe| format!("?stp=dst&oe={oe:X}")),
        ],
    )
        .prop_map(|(host, path, query)| format!("https://{host}{path}{query}"));
    prop_oneof![4 => allowed, 1 => url()]
}

/// Ids in the platform's forms, mostly; any value sometimes.
fn platform_id(platform: Platform) -> BoxedStrategy<Value> {
    let ids = match platform {
        Platform::Instagram => prop_oneof![
            "[1-9][0-9]{8,19}_[0-9]{1,12}",
            "[1-9][0-9]{8,19}",
            "[B-Za-z0-9_-][A-Za-z0-9_-]{5,11}",
        ]
        .boxed(),
        Platform::Twitter => "[1-9][0-9]{0,19}".boxed(),
        _ => "[1-9][0-9]{0,18}".boxed(),
    };
    prop_oneof![
        6 => ids.prop_map(Value::String),
        1 => (1_u64..u64::MAX).prop_map(Value::from),
        1 => leaf(),
    ]
    .boxed()
}

/// ISO dates, most of them in the plausible range; any value sometimes.
fn date() -> impl Strategy<Value = Value> {
    let iso = (
        1995_u32..2040,
        1_u32..=12,
        1_u32..=28,
        0_u32..24,
        0_u32..60,
        0_u32..1_000,
    )
        .prop_map(|(y, mo, d, h, mi, ms)| {
            json!(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:07.{ms:03}Z"))
        });
    prop_oneof![3 => iso, 1 => leaf()]
}

/// Item-like objects of `platform`: its fields with plausible values most of
/// the time, and noise.
fn item(platform: Platform) -> impl Strategy<Value = Value> {
    let media_entry = (
        prop::option::of(prop::sample::select(vec!["image", "video", "VIDEO"])),
        platform_url(platform),
        prop::option::of(platform_url(platform)),
    )
        .prop_map(|(kind, url, video)| {
            let mut entry = Map::new();
            if let Some(kind) = kind {
                entry.insert("type".into(), json!(kind));
            }
            entry.insert("url".into(), json!(url));
            if let Some(video) = video {
                entry.insert("videoUrl".into(), json!(video));
            }
            Value::Object(entry)
        });
    let media = prop_oneof![
        4 => prop::collection::vec(prop_oneof![5 => media_entry, 1 => value()], 0..8)
            .prop_map(Value::Array),
        1 => value(),
    ];
    let media_type = prop_oneof![
        3 => prop::sample::select(vec!["image", "images", "carousel", "video", "text"])
            .prop_map(|t| json!(t)),
        1 => leaf(),
    ];
    let urls = (
        prop::option::of(platform_url(platform)),
        prop::option::of(platform_url(platform)),
        prop::option::of(platform_url(platform)),
    );
    (
        prop::collection::vec((key(), value()), 0..6),
        prop::option::of(platform_id(platform)),
        urls,
        prop::option::of(media),
        prop::option::of(date()),
        prop::option::of(media_type),
        prop::option::of(leaf()),
    )
        .prop_map(
            |(noise, id, (thumbnail, post, profile), media, date, media_type, text)| {
                let mut object: Map<String, Value> = noise.into_iter().collect();
                let fields = [
                    ("id", id),
                    ("thumbnailUrl", thumbnail.map(Value::String)),
                    ("postUrl", post.map(Value::String)),
                    ("profileUrl", profile.map(Value::String)),
                    ("media", media),
                    ("timestamp", date),
                    ("mediaType", media_type),
                    ("text", text),
                ];
                for (key, value) in fields {
                    if let Some(value) = value {
                        object.insert(key.into(), value);
                    }
                }
                Value::Object(object)
            },
        )
}

fn platform() -> impl Strategy<Value = Platform> {
    prop::sample::select(vec![
        Platform::Instagram,
        Platform::Twitter,
        Platform::Pinterest,
    ])
}

/// A URL the post may carry: allowed for its platform.
fn assert_allowed(platform: Platform, url: Option<&str>) -> Result<(), TestCaseError> {
    if let Some(url) = url {
        prop_assert!(hosts::parse_allowed(platform, url).is_some(), "{url}");
        prop_assert!(hosts::utf16_len(url) <= MAX_URL_LEN);
    }
    Ok(())
}

proptest! {
    // Each case builds a library: fewer cases, about 20 items each.
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn arbitrary_batches_stay_bounded(
        (platform, items) in platform().prop_flat_map(|platform| {
            let items = prop_oneof![6 => item(platform), 1 => value()];
            (Just(platform), prop::collection::vec(items, 0..24))
        }),
        now in 1_500_000_000_000_i64..2_000_000_000_000,
    ) {
        let batch = sanitize_batch(platform, &items, now).unwrap();
        prop_assert_eq!(batch.posts.len(), batch.indices.len());
        prop_assert_eq!(batch.posts.len() + batch.rejected.len(), items.len());
        let mut seen: Vec<usize> = batch.indices.clone();
        seen.extend(batch.rejected.iter().map(|r| r.index));
        seen.sort_unstable();
        prop_assert_eq!(seen, (0..items.len()).collect::<Vec<_>>());
        prop_assert!(batch.indices.windows(2).all(|w| w[0] < w[1]));

        for post in &batch.posts {
            prop_assert_eq!(post.platform, platform);
            let prefix = match platform {
                Platform::Instagram => "ig_",
                Platform::Twitter => "x_",
                _ => "pin_",
            };
            prop_assert!(post.key.starts_with(prefix), "{}", post.key);
            prop_assert!(post.key.len() <= 200);
            prop_assert!(!post.native_id.is_empty());
            prop_assert!(MEDIA_TYPES.contains(&post.media_type.as_str()));
            prop_assert!(post.media.len() <= MAX_MEDIA);
            let caption = post.caption.as_deref().unwrap_or_default();
            prop_assert!(hosts::utf16_len(caption) <= MAX_TEXT_LEN);
            prop_assert!(caption.chars().count() <= CAPTION_MAX_CHARS);
            for text in [&post.author_username, &post.author_name, &post.shortcode] {
                prop_assert!(text.as_deref().is_none_or(|t| hosts::utf16_len(t) <= MAX_STRING_LEN));
            }
            if let Some(at) = post.posted_at {
                prop_assert!((MIN_POSTED_AT_MS..=now + MAX_POSTED_AT_AHEAD_MS).contains(&at));
            }
            assert_allowed(platform, post.post_url.as_deref())?;
            assert_allowed(platform, post.profile_url.as_deref())?;
            assert_allowed(platform, post.cover_url.as_deref())?;
            for m in &post.media {
                prop_assert!(m.kind == "image" || m.kind == "video");
                assert_allowed(platform, m.source_url.as_deref())?;
                assert_allowed(platform, m.video_url.as_deref())?;
                prop_assert!(m.object_id.is_none() && m.video_object_id.is_none());
            }
            prop_assert!(post.cover_object.is_none() && post.archive_state.is_none());
            prop_assert!(post.ai.is_empty());
        }

        // The merge takes every post, whatever the batch held.
        let mut conn = library();
        let tx = conn.transaction().unwrap();
        let merged = upsert_batch(&tx, &batch.posts, UpsertOptions::default(), now);
        prop_assert!(merged.is_ok(), "{:?}", merged.err());
    }
}

proptest! {
    #[test]
    fn clean_items_never_panic_on_any_value(platform in platform(), item in value()) {
        if let Ok(clean) = clean_item(platform, &item) {
            prop_assert!(!clean.id.is_empty());
            prop_assert!(hosts::utf16_len(&clean.id) <= MAX_ID_LEN);
            prop_assert!(hosts::utf16_len(&clean.timestamp) <= MAX_TIMESTAMP_LEN);
            prop_assert!(clean.media.len() <= MAX_MEDIA);
        }
    }

    #[test]
    fn clamping_matches_javascript(text in "[a-zé😀\u{10000}-\u{10010}]{0,40}", max in 0usize..50) {
        // `s.slice(0, max)` on UTF-16, minus a trailing lone high surrogate.
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut cut = units[..max.min(units.len())].to_vec();
        if cut.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
            cut.pop();
        }
        prop_assert_eq!(clamp(&text, max), String::from_utf16(&cut).unwrap());
    }
}
