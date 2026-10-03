//! Tests of the archive-state rule: each rule on its own, the agreement with
//! the migration's earlier rule, and the rule on a library.

use proptest::prelude::*;
use rusqlite::{Connection, params};

use super::*;
use crate::repo::media::{NewMediaObject, upsert_object};
use crate::repo::posts::{self, NewMedia, NewPost};
use crate::repo::settings::{SettingsChange, update};
use crate::schema::{self, Kind};

/// 2026-10-02T00:00:00Z.
const NOW: i64 = 1_790_899_200_000;
const PAST: Option<i64> = Some(NOW - 1);
const FUTURE: Option<i64> = Some(NOW + 3_600_000);

const IG: Platform = Platform::Instagram;
const X: Platform = Platform::Twitter;
const PIN: Platform = Platform::Pinterest;

fn post(platform: Platform, media_type: &str) -> PostFacts {
    PostFacts {
        platform,
        media_type: media_type.to_owned(),
        state: ArchiveState::Pending,
        cover: Asset::default(),
        slides: Vec::new(),
    }
}

fn url(expires_at: Option<i64>) -> Asset {
    Asset {
        has_url: true,
        expires_at,
        ..Asset::default()
    }
}

fn stored() -> Asset {
    Asset {
        stored: true,
        has_url: true,
        ..Asset::default()
    }
}

fn failed() -> Asset {
    Asset {
        failed: true,
        ..url(None)
    }
}

fn image(asset: Asset) -> SlideFacts {
    SlideFacts {
        image: true,
        asset,
        video_stored: false,
    }
}

fn video(asset: Asset) -> SlideFacts {
    SlideFacts {
        image: false,
        asset,
        video_stored: false,
    }
}

fn with(mut post: PostFacts, cover: Asset, slides: Vec<SlideFacts>) -> PostFacts {
    post.cover = cover;
    post.slides = slides;
    post
}

fn decide(post: &PostFacts) -> ArchiveState {
    state(post, &ArchivePolicy::default(), NOW)
}

use ArchiveState::{Client, Done, Failed, LinkOnly, Partial, Pending};

#[test]
fn the_server_fetches_what_it_can() {
    // Nothing to store: a text post, everything stored, or no URL at all.
    assert_eq!(decide(&post(X, "text")), Done);
    let all = with(
        post(IG, "carousel"),
        stored(),
        vec![image(stored()), image(stored())],
    );
    assert_eq!(decide(&all), Done);
    let no_url = with(post(X, "image"), stored(), vec![image(Asset::default())]);
    assert_eq!(decide(&no_url), Done);
    // A video is fetched on demand: its stored poster is enough.
    let video_post = with(post(IG, "video"), stored(), vec![video(url(None))]);
    assert_eq!(decide(&video_post), Done);

    let fresh = with(post(IG, "image"), url(FUTURE), vec![image(url(FUTURE))]);
    assert_eq!(decide(&fresh), Pending);
    let some = with(
        post(X, "images"),
        stored(),
        vec![image(stored()), image(url(None))],
    );
    assert_eq!(decide(&some), Partial);
    // An expiry matters on Instagram only.
    let x = with(post(X, "image"), url(PAST), vec![]);
    assert_eq!(decide(&x), Pending);
    // A text post's cover (X's author avatar) is a cover too.
    let avatar = with(post(X, "text"), url(None), vec![]);
    assert_eq!(decide(&avatar), Pending);
}

#[test]
fn expired_instagram_urls_go_to_the_extension_after_the_server() {
    let expired_cover = with(post(IG, "image"), url(PAST), vec![]);
    assert_eq!(decide(&expired_cover), Client);
    // The server first stores the slides it still can.
    let mixed = with(
        post(IG, "carousel"),
        url(PAST),
        vec![image(url(PAST)), image(url(FUTURE))],
    );
    assert_eq!(decide(&mixed), Pending);
    let mixed = with(
        post(IG, "carousel"),
        url(PAST),
        vec![image(stored()), image(url(FUTURE))],
    );
    assert_eq!(decide(&mixed), Partial);
    // Then only the extension can help.
    let left = with(
        post(IG, "carousel"),
        stored(),
        vec![image(stored()), image(url(PAST))],
    );
    assert_eq!(decide(&left), Client);
    // Exactly now is expired.
    let now = with(post(IG, "image"), url(Some(NOW)), vec![]);
    assert_eq!(decide(&now), Client);
}

#[test]
fn failures_count_only_when_nothing_else_can_be_done() {
    let gone = with(post(X, "image"), failed(), vec![image(failed())]);
    assert_eq!(decide(&gone), Failed);
    let half = with(
        post(X, "images"),
        stored(),
        vec![image(failed()), image(url(None))],
    );
    assert_eq!(decide(&half), Partial);
    let refresh = with(
        post(IG, "images"),
        stored(),
        vec![image(failed()), image(url(PAST))],
    );
    assert_eq!(decide(&refresh), Client);
    // A stored or URL-less asset never fails.
    let stored_failed = Asset {
        stored: true,
        ..failed()
    };
    assert_eq!(decide(&with(post(X, "image"), stored_failed, vec![])), Done);
}

#[test]
fn asset_types_choose_what_is_wanted() {
    let carousel = with(
        post(X, "carousel"),
        url(None),
        vec![image(stored()), image(url(None))],
    );
    let policy = |thumbnail, image| ArchivePolicy {
        modes: ArchiveModes::default(),
        assets: ArchiveAssetTypes {
            thumbnail,
            image,
            video: true,
        },
    };
    assert_eq!(state(&carousel, &policy(true, true), NOW), Partial);
    assert_eq!(state(&carousel, &policy(false, true), NOW), Partial);
    let slides_done = with(post(X, "carousel"), url(None), vec![image(stored())]);
    assert_eq!(state(&slides_done, &policy(false, true), NOW), Done);
    assert_eq!(state(&slides_done, &policy(true, false), NOW), Partial);
    let cover_done = with(post(X, "carousel"), stored(), vec![image(url(None))]);
    assert_eq!(state(&cover_done, &policy(true, false), NOW), Done);
    assert_eq!(state(&carousel, &policy(false, false), NOW), Done);
    // `video` never counts.
    let mut no_video = policy(true, true);
    no_video.assets.video = false;
    assert_eq!(state(&carousel, &no_video, NOW), Partial);
}

#[test]
fn modes_choose_who_fetches() {
    let fresh = with(post(PIN, "image"), url(None), vec![image(url(None))]);
    let mut policy = ArchivePolicy::default();
    assert_eq!(policy.modes.pinterest, ArchiveMode::Server, "L17");
    assert_eq!(state(&fresh, &policy, NOW), Pending);
    policy.modes = policy.modes.with(PIN, ArchiveMode::Auto);
    assert_eq!(state(&fresh, &policy, NOW), Pending, "auto is server-first");
    policy.modes = policy.modes.with(PIN, ArchiveMode::Client);
    assert_eq!(state(&fresh, &policy, NOW), Client);
    // Other platforms keep theirs.
    let x = with(post(X, "image"), url(None), vec![]);
    assert_eq!(state(&x, &policy, NOW), Pending);
    // An open breaker hands the platform over, then back.
    let open = ArchiveModes::default().with(X, ArchiveMode::Client);
    let breaker = ArchivePolicy {
        modes: open,
        ..ArchivePolicy::default()
    };
    assert_eq!(state(&x, &breaker, NOW), Client);
    assert_eq!(state(&x, &ArchivePolicy::default(), NOW), Pending);
    // Failures still fail in client mode.
    let gone = with(post(X, "image"), failed(), vec![]);
    assert_eq!(state(&gone, &breaker, NOW), Failed);
    assert_eq!(
        ArchiveModes::default().with(Platform::Web, ArchiveMode::Client),
        ArchiveModes::default()
    );
}

#[test]
fn posts_without_media_are_hydrated() {
    // The server hydrates first, on every platform (L17).
    assert_eq!(decide(&post(IG, "image")), Pending);
    assert_eq!(decide(&post(X, "image")), Pending);
    assert_eq!(decide(&post(PIN, "image")), Pending);
    assert_eq!(
        decide(&post(X, "text")),
        Done,
        "a text post has nothing to hydrate"
    );
    // Instagram handed to the extension: its `hydrate_link`. Only Instagram's
    // extension path hydrates.
    let handed_over = |platform| ArchivePolicy {
        modes: ArchiveModes::default().with(platform, ArchiveMode::Client),
        ..ArchivePolicy::default()
    };
    assert_eq!(state(&post(IG, "image"), &handed_over(IG), NOW), Client);
    assert_eq!(state(&post(X, "image"), &handed_over(X), NOW), Pending);
    // The hydration's verdict stays until the post has media: gated (the
    // extension's turn) or gone.
    for verdict in [Client, Failed] {
        let mut link = post(IG, "image");
        link.state = verdict;
        assert_eq!(decide(&link), verdict);
        link.cover = url(FUTURE);
        assert_eq!(decide(&link), Pending, "hydrated: the archive's turn");
    }
    let mut done = post(X, "image");
    done.state = Done;
    assert_eq!(decide(&done), Pending);
    assert!(needs_hydration(&post(IG, "carousel")));
    // Slides without URL or object are no media either.
    let empty = with(
        post(IG, "carousel"),
        Asset::default(),
        vec![image(Asset::default())],
    );
    assert!(needs_hydration(&empty));
    let kept = SlideFacts {
        video_stored: true,
        ..video(Asset::default())
    };
    assert!(!needs_hydration(&with(
        post(IG, "video"),
        Asset::default(),
        vec![kept]
    )));
    assert!(!needs_hydration(&post(Platform::Web, "website")));
}

#[test]
fn web_and_manual_posts_are_done_or_link_only() {
    for platform in [Platform::Web, Platform::Manual] {
        assert_eq!(decide(&post(platform, "website")), LinkOnly);
        let captured = with(post(platform, "website"), stored(), vec![]);
        assert_eq!(decide(&captured), Done);
        // A remote cover the archive never fetches does not keep it pending.
        let og_image = with(
            post(platform, "website"),
            url(None),
            vec![SlideFacts {
                image: false,
                asset: stored(),
                video_stored: false,
            }],
        );
        assert_eq!(decide(&og_image), Done);
        let file = SlideFacts {
            video_stored: true,
            ..SlideFacts::default()
        };
        assert_eq!(
            decide(&with(post(platform, "file"), Asset::default(), vec![file])),
            Done
        );
        // Their state is derived: a captured link is done.
        let mut link = captured.clone();
        link.state = LinkOnly;
        assert_eq!(decide(&link), Done);
    }
}

#[test]
fn link_only_is_kept_on_social_posts() {
    let mut post = with(post(IG, "image"), url(FUTURE), vec![image(url(FUTURE))]);
    post.state = LinkOnly;
    assert_eq!(decide(&post), LinkOnly);
    post.state = Failed;
    assert_eq!(decide(&post), Pending, "other states are derived");
}

#[test]
fn needs_per_asset() {
    let mode = ArchiveMode::Server;
    assert_eq!(stored().need(IG, mode, NOW), Need::Nothing);
    assert_eq!(Asset::default().need(IG, mode, NOW), Need::Nothing);
    assert_eq!(failed().need(IG, mode, NOW), Need::Failed);
    assert_eq!(url(PAST).need(IG, mode, NOW), Need::Refresh);
    assert_eq!(url(PAST).need(X, mode, NOW), Need::Server);
    assert_eq!(url(FUTURE).need(IG, mode, NOW), Need::Server);
    assert_eq!(url(None).need(PIN, ArchiveMode::Auto, NOW), Need::Server);
    assert_eq!(url(None).need(PIN, ArchiveMode::Client, NOW), Need::Upload);
    assert_eq!(url(PAST).need(IG, ArchiveMode::Client, NOW), Need::Refresh);
}

#[test]
fn values_round_trip() {
    for state in ArchiveState::ALL {
        assert_eq!(state.as_str().parse::<ArchiveState>(), Ok(state));
        assert_eq!(state.to_string(), state.as_str());
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            format!("\"{}\"", state.as_str())
        );
    }
    assert!("Done".parse::<ArchiveState>().is_err());
    for mode in [ArchiveMode::Server, ArchiveMode::Client, ArchiveMode::Auto] {
        assert_eq!(mode.as_str().parse::<ArchiveMode>(), Ok(mode));
    }
    assert!("browser".parse::<ArchiveMode>().is_err());
    assert!(ArchiveMode::Auto.server_fetches());
    assert!(!ArchiveMode::Client.server_fetches());
    let defaults = ArchiveModes::default();
    assert_eq!(defaults.get(IG), Some(ArchiveMode::Server));
    assert_eq!(defaults.get(X), Some(ArchiveMode::Server));
    assert_eq!(defaults.get(PIN), Some(ArchiveMode::Server));
    assert_eq!(defaults.get(Platform::Manual), None);
    let mut counts = StateCounts::default();
    for state in ArchiveState::ALL {
        counts.add(state);
        counts.add(state);
        assert_eq!(counts.get(state), 2);
    }
    assert_eq!(counts.server_work(), 4);
}

// ── Against the migration's earlier rule ─────────────────────────────────────

/// The rule of the bundle builder and the install before P2-02
/// (`archive_state` in `crates/migrate/src/bundle/mod.rs`, `archive_states`
/// in `crates/server/src/migrations/install.rs`).
fn earlier_rule(post: &PostFacts, now: i64) -> ArchiveState {
    let cover_pending = !post.cover.stored && post.cover.has_url;
    let slides_pending = post
        .slides
        .iter()
        .any(|s| s.image && !s.asset.stored && s.asset.has_url);
    let stored = post.cover.stored || post.slides.iter().any(|s| s.asset.stored);
    if !cover_pending && !slides_pending {
        Done
    } else if cover_pending
        && post.platform == IG
        && post.cover.expires_at.is_some_and(|at| at <= now)
    {
        Client
    } else if stored {
        Partial
    } else {
        Pending
    }
}

/// Whether a difference from the earlier rule is one the module documents.
fn documented(post: &PostFacts, earlier: ArchiveState, now: i64) -> bool {
    let new = decide(post);
    let image_needs = || {
        post.slides
            .iter()
            .filter(|s| s.image)
            .map(|s| s.asset.need(post.platform, ArchiveMode::Server, now))
    };
    match post.platform {
        // Web and manual posts are done or link-only.
        Platform::Web | Platform::Manual => {
            matches!(new, Done | LinkOnly) && (new == Done) == post.stores_anything()
        }
        // Posts without media are hydrated.
        _ if needs_hydration(post) => earlier == Done,
        // An expired cover waits until the server stored the slides it can.
        _ if earlier == Client => {
            matches!(new, Pending | Partial) && image_needs().any(|n| n == Need::Server)
        }
        // Expired slides alone go to the extension.
        _ => {
            new == Client
                && matches!(earlier, Pending | Partial)
                && image_needs().any(|n| n == Need::Refresh)
        }
    }
}

fn asset_strategy() -> impl Strategy<Value = Asset> {
    (
        any::<bool>(),
        any::<bool>(),
        prop::sample::select(vec![None, PAST, FUTURE, Some(NOW)]),
    )
        .prop_map(|(stored, has_url, expires_at)| Asset {
            stored,
            has_url,
            expires_at,
            failed: false,
        })
}

prop_compose! {
    fn fresh_post()(
        platform in prop::sample::select(Platform::ALL.to_vec()),
        media_type in prop::sample::select(vec!["image", "images", "carousel", "video", "text", "website", "file"]),
        cover in asset_strategy(),
        slides in prop::collection::vec(
            (any::<bool>(), asset_strategy(), any::<bool>())
                .prop_map(|(image, asset, video_stored)| SlideFacts { image, asset, video_stored }),
            0..5,
        ),
    ) -> PostFacts {
        PostFacts { platform, media_type: media_type.to_owned(), state: Pending, cover, slides }
    }
}

proptest! {
    #[test]
    fn agrees_with_the_earlier_rule_but_where_documented(post in fresh_post()) {
        let earlier = earlier_rule(&post, NOW);
        let new = decide(&post);
        prop_assert!(
            new == earlier || documented(&post, earlier, NOW),
            "{post:?}: earlier {earlier}, now {new}"
        );
    }

    #[test]
    fn only_policies_and_verdicts_are_read_back(post in fresh_post(), state_now in prop::sample::select(ArchiveState::ALL.to_vec())) {
        // `link_only` on social posts and the hydration's verdict on posts
        // without media are read back; the rest is derived from the rows.
        let mut again = post.clone();
        again.state = state_now;
        let next = decide(&again);
        let social = post.platform != Platform::Web && post.platform != Platform::Manual;
        let verdict = needs_hydration(&post) && matches!(state_now, Client | Failed);
        if social && (state_now == LinkOnly || verdict) {
            prop_assert_eq!(next, state_now);
        } else {
            prop_assert_eq!(next, decide(&post));
        }
    }
}

// ── On a library ─────────────────────────────────────────────────────────────

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

fn slide(kind: &str, url: Option<&str>, object_id: Option<i64>) -> NewMedia {
    NewMedia {
        kind: kind.to_owned(),
        source_url: url.map(str::to_owned),
        object_id,
        ..NewMedia::default()
    }
}

fn new_post(key: &str, platform: Platform, media_type: &str) -> NewPost {
    let native = key.split_once('_').unwrap().1;
    NewPost::new(key, platform, native, media_type, NOW - 1_000)
}

fn states(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT key || '=' || archive_state FROM posts ORDER BY key")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// A small library with one post per rule. Returns the ids by key order.
fn seed(conn: &Connection) -> Vec<i64> {
    let cover = object(conn, 1);
    let mut ids = Vec::new();
    let cdn = "https://scontent.cdninstagram.com/a.jpg";
    let mut add = |mut post: NewPost| {
        post.archive_state = Some("pending".to_owned());
        ids.push(posts::insert(conn, &post, NOW).unwrap());
    };
    // ig_1: stored cover, a slide to fetch.
    let mut p = new_post("ig_1", IG, "carousel");
    p.cover_object = Some(cover);
    p.cover_url = Some(cdn.to_owned());
    p.media = vec![
        slide("image", Some(cdn), Some(cover)),
        slide("image", Some(cdn), None),
    ];
    add(p);
    // ig_2: an expired cover, no slides.
    let mut p = new_post("ig_2", IG, "image");
    p.cover_url = Some(cdn.to_owned());
    p.cover_url_expires_at = Some(NOW - 1);
    add(p);
    // ig_3: no media at all.
    add(new_post("ig_3", IG, "image"));
    // x_4: everything stored.
    let mut p = new_post("x_4", X, "image");
    p.cover_object = Some(cover);
    p.media = vec![slide(
        "image",
        Some("https://pbs.twimg.com/a.jpg"),
        Some(cover),
    )];
    add(p);
    // x_5: its only slide is gone (slide 0: the cover's fetch state too).
    let mut p = new_post("x_5", X, "image");
    p.cover_url = Some("https://pbs.twimg.com/b.jpg".to_owned());
    p.media = vec![slide("image", Some("https://pbs.twimg.com/b.jpg"), None)];
    add(p);
    // pin_6: a fresh pin.
    let mut p = new_post("pin_6", PIN, "image");
    p.cover_url = Some("https://i.pinimg.com/c.jpg".to_owned());
    p.media = vec![slide("image", Some("https://i.pinimg.com/c.jpg"), None)];
    add(p);
    // web_7: an uncaptured link.
    add(new_post("web_7", Platform::Web, "website"));
    ids
}

#[test]
fn refresh_derives_and_stores_the_states() {
    let mut conn = library();
    let ids = seed(&conn);
    conn.execute(
        "UPDATE post_media SET fetch_error = 'gone', fetch_attempts = 1
         WHERE post_id = ?1",
        [ids[4]],
    )
    .unwrap();
    let updated_at: Vec<i64> = conn
        .prepare("SELECT updated_at FROM posts ORDER BY key")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    let tx = conn.transaction().unwrap();
    let refreshed = refresh_states(&tx, Scope::All, &ArchivePolicy::default(), NOW).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        states(&conn),
        [
            "ig_1=partial",
            "ig_2=client",
            "ig_3=pending",
            "pin_6=pending",
            "web_7=link_only",
            "x_4=done",
            "x_5=failed",
        ]
    );
    assert_eq!(refreshed.changed, 5, "ig_3 and pin_6 were pending already");
    assert_eq!(
        refreshed.counts,
        StateCounts {
            pending: 2,
            partial: 1,
            done: 1,
            failed: 1,
            client: 1,
            link_only: 1,
        }
    );
    let after: Vec<i64> = conn
        .prepare("SELECT updated_at FROM posts ORDER BY key")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(after, updated_at, "the state is derived data");

    // Again: nothing changes.
    let again = refresh_states(&conn, Scope::All, &ArchivePolicy::default(), NOW).unwrap();
    assert_eq!(again.changed, 0);
    assert_eq!(again.counts, refreshed.counts);
}

#[test]
fn scopes_select_posts() {
    let conn = library();
    let ids = seed(&conn);
    let policy = ArchivePolicy::default();
    let refreshed = refresh_states(&conn, Scope::Posts(&[ids[1], 9_999]), &policy, NOW).unwrap();
    assert_eq!((refreshed.changed, refreshed.counts.client), (1, 1));
    assert_eq!(states(&conn)[1], "ig_2=client");
    assert_eq!(states(&conn)[0], "ig_1=pending", "out of scope");
    let refreshed = refresh_states(&conn, Scope::Posts(&[]), &policy, NOW).unwrap();
    assert_eq!(refreshed, Refreshed::default());

    // A breaker opening on Pinterest moves its posts to the extension, and
    // closing it brings them back.
    let open = ArchivePolicy {
        modes: ArchiveModes::default().with(PIN, ArchiveMode::Client),
        ..policy
    };
    let refreshed = refresh_states(&conn, Scope::Platform(PIN), &open, NOW).unwrap();
    assert_eq!((refreshed.changed, refreshed.counts.client), (1, 1));
    assert!(states(&conn).contains(&"pin_6=client".to_owned()));
    refresh_states(&conn, Scope::Platform(PIN), &policy, NOW).unwrap();
    assert!(states(&conn).contains(&"pin_6=pending".to_owned()));
    assert!(
        states(&conn).contains(&"x_4=pending".to_owned()),
        "out of scope"
    );
}

#[test]
fn load_reads_the_facts() {
    let conn = library();
    let ids = seed(&conn);
    conn.execute(
        "UPDATE post_media SET fetch_attempts = ?2 WHERE post_id = ?1 AND position = 1",
        params![ids[0], FETCH_TRIES],
    )
    .unwrap();
    conn.execute(
        "UPDATE posts SET archive_state = 'link_only' WHERE id = ?1",
        [ids[5]],
    )
    .unwrap();
    let facts = load(&conn, Scope::All).unwrap();
    assert_eq!(facts.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ids);
    let (_, ig1) = &facts[0];
    assert_eq!(ig1.platform, IG);
    assert_eq!(ig1.media_type, "carousel");
    assert!(ig1.cover.stored && ig1.cover.has_url && !ig1.cover.failed);
    assert_eq!(ig1.slides.len(), 2);
    assert!(ig1.slides[0].image && ig1.slides[0].asset.stored);
    assert!(ig1.slides[1].asset.failed, "out of tries");
    // What is left is out of tries.
    assert_eq!(state(ig1, &ArchivePolicy::default(), NOW), Failed);
    let (_, ig2) = &facts[1];
    assert_eq!(ig2.cover.expires_at, Some(NOW - 1));
    assert!(ig2.slides.is_empty());
    let (_, pin) = &facts[5];
    assert_eq!(pin.state, LinkOnly);
    assert_eq!(state(pin, &ArchivePolicy::default(), NOW), LinkOnly);

    // The cover fails with slide 0.
    conn.execute(
        "UPDATE post_media SET fetch_error = 'gone' WHERE post_id = ?1",
        [ids[4]],
    )
    .unwrap();
    let facts = load(&conn, Scope::Posts(&[ids[4]])).unwrap();
    assert_eq!(facts.len(), 1);
    assert!(facts[0].1.cover.failed);
    assert_eq!(state(&facts[0].1, &ArchivePolicy::default(), NOW), Failed);
}

/// A cover without slides (an X text tweet's avatar) keeps its own fetch
/// state in `posts.cover_fetch_*` (library v3); slide 0's wins when there
/// is one.
#[test]
fn a_cover_without_slides_has_its_own_fetch_state() {
    let conn = library();
    let mut avatar = new_post("x_8", X, "text");
    avatar.cover_url = Some("https://pbs.twimg.com/profile_images/1/a.jpg".to_owned());
    let id = posts::insert(&conn, &avatar, NOW).unwrap();
    let policy = ArchivePolicy::default();
    let derive = || {
        let facts = load(&conn, Scope::Posts(&[id])).unwrap();
        state(&facts[0].1, &policy, NOW)
    };
    assert_eq!(derive(), Pending);
    conn.execute(
        "UPDATE posts SET cover_fetch_attempts = ?2 - 1, cover_fetch_error = 'transient'
         WHERE id = ?1",
        params![id, FETCH_TRIES],
    )
    .unwrap();
    assert_eq!(derive(), Pending, "a try is left");
    conn.execute(
        "UPDATE posts SET cover_fetch_attempts = ?2 WHERE id = ?1",
        params![id, FETCH_TRIES],
    )
    .unwrap();
    assert_eq!(derive(), Failed, "out of tries");
    conn.execute(
        "UPDATE posts SET cover_fetch_attempts = 1, cover_fetch_error = 'gone' WHERE id = ?1",
        [id],
    )
    .unwrap();
    assert_eq!(derive(), Failed, "gone");

    // With a slide 0, its state is the cover's: the post's columns are not
    // read.
    let ids = seed(&conn);
    conn.execute(
        "UPDATE posts SET cover_fetch_error = 'gone' WHERE id = ?1",
        [ids[5]],
    )
    .unwrap();
    let facts = load(&conn, Scope::Posts(&[ids[5]])).unwrap();
    assert!(!facts[0].1.cover.failed);
    assert!(fetch_failed(FETCH_TRIES, None) && fetch_failed(0, Some(FETCH_ERROR_GONE)));
    assert!(!fetch_failed(FETCH_TRIES - 1, Some("transient")));
}

#[test]
fn the_policy_reads_the_users_asset_types() {
    let conn = library();
    let policy = ArchivePolicy::read(&conn, ArchiveModes::default()).unwrap();
    assert_eq!(policy, ArchivePolicy::default());
    let types = ArchiveAssetTypes {
        thumbnail: true,
        image: false,
        video: false,
    };
    update(
        &conn,
        &SettingsChange {
            language: None,
            archive_asset_types: Some(types),
            ..SettingsChange::default()
        },
        NOW,
    )
    .unwrap();
    let modes = ArchiveModes::default().with(X, ArchiveMode::Client);
    let policy = ArchivePolicy::read(&conn, modes).unwrap();
    assert_eq!(policy.assets, types);
    assert_eq!(policy.modes, modes);
}
