//! The search indexes (`posts_fts`, and `posts_infix` since P1-05) stay in
//! step with every write that changes searchable text (plan §2.7): inserts,
//! updates and deletes.

mod support;

use rusqlite::Connection;
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, AiLayer, PageRequest, PostFilter, Sort, UserContentPatch};
use shelfy_core::search::index;
use support::{NOW, bare_post, fixture_library, insert_all, library, synthetic_posts};

/// Keys of the posts the FTS index returns for `q` (trash included), sorted.
fn fts_hits(conn: &Connection, q: &str) -> Vec<String> {
    let mut keys: Vec<String> = conn
        .prepare(
            "SELECT p.key FROM posts_fts f JOIN posts p ON p.id = f.rowid
             WHERE posts_fts MATCH ?1",
        )
        .unwrap()
        .query_map([q], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    keys.sort();
    keys
}

/// Keys the gallery search returns for `q` (trash excluded), sorted.
fn search(conn: &Connection, q: &str) -> Vec<String> {
    let filter = PostFilter {
        q: Some(q.into()),
        ..PostFilter::default()
    };
    let mut keys: Vec<String> = posts::list_ids(conn, &filter)
        .unwrap()
        .into_iter()
        .map(|id| {
            conn.query_row("SELECT key FROM posts WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        })
        .collect();
    keys.sort();
    keys
}

fn index_rows(conn: &Connection) -> i64 {
    // A contentless table still counts its rows through the docsize shadow table.
    conn.query_row("SELECT count(*) FROM posts_fts_docsize", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn inserts_are_searchable_in_every_column() {
    let conn = library();
    fixture_library(&conn);
    let carousel = ["ig_3101"];
    assert_eq!(search(&conn, "soffiato"), carousel); // caption
    assert_eq!(search(&conn, "wooden"), carousel); // AI description
    assert_eq!(search(&conn, "soggiorno"), carousel); // note
    assert_eq!(search(&conn, "murano"), carousel); // entity
    assert_eq!(search(&conn, "lamp"), carousel); // keyword, by prefix
    assert_eq!(search(&conn, "headphones"), carousel); // alias-resolved tag
    assert_eq!(search(&conn, "cuffie"), carousel); // the tag as written
    assert_eq!(
        search(&conn, "studio.example"),
        ["ig_3101", "web_00a1b2c3d4e5f6a7b8c9"]
    ); // author
    assert_eq!(search(&conn, "selected"), ["web_00a1b2c3d4e5f6a7b8c9"]); // page digest
    assert_eq!(search(&conn, "client"), ["m_01J9Z3B8K4QW6TFX0V7G2N5RCE"]);
}

#[test]
fn search_folds_case_and_diacritics_and_matches_prefixes() {
    let conn = library();
    let mut a = bare_post("ig_1", Platform::Instagram, NOW);
    a.caption = Some("Città PERCHÉ Photography".into());
    insert_all(&conn, &[a]);
    for q in ["citta", "città", "CITTÀ", "perche", "photo", "PHOT"] {
        assert_eq!(search(&conn, q), ["ig_1"], "{q}");
    }
    assert!(search(&conn, "photographs").is_empty());
}

/// A term inside a longer token (a compound hashtag) is found through the
/// infix index (P1-05), and ranks below a whole-token or prefix hit.
#[test]
fn terms_inside_longer_tokens_match_and_rank_last() {
    let conn = library();
    let mut inside = bare_post("ig_1", Platform::Instagram, NOW);
    inside.caption = Some("#simulazionefluidi #productdesign".into());
    let mut token = bare_post("ig_2", Platform::Instagram, NOW - 1);
    token.caption = Some("fluidi e design".into());
    let mut none = bare_post("ig_3", Platform::Instagram, NOW - 2);
    none.caption = Some("fluid desi".into());
    let ids = insert_all(&conn, &[inside, token, none]);
    for q in ["fluidi", "design", "imulazion", "FLUIDI", "productdesign"] {
        let mut expected = if q == "productdesign" || q == "imulazion" {
            vec!["ig_1"]
        } else {
            vec!["ig_1", "ig_2"]
        };
        expected.sort_unstable();
        assert_eq!(search(&conn, q), expected, "{q}");
    }
    // Too short for a trigram: "ui" is inside "fluidi", but only token
    // prefixes match it.
    assert!(search(&conn, "ui").is_empty());
    assert_eq!(search(&conn, "fl"), ["ig_2", "ig_3"]);
    let ranked: Vec<String> = posts::list(
        &conn,
        &PostFilter {
            q: Some("fluidi".into()),
            ..PostFilter::default()
        },
        &PageRequest {
            sort: Sort::Relevance,
            ..PageRequest::default()
        },
    )
    .unwrap()
    .items
    .into_iter()
    .map(|p| p.key)
    .collect();
    assert_eq!(ranked, ["ig_2", "ig_1"], "the whole token first");

    // Writes keep the infix index in step: trash, restore, edit, purge.
    posts::trash(&conn, &ids[..1], NOW).unwrap();
    assert_eq!(search(&conn, "imulazion"), Vec::<String>::new());
    posts::restore(&conn, &ids[..1], NOW).unwrap();
    assert_eq!(search(&conn, "imulazion"), ["ig_1"]);
    let patch = UserContentPatch {
        note: Some(Some("#moodboardceramica".into())),
        tags: None,
    };
    posts::update_user_content(&conn, ids[2], &patch, NOW).unwrap();
    assert_eq!(search(&conn, "boardcera"), ["ig_3"]);
    posts::purge(&conn, &ids[2..], NOW).unwrap();
    assert!(search(&conn, "boardcera").is_empty());
    assert_eq!(index::verify(&conn).unwrap(), Vec::<i64>::new());
}

#[test]
fn updates_reindex_the_post() {
    let conn = library();
    let mut p = bare_post("ig_1", Platform::Instagram, NOW);
    p.caption = Some("ceramic vase".into());
    let id = posts::insert(&conn, &p, NOW).unwrap();

    // Note and manual tags.
    let patch = UserContentPatch {
        note: Some(Some("gift for Giulia".into())),
        tags: Some(vec!["Pottery".into()]),
    };
    posts::update_user_content(&conn, id, &patch, NOW).unwrap();
    assert_eq!(search(&conn, "giulia"), ["ig_1"]);
    assert_eq!(search(&conn, "pottery"), ["ig_1"]);
    let patch = UserContentPatch {
        note: Some(None),
        tags: Some(vec!["Stoneware".into()]),
    };
    posts::update_user_content(&conn, id, &patch, NOW).unwrap();
    assert!(search(&conn, "giulia").is_empty());
    assert!(search(&conn, "pottery").is_empty());
    assert_eq!(search(&conn, "stoneware"), ["ig_1"]);

    // The AI layer, replaced and then cleared.
    let ai = AiLayer {
        description: Some("A glazed bowl".into()),
        tags: vec!["Glaze".into()],
        keywords: vec!["kiln".into()],
        entities: vec!["Faenza".into()],
        ..AiLayer::default()
    };
    posts::set_ai(&conn, id, &ai, NOW).unwrap();
    for q in ["glazed", "glaze", "kiln", "faenza"] {
        assert_eq!(search(&conn, q), ["ig_1"], "{q}");
    }
    posts::set_ai(
        &conn,
        id,
        &AiLayer {
            description: Some("A teapot".into()),
            ..AiLayer::default()
        },
        NOW,
    )
    .unwrap();
    assert!(search(&conn, "kiln").is_empty());
    assert_eq!(search(&conn, "teapot"), ["ig_1"]);
    posts::clear_ai(&conn, id, NOW).unwrap();
    assert!(search(&conn, "teapot").is_empty());
    // Text owned by other layers survives.
    assert_eq!(search(&conn, "ceramic"), ["ig_1"]);
    assert_eq!(search(&conn, "stoneware"), ["ig_1"]);
    assert_eq!(index_rows(&conn), 1);
}

#[test]
fn deletes_leave_the_index_consistent() {
    let conn = library();
    let mut a = bare_post("ig_1", Platform::Instagram, NOW);
    a.caption = Some("brutalist concrete house".into());
    let mut b = bare_post("ig_2", Platform::Instagram, NOW);
    b.caption = Some("concrete stool".into());
    let ids = insert_all(&conn, &[a, b]);

    // Trash removes the index row; restore brings it back.
    posts::trash(&conn, &[ids[0]], NOW).unwrap();
    assert_eq!(search(&conn, "concrete"), ["ig_2"]);
    assert!(fts_hits(&conn, "brutalist").is_empty());
    assert_eq!(index_rows(&conn), 1);
    // Edits while in the trash do not index the post again.
    let patch = UserContentPatch {
        note: Some(Some("tadao".into())),
        tags: None,
    };
    posts::update_user_content(&conn, ids[0], &patch, NOW).unwrap();
    assert!(fts_hits(&conn, "tadao").is_empty());
    posts::restore(&conn, &[ids[0]], NOW).unwrap();
    assert_eq!(search(&conn, "brutalist"), ["ig_1"]);
    assert_eq!(search(&conn, "tadao"), ["ig_1"]);
    assert_eq!(index_rows(&conn), 2);

    // Purge removes the index row.
    posts::purge(&conn, &[ids[1]], NOW).unwrap();
    assert_eq!(search(&conn, "concrete"), ["ig_1"]);
    assert_eq!(index_rows(&conn), 1);

    // SQLite reuses the rowid of the deleted maximum: the new post must not
    // inherit the old post's terms.
    let mut c = bare_post("ig_3", Platform::Instagram, NOW);
    c.caption = Some("linen curtains".into());
    let reused = posts::insert(&conn, &c, NOW).unwrap();
    assert_eq!(reused, ids[1]);
    assert!(search(&conn, "stool").is_empty());
    assert!(fts_hits(&conn, "stool").is_empty());
    assert_eq!(search(&conn, "linen"), ["ig_3"]);
}

#[test]
fn posts_without_text_have_no_index_row() {
    let conn = library();
    let id = posts::insert(&conn, &bare_post("ig_1", Platform::Instagram, NOW), NOW).unwrap();
    assert_eq!(index_rows(&conn), 0);
    posts::update_user_content(
        &conn,
        id,
        &UserContentPatch {
            note: Some(Some("hello".into())),
            tags: None,
        },
        NOW,
    )
    .unwrap();
    assert_eq!(index_rows(&conn), 1);
    posts::update_user_content(
        &conn,
        id,
        &UserContentPatch {
            note: Some(None),
            tags: None,
        },
        NOW,
    )
    .unwrap();
    assert_eq!(index_rows(&conn), 0);
}

#[test]
fn rebuild_matches_the_incremental_index() {
    let conn = library();
    insert_all(&conn, &synthetic_posts(400, 21));
    fixture_library(&conn);
    let queries = [
        "lampada",
        "design*",
        "studio",
        "photo*",
        "vetro OR glass",
        "author_1*",
        "kitchen",
    ];
    let before: Vec<Vec<String>> = queries.iter().map(|q| fts_hits(&conn, q)).collect();
    let rows_before = index_rows(&conn);

    let indexed = index::rebuild(&conn).unwrap();
    assert_eq!(
        indexed, 405,
        "every live post (the fixture has one in the trash)"
    );
    let after: Vec<Vec<String>> = queries.iter().map(|q| fts_hits(&conn, q)).collect();
    assert_eq!(after, before);
    assert_eq!(index_rows(&conn), rows_before);
    assert!(before.iter().all(|hits| !hits.is_empty()));
}
