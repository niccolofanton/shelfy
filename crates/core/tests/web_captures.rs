//! The versions of a website (P4-04): a new version and the post's mirror of
//! it, the AI layer frozen into the outgoing version, the version list, and
//! the two delete modes, "delete a version" and "delete only the report"
//! (WEB-47, WEB-48). After every operation the search indexes match the
//! library and no object is left unreferenced without a GC stamp (§1.2 #5).

mod support;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};
use shelfy_core::ids::{self, IdError};
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::posts::{self, AiLayer, PostFilter, UserContentPatch};
use shelfy_core::repo::{Platform, RepoError, media};
use shelfy_core::schema::Kind;
use shelfy_core::search::index;
use shelfy_core::web::AssetRole;
use shelfy_core::web::captures::{
    self, CaptureStatus, LatestDeleted, NewAsset, NewCapture, SITE_LEVEL,
};
use support::{DAY, NOW, bare_post, dump, library, object};

const URL: &str = "https://www.studio.example.test/";
const FINAL_URL: &str = "https://studio.example.test/";
const DOMAIN: &str = "studio.example.test";

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Distinct objects for one library.
struct Objects {
    next: u8,
}

impl Objects {
    fn new() -> Self {
        Self { next: 1 }
    }

    fn add(&mut self, conn: &Connection, role: &str) -> i64 {
        let n = self.next;
        self.next += 1;
        media::upsert_object(conn, &object(n, role, "webp"), NOW).unwrap()
    }
}

/// A version to insert, with the ids of the objects it references.
struct Version {
    capture: NewCapture,
    assets: Vec<NewAsset>,
    /// The hero of each page.
    heroes: Vec<i64>,
    /// Every object of the version.
    objects: Vec<i64>,
}

impl Version {
    /// Replaces the object of the asset `(page, role, seq)`, to share one
    /// with another version: the same bytes are one object. The object it
    /// replaces is dropped, as a capture would never have recorded it.
    fn share(
        &mut self,
        conn: &Connection,
        page_index: i64,
        role: AssetRole,
        seq: i64,
        object_id: i64,
    ) {
        let asset = self
            .assets
            .iter_mut()
            .find(|a| a.page_index == page_index && a.role == role && a.seq == seq)
            .expect("the asset exists");
        let old = asset.object_id;
        asset.object_id = object_id;
        conn.execute("DELETE FROM media_objects WHERE id = ?1", [old])
            .unwrap();
        for id in self
            .objects
            .iter_mut()
            .chain(self.heroes.iter_mut())
            .chain(self.capture.hero_object.as_mut())
            .chain(self.capture.favicon_object.as_mut())
        {
            if *id == old {
                *id = object_id;
            }
        }
        self.objects.sort_unstable();
        self.objects.dedup();
    }
}

/// A two-page version captured at `at`: per page a hero and two bands, a
/// section and a footer on the home page, and the og image and favicon.
fn version(conn: &Connection, objects: &mut Objects, at: i64, title: &str, text: &str) -> Version {
    let pages = [
        (FINAL_URL.to_owned(), "Home"),
        (format!("{FINAL_URL}work"), "Work"),
    ];
    let mut assets = Vec::new();
    let mut heroes = Vec::new();
    let mut add = |conn: &Connection, page: i64, role: AssetRole, seq: i64, object_role: &str| {
        let id = objects.add(conn, object_role);
        assets.push(NewAsset {
            css_top: (role == AssetRole::Band).then_some(seq * 900),
            css_height: (role == AssetRole::Band).then_some(900),
            ..NewAsset::new(page, role, seq, id)
        });
        id
    };
    for page in 0..2 {
        heroes.push(add(conn, page, AssetRole::Hero, 0, "screenshot"));
        add(conn, page, AssetRole::Band, 0, "band");
        add(conn, page, AssetRole::Band, 1, "band");
    }
    add(conn, 0, AssetRole::Section, 0, "section");
    add(conn, 0, AssetRole::Footer, 0, "footer");
    add(conn, SITE_LEVEL, AssetRole::Og, 0, "og");
    let favicon = add(conn, SITE_LEVEL, AssetRole::Favicon, 0, "favicon");
    let mut objects: Vec<i64> = assets.iter().map(|a| a.object_id).collect();
    objects.sort_unstable();
    Version {
        capture: NewCapture {
            requested_url: Some(URL.into()),
            final_url: Some(FINAL_URL.into()),
            partial: false,
            engine: Some("playwright".into()),
            viewport: Some("1440x900".into()),
            title: Some(title.into()),
            palette: Some(json!([{"hex": "#111111", "role": "text"}])),
            fonts: Some(json!([{"family": "Inter", "role": "body"}])),
            tech: Some(json!(["react"])),
            awards: Some(json!([])),
            meta: Some(json!({
                "description": "Product design studio",
                "ogImage": "https://studio.example.test/og.png"
            })),
            pages: pages
                .iter()
                .enumerate()
                .map(|(i, (url, page_title))| {
                    json!({
                        "url": url,
                        "title": page_title,
                        "pageType": if i == 0 { "home" } else { "work" },
                        "contentText": if i == 0 { "Selected projects" } else { text },
                    })
                })
                .collect(),
            traits: Some(json!({"scroll": "smooth"})),
            hero_object: Some(heroes[0]),
            favicon_object: Some(favicon),
            ..NewCapture::new(at)
        },
        assets,
        heroes,
        objects,
    }
}

/// A new site (a placeholder) with its id.
fn new_site(conn: &Connection) -> i64 {
    let post = captures::placeholder(URL, NOW - 30 * DAY).unwrap();
    posts::insert(conn, &post, NOW - 30 * DAY).unwrap()
}

fn insert(conn: &Connection, post: i64, v: &Version, now: i64) -> i64 {
    captures::insert(conn, post, &v.capture, &v.assets, now).unwrap()
}

/// The checks that hold after every operation: both search indexes match the
/// library, and an object is stamped for the GC exactly when nothing
/// references it (none leaks, none is collected while in use).
fn check(conn: &Connection, after: &str) {
    assert_eq!(index::verify(conn).unwrap(), Vec::<i64>::new(), "{after}");
    let wrong: Vec<(i64, bool)> = conn
        .prepare(
            "SELECT id, unreferenced_since IS NOT NULL FROM media_objects
             WHERE (unreferenced_since IS NOT NULL) = (
               EXISTS (SELECT 1 FROM posts WHERE cover_object = media_objects.id)
               OR EXISTS (SELECT 1 FROM post_media WHERE object_id = media_objects.id)
               OR EXISTS (SELECT 1 FROM post_media WHERE video_object_id = media_objects.id)
               OR EXISTS (SELECT 1 FROM web_captures
                          WHERE hero_object = media_objects.id OR favicon_object = media_objects.id)
               OR EXISTS (SELECT 1 FROM web_capture_assets WHERE object_id = media_objects.id))
             ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        wrong,
        [],
        "{after}: (object, stamped) against its references"
    );
}

/// When each object was stamped, in the order of `ids`.
fn stamps(conn: &Connection, ids: &[i64]) -> Vec<Option<i64>> {
    ids.iter()
        .map(|id| {
            conn.query_row(
                "SELECT unreferenced_since FROM media_objects WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
        })
        .collect()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn search(conn: &Connection, q: &str) -> u64 {
    posts::count(
        conn,
        &PostFilter {
            q: Some(q.into()),
            ..PostFilter::default()
        },
    )
    .unwrap()
}

/// The post's mirror of its current version.
#[derive(Debug, PartialEq)]
struct Mirror {
    current: Option<i64>,
    cover: Option<i64>,
    caption: Option<String>,
    author_username: Option<String>,
    author_name: Option<String>,
    profile_url: Option<String>,
    post_url: Option<String>,
    web_url: Option<String>,
    web_domain: Option<String>,
    web_final_url: Option<String>,
    posted_at: Option<i64>,
    sort_ts: i64,
    media_type: String,
    media_count: i64,
    archive_state: String,
    cover_url: Option<String>,
}

fn mirror(conn: &Connection, post: i64) -> Mirror {
    conn.query_row(
        "SELECT current_capture_id, cover_object, caption, author_username, author_name,
                profile_url, post_url, web_url, web_domain, web_final_url, posted_at, sort_ts,
                media_type, media_count, archive_state, cover_url
         FROM posts WHERE id = ?1",
        [post],
        |r| {
            Ok(Mirror {
                current: r.get(0)?,
                cover: r.get(1)?,
                caption: r.get(2)?,
                author_username: r.get(3)?,
                author_name: r.get(4)?,
                profile_url: r.get(5)?,
                post_url: r.get(6)?,
                web_url: r.get(7)?,
                web_domain: r.get(8)?,
                web_final_url: r.get(9)?,
                posted_at: r.get(10)?,
                sort_ts: r.get(11)?,
                media_type: r.get(12)?,
                media_count: r.get(13)?,
                archive_state: r.get(14)?,
                cover_url: r.get(15)?,
            })
        },
    )
    .unwrap()
}

/// The slides: position, kind, URL, label, object, width and height.
type Slide = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn slides(conn: &Connection, post: i64) -> Vec<Slide> {
    conn.prepare(
        "SELECT position, kind, source_url, label, object_id, width, height FROM post_media
         WHERE post_id = ?1 ORDER BY position",
    )
    .unwrap()
    .query_map([post], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// The AI columns of a post, as text.
fn ai_columns(conn: &Connection, post: i64) -> Vec<Option<String>> {
    conn.query_row(
        "SELECT ai_status, ai_provider, ai_model, CAST(ai_schema_version AS TEXT), ai_error,
                ai_description, ai_save_reason, ai_language, ai_category, ai_content_type,
                ai_tags_json, ai_entities_json, ai_keywords_json, ai_web_json,
                CAST(ai_analyzed_at AS TEXT), CAST(ai_attempts AS TEXT),
                CAST(ai_next_at AS TEXT)
         FROM posts WHERE id = ?1",
        [post],
        |r| (0..17).map(|i| r.get(i)).collect(),
    )
    .unwrap()
}

/// Tag rows of a post: norm, form, source, tier.
fn tag_rows(conn: &Connection, post: i64) -> Vec<(String, String, String, Option<String>)> {
    conn.prepare(
        "SELECT tag_norm, tag_form, source, tier FROM post_tags WHERE post_id = ?1
         ORDER BY source, tag_norm",
    )
    .unwrap()
    .query_map([post], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn entity_rows(conn: &Connection, post: i64) -> Vec<(String, String)> {
    conn.prepare(
        "SELECT ent_norm, ent_form FROM post_entities WHERE post_id = ?1 ORDER BY ent_norm",
    )
    .unwrap()
    .query_map([post], |r| Ok((r.get(0)?, r.get(1)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn snapshot(conn: &Connection, capture: i64) -> Option<Value> {
    conn.query_row(
        "SELECT ai_snapshot_json FROM web_captures WHERE id = ?1",
        [capture],
        |r| r.get::<_, Option<String>>(0),
    )
    .unwrap()
    .map(|raw| serde_json::from_str(&raw).unwrap())
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// A full AI layer, as a catalog analysis (P3) writes it.
fn catalog(description: &str, tag: &str, facet: &str) -> AiLayer {
    AiLayer {
        status: Some("done".into()),
        provider: Some("ornith".into()),
        model: Some("ornith-1.5".into()),
        schema_version: Some(2),
        description: Some(description.into()),
        save_reason: Some("layout reference".into()),
        language: Some("en".into()),
        category: Some("portfolio".into()),
        content_type: Some("showcase".into()),
        tags: strings(&[tag, "Typography", "Grid"]),
        general_tags: Some(strings(&["typography"])),
        specific_tags: Some(strings(&[tag])),
        entities: strings(&["Figma", "Webflow"]),
        keywords: strings(&["studio website"]),
        web: Some(json!({"schema": 2, "facets": {"style": [facet], "tech": ["React"]}})),
        analyzed_at: Some(NOW - DAY),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn a_new_site_is_a_placeholder_keyed_by_its_scheme_less_url() {
    let post = captures::placeholder(URL, NOW).unwrap();
    let id = ids::web::from_url(URL).unwrap();
    assert_eq!(post.key, id.key());
    assert_eq!(post.native_id, id.native_id());
    assert_eq!(
        captures::placeholder("http://studio.example.test", NOW)
            .unwrap()
            .key,
        post.key,
        "http and https, with or without www., are one site"
    );
    assert_eq!(post.platform, Platform::Web);
    assert_eq!(post.media_type, "website");
    assert_eq!(post.web_url.as_deref(), Some(URL));
    assert_eq!(post.web_domain.as_deref(), Some(DOMAIN));
    assert_eq!(post.author_username.as_deref(), Some(DOMAIN));
    assert_eq!(post.author_name.as_deref(), Some(DOMAIN));
    assert_eq!(
        post.profile_url.as_deref(),
        Some("https://studio.example.test")
    );
    assert!(post.caption.is_none() && post.cover_object.is_none() && post.media.is_empty());
    assert!(post.ai.is_none() && post.posted_at.is_none());
    assert_eq!(
        captures::placeholder("  ", NOW).unwrap_err(),
        IdError::Empty
    );

    let conn = library();
    let site = posts::insert(&conn, &post, NOW).unwrap();
    assert_eq!(captures::list(&conn, site).unwrap(), []);
    // The domain finds it.
    assert_eq!(search(&conn, "studio"), 1);
    check(&conn, "placeholder");
}

#[test]
fn a_new_version_becomes_current_and_the_post_mirrors_it() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    let v = version(
        &conn,
        &mut objects,
        NOW - DAY,
        "Studio Example",
        "Cormorant serif",
    );
    let id = insert(&conn, post, &v, NOW);

    // The version and its files.
    let summary = &captures::list(&conn, post).unwrap()[0];
    assert_eq!(
        (
            summary.id,
            summary.status.as_str(),
            summary.partial,
            summary.current
        ),
        (id, "done", false, true)
    );
    assert_eq!(summary.title.as_deref(), Some("Studio Example"));
    let row: (Option<i64>, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT hero_object, favicon_object, ai_snapshot_json FROM web_captures WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, (Some(v.heroes[0]), v.capture.favicon_object, None));
    let mut stored: Vec<i64> = conn
        .prepare("SELECT object_id FROM web_capture_assets WHERE capture_id = ?1")
        .unwrap()
        .query_map([id], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    stored.sort_unstable();
    assert_eq!(stored, v.objects);

    // The post mirrors it.
    assert_eq!(
        mirror(&conn, post),
        Mirror {
            current: Some(id),
            cover: Some(v.heroes[0]),
            caption: Some("Studio Example\n\nProduct design studio".into()),
            author_username: Some(DOMAIN.into()),
            author_name: Some("Studio Example".into()),
            profile_url: Some("https://studio.example.test".into()),
            post_url: Some(FINAL_URL.into()),
            web_url: Some(URL.into()),
            web_domain: Some(DOMAIN.into()),
            web_final_url: Some(FINAL_URL.into()),
            posted_at: Some(NOW - DAY),
            sort_ts: NOW - DAY,
            media_type: "website".into(),
            media_count: 2,
            archive_state: "done".into(),
            cover_url: None,
        }
    );
    assert_eq!(
        slides(&conn, post),
        [
            (
                0,
                "page".into(),
                Some(FINAL_URL.into()),
                Some("Home".into()),
                Some(v.heroes[0]),
                Some(1080),
                Some(1350)
            ),
            (
                1,
                "page".into(),
                Some(format!("{FINAL_URL}work")),
                Some("Work".into()),
                Some(v.heroes[1]),
                Some(1080),
                Some(1350)
            ),
        ]
    );
    // The page text is searchable through the current version.
    assert_eq!(search(&conn, "cormorant"), 1);
    assert_eq!(search(&conn, "product design"), 1);
    check(&conn, "first version");

    // The detail and the gallery read it as the current capture.
    let detail = posts::get(&conn, &posts::keys_of(&conn, &[post]).unwrap()[0])
        .unwrap()
        .unwrap();
    let current = detail.summary.web_capture.unwrap();
    assert_eq!(current.id, id);
    assert_eq!(current.title.as_deref(), Some("Studio Example"));

    // A site in the trash gets its new version without entering the index.
    posts::trash(&conn, &[post], NOW + 1).unwrap();
    let w = version(&conn, &mut objects, NOW, "Studio Example", "Garamond");
    insert(&conn, post, &w, NOW + 2);
    assert_eq!(search(&conn, "garamond"), 0);
    check(&conn, "a version of a trashed site");
    posts::restore(&conn, &[post], NOW + 3).unwrap();
    assert_eq!(search(&conn, "garamond"), 1);
    assert_eq!(search(&conn, "cormorant"), 0);
    check(&conn, "restored");
}

#[test]
fn a_new_version_frees_the_cover_and_slides_that_no_version_holds() {
    // A migrated site can carry a cover and slides that are files of no
    // version: the desktop's thumbnail and its own slide files.
    let conn = library();
    let mut objects = Objects::new();
    let cover = objects.add(&conn, "image");
    let page = objects.add(&conn, "screenshot");
    let mut post = captures::placeholder(URL, NOW - 30 * DAY).unwrap();
    post.cover_object = Some(cover);
    post.media = vec![posts::NewMedia {
        kind: "page".into(),
        source_url: Some(FINAL_URL.into()),
        object_id: Some(page),
        ..posts::NewMedia::default()
    }];
    let site = posts::insert(&conn, &post, NOW - 30 * DAY).unwrap();
    check(&conn, "migrated site");

    let v = version(&conn, &mut objects, NOW, "Studio", "a");
    insert(&conn, site, &v, NOW);
    assert_eq!(stamps(&conn, &[cover, page]), [Some(NOW), Some(NOW)]);
    check(&conn, "new version of a migrated site");
}

#[test]
fn a_new_version_freezes_the_ai_layer_into_the_outgoing_one() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    let first = version(&conn, &mut objects, NOW - 2 * DAY, "Studio", "Old text");
    let v1 = insert(&conn, post, &first, NOW - 2 * DAY);
    posts::set_ai(
        &conn,
        post,
        &catalog("A calm studio site", "Minimal", "minimal"),
        NOW,
    )
    .unwrap();
    let layer = ai_columns(&conn, post);
    let tags = tag_rows(&conn, post);

    let second = version(&conn, &mut objects, NOW - DAY, "Studio", "New text");
    let v2 = insert(&conn, post, &second, NOW);
    assert_eq!(
        snapshot(&conn, v1),
        Some(json!({
            "status": "done",
            "provider": "ornith",
            "model": "ornith-1.5",
            "schemaVersion": 2,
            "error": null,
            "description": "A calm studio site",
            "saveReason": "layout reference",
            "language": "en",
            "category": "portfolio",
            "contentType": "showcase",
            "tags": ["Minimal", "Typography", "Grid"],
            "generalTags": ["Typography"],
            "specificTags": ["Minimal"],
            "entities": ["Figma", "Webflow"],
            "keywords": ["studio website"],
            "web": {"schema": 2, "facets": {"style": ["minimal"], "tech": ["React"]}},
            "analyzedAt": NOW - DAY,
        }))
    );
    assert_eq!(
        snapshot(&conn, v2),
        None,
        "the current version's layer is the post's"
    );
    // The post keeps its layer until the catalog analyzes the new version.
    assert_eq!(ai_columns(&conn, post), layer);
    assert_eq!(tag_rows(&conn, post), tags);
    check(&conn, "second version");

    // Without an AI layer there is nothing to freeze.
    let other = posts::insert(
        &conn,
        &captures::placeholder("https://other.example.test/", NOW).unwrap(),
        NOW,
    )
    .unwrap();
    let a = version(&conn, &mut objects, NOW - DAY, "Other", "x");
    let o1 = insert(&conn, other, &a, NOW);
    let b = version(&conn, &mut objects, NOW, "Other", "y");
    insert(&conn, other, &b, NOW);
    assert_eq!(snapshot(&conn, o1), None);
    check(&conn, "unanalyzed site");
}

#[test]
fn deleting_the_latest_report_restores_the_previous_version_with_its_ai() {
    let conn = library();
    let post = new_site(&conn);
    posts::update_user_content(
        &conn,
        post,
        &UserContentPatch {
            note: Some(Some("for the portfolio".into())),
            tags: Some(strings(&["inspo"])),
        },
        NOW,
    )
    .unwrap();
    let mut objects = Objects::new();
    let first = version(
        &conn,
        &mut objects,
        NOW - 3 * DAY,
        "Studio One",
        "Brutalist grid",
    );
    let v1 = insert(&conn, post, &first, NOW - 3 * DAY);
    posts::set_ai(
        &conn,
        post,
        &catalog("The first look", "Brutalist", "brutalist"),
        NOW,
    )
    .unwrap();
    let first_layer = ai_columns(&conn, post);
    let first_tags = tag_rows(&conn, post);
    let first_entities = entity_rows(&conn, post);
    let first_mirror = mirror(&conn, post);
    let first_slides = slides(&conn, post);

    // The second version shares the favicon; the catalog then analyzes it.
    let mut second = version(
        &conn,
        &mut objects,
        NOW - DAY,
        "Studio Two",
        "Soft gradients",
    );
    second.share(
        &conn,
        SITE_LEVEL,
        AssetRole::Favicon,
        0,
        first.capture.favicon_object.unwrap(),
    );
    let v2 = insert(&conn, post, &second, NOW - DAY);
    let mut changed = catalog("The second look", "Gradient", "colourful");
    changed.entities = strings(&["Framer"]);
    posts::set_ai(&conn, post, &changed, NOW).unwrap();
    conn.execute(
        "UPDATE posts SET thumbhash = X'0102', ai_error = 'provider_down', ai_attempts = 2,
                          ai_next_at = ?2 WHERE id = ?1",
        params![post, NOW + DAY],
    )
    .unwrap();
    assert_eq!(search(&conn, "gradients"), 1);
    check(&conn, "two versions");

    let outcome = captures::delete_latest(&conn, post, NOW + 1).unwrap();
    let only_second: Vec<i64> = second
        .objects
        .iter()
        .copied()
        .filter(|id| !first.objects.contains(id))
        .collect();
    assert_eq!(
        outcome,
        LatestDeleted {
            deleted: Some(v2),
            current: Some(v1),
            stamped: only_second.len(),
            cover_needs_thumbhash: Some(first.heroes[0]),
        }
    );
    // The first version is current again, with its AI layer.
    assert_eq!(mirror(&conn, post), first_mirror);
    assert_eq!(slides(&conn, post), first_slides);
    assert_eq!(ai_columns(&conn, post), first_layer);
    assert_eq!(tag_rows(&conn, post), first_tags);
    assert_eq!(entity_rows(&conn, post), first_entities);
    assert!(
        tag_rows(&conn, post)
            .iter()
            .any(|(norm, _, source, _)| norm == "inspo" && source == "manual"),
        "manual tags stay"
    );
    assert_eq!(snapshot(&conn, v1), None);
    let note: Option<String> = conn
        .query_row("SELECT user_note FROM posts WHERE id = ?1", [post], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(note.as_deref(), Some("for the portfolio"));
    let thumbhash: Option<Vec<u8>> = conn
        .query_row("SELECT thumbhash FROM posts WHERE id = ?1", [post], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(thumbhash, None, "the cover changed");

    // Only the second version's own files are stamped; the shared favicon
    // stays with the first.
    assert_eq!(
        stamps(&conn, &only_second),
        vec![Some(NOW + 1); only_second.len()]
    );
    assert_eq!(
        stamps(&conn, &first.objects),
        vec![None; first.objects.len()]
    );
    assert_eq!(count(&conn, "SELECT count(*) FROM web_captures"), 1);
    assert_eq!(search(&conn, "gradients"), 0);
    assert_eq!(search(&conn, "brutalist"), 1);
    check(&conn, "delete latest, promoted");
}

#[test]
fn deleting_the_only_report_leaves_a_placeholder() {
    let conn = library();
    let post = new_site(&conn);
    let folder = collections::create(
        &conn,
        &NewCollection {
            name: "Sites".into(),
            ..NewCollection::default()
        },
        NOW,
    )
    .unwrap();
    collections::add_posts(&conn, &[post], &[folder.id], NOW).unwrap();
    posts::update_user_content(
        &conn,
        post,
        &UserContentPatch {
            note: Some(Some("check the menu".into())),
            tags: Some(strings(&["Navigation"])),
        },
        NOW,
    )
    .unwrap();
    let mut objects = Objects::new();
    let only = version(
        &conn,
        &mut objects,
        NOW - DAY,
        "Studio Example",
        "Kinetic type",
    );
    let v1 = insert(&conn, post, &only, NOW - DAY);
    posts::set_ai(&conn, post, &catalog("A studio", "Kinetic", "motion"), NOW).unwrap();
    conn.execute(
        "UPDATE posts SET thumbhash = X'0102', ai_attempts = 1 WHERE id = ?1",
        [post],
    )
    .unwrap();

    let outcome = captures::delete_latest(&conn, post, NOW + 1).unwrap();
    assert_eq!(
        outcome,
        LatestDeleted {
            deleted: Some(v1),
            current: None,
            stamped: only.objects.len(),
            cover_needs_thumbhash: None,
        }
    );
    assert_eq!(
        mirror(&conn, post),
        Mirror {
            current: None,
            cover: None,
            caption: None,
            author_username: Some(DOMAIN.into()),
            author_name: Some("Studio Example".into()),
            profile_url: Some("https://studio.example.test".into()),
            post_url: Some(FINAL_URL.into()),
            web_url: Some(URL.into()),
            web_domain: Some(DOMAIN.into()),
            web_final_url: Some(FINAL_URL.into()),
            posted_at: Some(NOW - DAY),
            sort_ts: NOW - DAY,
            media_type: "website".into(),
            media_count: 1,
            archive_state: "pending".into(),
            cover_url: None,
        }
    );
    assert_eq!(slides(&conn, post), []);
    // AI cleared, retry state included; the user's layer and folders stay.
    let mut cleared = vec![None; 17];
    cleared[15] = Some("0".to_owned());
    assert_eq!(ai_columns(&conn, post), cleared);
    assert_eq!(
        tag_rows(&conn, post),
        [(
            "navigation".into(),
            "Navigation".into(),
            "manual".into(),
            None
        )]
    );
    assert_eq!(entity_rows(&conn, post), []);
    let (note, user_tags, thumbhash): (Option<String>, Option<String>, Option<Vec<u8>>) = conn
        .query_row(
            "SELECT user_note, user_tags_json, thumbhash FROM posts WHERE id = ?1",
            [post],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(note.as_deref(), Some("check the menu"));
    assert_eq!(user_tags.as_deref(), Some(r#"["Navigation"]"#));
    assert_eq!(thumbhash, None);
    assert_eq!(
        count(&conn, "SELECT count(*) FROM post_collections"),
        1,
        "folders stay"
    );
    assert_eq!(count(&conn, "SELECT count(*) FROM web_captures"), 0);
    assert_eq!(count(&conn, "SELECT count(*) FROM web_capture_assets"), 0);
    assert_eq!(
        stamps(&conn, &only.objects),
        vec![Some(NOW + 1); only.objects.len()]
    );
    assert_eq!(search(&conn, "kinetic"), 0);
    assert_eq!(search(&conn, "studio"), 1, "the placeholder is still found");
    check(&conn, "placeholder");

    // A placeholder has no report to delete: nothing changes.
    let before = dump(&conn, Kind::Library, "placeholder");
    let again = captures::delete_latest(&conn, post, NOW + 2).unwrap();
    assert_eq!(
        again,
        LatestDeleted {
            deleted: None,
            current: None,
            stamped: 0,
            cover_needs_thumbhash: None,
        }
    );
    assert_eq!(dump(&conn, Kind::Library, "placeholder"), before);

    // A new capture of the placeholder becomes its only version.
    let next = version(&conn, &mut objects, NOW, "Studio Example", "Fresh");
    let v2 = insert(&conn, post, &next, NOW + 3);
    assert_eq!(mirror(&conn, post).current, Some(v2));
    check(&conn, "captured again");
}

#[test]
fn objects_shared_between_versions_are_not_stamped() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    let first = version(&conn, &mut objects, NOW - 2 * DAY, "Studio", "One");
    let v1 = insert(&conn, post, &first, NOW - 2 * DAY);
    // The second capture finds the same favicon, home hero and second band:
    // identical bytes are one object.
    let mut second = version(&conn, &mut objects, NOW - DAY, "Studio", "Two");
    let shared = [
        first.capture.favicon_object.unwrap(),
        first.heroes[0],
        first.assets[2].object_id,
    ];
    second.share(&conn, SITE_LEVEL, AssetRole::Favicon, 0, shared[0]);
    second.share(&conn, 0, AssetRole::Hero, 0, shared[1]);
    second.share(&conn, 0, AssetRole::Band, 1, shared[2]);
    let v2 = insert(&conn, post, &second, NOW - DAY);
    check(&conn, "two versions sharing files");

    let only_first: Vec<i64> = first
        .objects
        .iter()
        .copied()
        .filter(|id| !shared.contains(id))
        .collect();
    let stamped = captures::delete_version(&conn, post, v1, NOW).unwrap();
    assert_eq!(stamped, only_first.len());
    assert_eq!(
        stamps(&conn, &only_first),
        vec![Some(NOW); only_first.len()]
    );
    assert_eq!(stamps(&conn, &shared), [None, None, None]);
    assert_eq!(
        stamps(&conn, &second.objects),
        vec![None; second.objects.len()]
    );
    // The post did not change: it mirrors the second version.
    assert_eq!(mirror(&conn, post).current, Some(v2));
    assert_eq!(mirror(&conn, post).cover, Some(shared[1]));
    check(&conn, "older version deleted");

    // The same hero keeps the ThumbHash; a new one drops it.
    conn.execute("UPDATE posts SET thumbhash = X'0102' WHERE id = ?1", [post])
        .unwrap();
    let mut third = version(&conn, &mut objects, NOW, "Studio", "Three");
    third.share(&conn, 0, AssetRole::Hero, 0, shared[1]);
    insert(&conn, post, &third, NOW);
    let thumbhash = |conn: &Connection| -> Option<Vec<u8>> {
        conn.query_row("SELECT thumbhash FROM posts WHERE id = ?1", [post], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(thumbhash(&conn), Some(vec![1, 2]));
    let fourth = version(&conn, &mut objects, NOW + 1, "Studio", "Four");
    insert(&conn, post, &fourth, NOW + 1);
    assert_eq!(thumbhash(&conn), None);
    check(&conn, "new heroes");
}

#[test]
fn every_band_section_and_frame_is_referenced_and_freed_with_its_version() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    // Every role: a tall home page (screenshot, hero, bands, sections,
    // footer), a scroll-jacked page (hero, filmstrip frames), and the site's
    // og image, favicon and scroll video.
    let mut assets = Vec::new();
    let mut add = |conn: &Connection, page: i64, role: AssetRole, seq: i64, object_role: &str| {
        let id = objects.add(conn, object_role);
        assets.push(NewAsset::new(page, role, seq, id));
        id
    };
    let hero = add(&conn, 0, AssetRole::Hero, 0, "screenshot");
    add(&conn, 0, AssetRole::Screenshot, 0, "screenshot");
    for seq in 0..3 {
        add(&conn, 0, AssetRole::Band, seq, "band");
    }
    for seq in 0..2 {
        add(&conn, 0, AssetRole::Section, seq, "section");
    }
    add(&conn, 0, AssetRole::Footer, 0, "footer");
    add(&conn, 1, AssetRole::Hero, 0, "screenshot");
    for seq in 0..4 {
        add(&conn, 1, AssetRole::Filmstrip, seq, "filmstrip");
    }
    add(&conn, SITE_LEVEL, AssetRole::Og, 0, "og");
    let favicon = add(&conn, SITE_LEVEL, AssetRole::Favicon, 0, "favicon");
    add(&conn, SITE_LEVEL, AssetRole::Video, 0, "video");
    add(&conn, SITE_LEVEL, AssetRole::VideoPreview, 0, "preview");
    add(&conn, SITE_LEVEL, AssetRole::VideoPoster, 0, "poster");
    let mut all: Vec<i64> = assets.iter().map(|a| a.object_id).collect();
    all.sort_unstable();
    let capture = NewCapture {
        final_url: Some(FINAL_URL.into()),
        title: Some("Studio".into()),
        pages: vec![
            json!({"url": FINAL_URL, "title": "Home"}),
            json!({"url": format!("{FINAL_URL}story"), "title": "Story", "jacked": true}),
        ],
        hero_object: None,
        favicon_object: Some(favicon),
        ..NewCapture::new(NOW - DAY)
    };
    let v1 = captures::insert(&conn, post, &capture, &assets, NOW - DAY).unwrap();

    // Every file is a row of web_capture_assets.
    let mut referenced: Vec<i64> = conn
        .prepare("SELECT DISTINCT object_id FROM web_capture_assets WHERE capture_id = ?1")
        .unwrap()
        .query_map([v1], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    referenced.sort_unstable();
    assert_eq!(referenced, all);
    assert_eq!(count(&conn, "SELECT count(*) FROM media_objects"), 18);
    // Without an explicit hero, the first page's image is the hero: its
    // screenshot, as the desktop's `screenshotPath`.
    let screenshot = assets[1].object_id;
    assert_eq!(mirror(&conn, post).cover, Some(screenshot));
    let detail = captures::get(&conn, post, v1).unwrap().unwrap();
    assert_eq!(
        detail.summary.hero.as_ref().map(|h| h.bytes),
        Some(1000 + 2),
        "the screenshot is object 2"
    );
    assert_ne!(Some(hero), Some(screenshot));
    // The jacked page's slide is its hero.
    let slide_objects: Vec<Option<i64>> = slides(&conn, post).into_iter().map(|s| s.4).collect();
    assert_eq!(slide_objects, [Some(screenshot), Some(assets[8].object_id)]);
    check(&conn, "every role");

    // A second version replaces it; deleting the first frees all its files.
    let next = version(&conn, &mut objects, NOW, "Studio", "Next");
    insert(&conn, post, &next, NOW);
    assert_eq!(
        captures::delete_version(&conn, post, v1, NOW + 1).unwrap(),
        18
    );
    assert_eq!(stamps(&conn, &all), vec![Some(NOW + 1); all.len()]);
    check(&conn, "first version deleted");

    // Deleting the last report frees the second version's files too.
    let outcome = captures::delete_latest(&conn, post, NOW + 2).unwrap();
    assert_eq!(outcome.stamped, next.objects.len());
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM media_objects WHERE unreferenced_since IS NULL"
        ),
        0,
        "no file outlives its versions"
    );
    check(&conn, "all versions deleted");
}

#[test]
fn versions_list_newest_first_with_their_files() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    let a = version(&conn, &mut objects, NOW - 3 * DAY, "First", "a");
    let v1 = insert(&conn, post, &a, NOW - 3 * DAY);
    posts::set_ai(
        &conn,
        post,
        &catalog("First look", "Minimal", "minimal"),
        NOW,
    )
    .unwrap();
    let mut b = version(&conn, &mut objects, NOW - DAY, "Second", "b");
    b.capture.partial = true;
    b.capture.status = CaptureStatus::Blocked;
    let v2 = insert(&conn, post, &b, NOW - DAY);
    // A capture that ran earlier but finished later sorts by capture time.
    let c = version(&conn, &mut objects, NOW - 2 * DAY, "Third", "c");
    let v3 = insert(&conn, post, &c, NOW);

    let list = captures::list(&conn, post).unwrap();
    let order: Vec<(i64, bool, &str, bool, i64, i64)> = list
        .iter()
        .map(|s| {
            (
                s.id,
                s.current,
                s.status.as_str(),
                s.partial,
                s.page_count,
                s.asset_count,
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            (v2, false, "blocked", true, 2, 10),
            (v3, true, "done", false, 2, 10),
            (v1, false, "done", false, 2, 10),
        ]
    );
    assert_eq!(list[2].title.as_deref(), Some("First"));
    assert_eq!(list[2].captured_at, NOW - 3 * DAY);
    assert_eq!(list[2].created_at, NOW - 3 * DAY);
    assert!(list.iter().all(|s| s.hero.is_some() && s.favicon.is_some()));

    let detail = captures::get(&conn, post, v1).unwrap().unwrap();
    assert_eq!(detail.summary, list[2]);
    assert_eq!(detail.engine.as_deref(), Some("playwright"));
    assert_eq!(detail.viewport.as_deref(), Some("1440x900"));
    assert_eq!(
        detail.palette,
        Some(json!([{"hex": "#111111", "role": "text"}]))
    );
    assert_eq!(detail.tech, Some(json!(["react"])));
    assert_eq!(detail.traits, Some(json!({"scroll": "smooth"})));
    assert_eq!(detail.pages.len(), 2);
    assert_eq!(detail.pages[1]["title"], "Work");
    assert_eq!(
        detail
            .ai_snapshot
            .as_ref()
            .map(|s| s["description"].clone()),
        Some(json!("First look"))
    );
    let files: Vec<(i64, &str, i64, Option<i64>)> = detail
        .assets
        .iter()
        .map(|f| (f.page_index, f.role.as_str(), f.seq, f.css_top))
        .collect();
    assert_eq!(
        files,
        [
            (SITE_LEVEL, "favicon", 0, None),
            (SITE_LEVEL, "og", 0, None),
            (0, "band", 0, Some(0)),
            (0, "band", 1, Some(900)),
            (0, "footer", 0, None),
            (0, "hero", 0, None),
            (0, "section", 0, None),
            (1, "band", 0, Some(0)),
            (1, "band", 1, Some(900)),
            (1, "hero", 0, None),
        ]
    );
    assert_eq!(detail.assets[5].object.width, Some(1080));
    assert!(
        captures::get(&conn, post, v3)
            .unwrap()
            .unwrap()
            .ai_snapshot
            .is_none()
    );

    // Another site's versions are not this one's.
    let other = posts::insert(
        &conn,
        &captures::placeholder("https://other.example.test/", NOW).unwrap(),
        NOW,
    )
    .unwrap();
    assert_eq!(captures::get(&conn, other, v1).unwrap(), None);
    assert_eq!(captures::list(&conn, other).unwrap(), []);
    check(&conn, "three versions");
}

#[test]
fn only_older_versions_of_a_site_are_deleted_one_by_one() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    let a = version(&conn, &mut objects, NOW - DAY, "Studio", "a");
    let v1 = insert(&conn, post, &a, NOW - DAY);
    let b = version(&conn, &mut objects, NOW, "Studio", "b");
    let v2 = insert(&conn, post, &b, NOW);
    let ig = posts::insert(&conn, &bare_post("ig_42", Platform::Instagram, NOW), NOW).unwrap();
    let other = posts::insert(
        &conn,
        &captures::placeholder("https://other.example.test/", NOW).unwrap(),
        NOW,
    )
    .unwrap();
    let before = dump(&conn, Kind::Library, "two versions");

    assert!(matches!(
        captures::delete_version(&conn, post, v2, NOW),
        Err(RepoError::Conflict("current version"))
    ));
    for (site, version) in [(other, v1), (post, 9999), (ig, v1), (9999, v1)] {
        assert!(
            matches!(
                captures::delete_version(&conn, site, version, NOW),
                Err(RepoError::NotFound)
            ),
            "site {site}, version {version}"
        );
    }
    for site in [ig, 9999] {
        assert!(matches!(
            captures::delete_latest(&conn, site, NOW),
            Err(RepoError::NotFound)
        ));
        assert!(matches!(
            captures::insert(&conn, site, &a.capture, &a.assets, NOW),
            Err(RepoError::NotFound)
        ));
    }
    assert_eq!(dump(&conn, Kind::Library, "two versions"), before);

    captures::delete_version(&conn, post, v1, NOW).unwrap();
    let ids: Vec<i64> = captures::list(&conn, post)
        .unwrap()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, [v2]);
    check(&conn, "older version deleted");
}

#[test]
fn a_new_version_is_checked_before_anything_is_written() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    let good = version(&conn, &mut objects, NOW - DAY, "Studio", "a");
    insert(&conn, post, &good, NOW - DAY);
    let before = dump(&conn, Kind::Library, "one version");

    // Each attempt records its objects and fails in one transaction, as the
    // capture ingest does; the insert itself writes nothing.
    let mut attempt = |field: &str, spoil: &dyn Fn(&mut Version)| {
        conn.execute_batch("SAVEPOINT attempt").unwrap();
        let mut v = version(&conn, &mut objects, NOW, "Studio", "b");
        spoil(&mut v);
        let recorded = dump(&conn, Kind::Library, "attempt");
        let err = captures::insert(&conn, post, &v.capture, &v.assets, NOW).unwrap_err();
        assert!(
            matches!(&err, RepoError::Invalid { field: f, .. } if *f == field),
            "{field}: {err:?}"
        );
        assert_eq!(dump(&conn, Kind::Library, "attempt"), recorded, "{field}");
        conn.execute_batch("ROLLBACK TO attempt; RELEASE attempt")
            .unwrap();
    };
    attempt("assets.pageIndex", &|v| v.assets[0].page_index = 2);
    attempt("assets.pageIndex", &|v| {
        v.assets
            .push(NewAsset::new(0, AssetRole::Og, 1, v.objects[0]));
    });
    attempt("assets.pageIndex", &|v| {
        v.assets
            .push(NewAsset::new(SITE_LEVEL, AssetRole::Band, 5, v.objects[0]));
    });
    attempt("assets.seq", &|v| v.assets[0].seq = -1);
    attempt("assets", &|v| {
        let copy = v.assets[1];
        v.assets.push(NewAsset {
            object_id: v.objects[0],
            ..copy
        });
    });
    attempt("objectId", &|v| v.assets[0].object_id = 9999);
    attempt("objectId", &|v| v.capture.hero_object = Some(9999));
    attempt("objectId", &|v| v.capture.favicon_object = Some(9998));
    attempt("pages", &|v| v.capture.pages.push(json!("not an object")));

    assert_eq!(dump(&conn, Kind::Library, "one version"), before);
    check(&conn, "refused versions");
}

#[test]
fn a_blocked_site_keeps_its_og_image_as_a_version_without_pages() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    let og = objects.add(&conn, "og");
    let capture = NewCapture {
        requested_url: Some(URL.into()),
        status: CaptureStatus::Blocked,
        partial: true,
        meta: Some(
            json!({"description": "A studio", "ogImage": "https://studio.example.test/og.png"}),
        ),
        hero_object: Some(og),
        ..NewCapture::new(NOW)
    };
    let id = captures::insert(
        &conn,
        post,
        &capture,
        &[NewAsset::new(SITE_LEVEL, AssetRole::Og, 0, og)],
        NOW,
    )
    .unwrap();
    let m = mirror(&conn, post);
    assert_eq!((m.current, m.cover, m.media_count), (Some(id), Some(og), 1));
    assert_eq!(m.caption.as_deref(), Some("A studio"));
    assert_eq!(
        m.author_name.as_deref(),
        Some(DOMAIN),
        "no title: the domain"
    );
    assert_eq!(
        m.post_url.as_deref(),
        Some(URL),
        "no final URL: the requested one"
    );
    assert_eq!(
        m.web_final_url.as_deref(),
        Some(URL),
        "the placeholder's stays"
    );
    assert_eq!(slides(&conn, post), []);
    let summary = &captures::list(&conn, post).unwrap()[0];
    assert_eq!(
        (summary.status.as_str(), summary.page_count),
        ("blocked", 0)
    );
    check(&conn, "blocked");
}

#[test]
fn a_migrated_version_restores_the_desktop_snapshot() {
    let conn = library();
    let post = new_site(&conn);
    let mut objects = Objects::new();
    let old = version(&conn, &mut objects, NOW - 2 * DAY, "Old", "a");
    let v1 = insert(&conn, post, &old, NOW - 2 * DAY);
    let new = version(&conn, &mut objects, NOW - DAY, "New", "b");
    insert(&conn, post, &new, NOW - DAY);
    posts::set_ai(&conn, post, &catalog("Now", "Fresh", "fresh"), NOW).unwrap();
    // The frozen layer as the migration (P1-19) writes a desktop snapshot.
    conn.execute(
        "UPDATE web_captures SET ai_snapshot_json = ?2 WHERE id = ?1",
        params![
            v1,
            json!({
                "description": "Desktop analysis",
                "tags": ["Retro", "retro", "Serif"],
                "model": "llava",
                "status": "done",
                "analyzedAt": NOW - 10 * DAY,
                "category": "portfolio",
                "contentType": "showcase",
                "entities": ["Webflow"],
                "keywords": ["grid"],
                "language": "it",
                "saveReason": "fonts",
                "web": null,
            })
            .to_string()
        ],
    )
    .unwrap();
    captures::delete_latest(&conn, post, NOW).unwrap();
    assert_eq!(
        ai_columns(&conn, post),
        [
            Some("done".to_owned()),
            None,
            Some("llava".to_owned()),
            None,
            None,
            Some("Desktop analysis".to_owned()),
            Some("fonts".to_owned()),
            Some("it".to_owned()),
            Some("portfolio".to_owned()),
            Some("showcase".to_owned()),
            Some(r#"["Retro","retro","Serif"]"#.to_owned()),
            Some(r#"["Webflow"]"#.to_owned()),
            Some(r#"["grid"]"#.to_owned()),
            None,
            Some((NOW - 10 * DAY).to_string()),
            Some("0".to_owned()),
            None,
        ]
    );
    assert_eq!(
        tag_rows(&conn, post),
        [
            ("retro".into(), "Retro".into(), "ai".into(), None),
            ("serif".into(), "Serif".into(), "ai".into(), None),
        ]
    );
    assert_eq!(
        entity_rows(&conn, post),
        [("webflow".into(), "Webflow".into())]
    );

    // A version frozen without an analysis restores none.
    let unanalyzed = posts::insert(
        &conn,
        &captures::placeholder("https://plain.example.test/", NOW).unwrap(),
        NOW,
    )
    .unwrap();
    let a = version(&conn, &mut objects, NOW - DAY, "Plain", "a");
    insert(&conn, unanalyzed, &a, NOW - DAY);
    let b = version(&conn, &mut objects, NOW, "Plain", "b");
    insert(&conn, unanalyzed, &b, NOW);
    posts::set_ai(&conn, unanalyzed, &catalog("Later", "Plain", "plain"), NOW).unwrap();
    captures::delete_latest(&conn, unanalyzed, NOW + 1).unwrap();
    let status: Option<String> = conn
        .query_row(
            "SELECT ai_status FROM posts WHERE id = ?1",
            [unanalyzed],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten();
    assert_eq!(status, None, "unanalyzed again");
    check(&conn, "migrated snapshots");
}
