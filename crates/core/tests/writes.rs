//! The write side of the library (P1-03): the manual AI edit and the
//! partial AI update behind it, the lookup of saved posts, collection order
//! and membership by selector, selections of posts, and the search index
//! after every kind of write.

mod support;

use rusqlite::{Connection, params};
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::posts::{
    self, AiLayer, AiPatch, MANUAL_AI_MODEL, PostFilter, UserContentPatch,
};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_core::search::index;
use shelfy_core::selector::{self, MAX_EXCEPT_KEYS, MAX_KEYS, Selector};
use support::{DAY, NOW, bare_post, fixture_library, insert_all, library, synthetic_posts};

/// The AI columns of a post, in a fixed order.
#[derive(Debug, PartialEq)]
struct AiColumns {
    status: Option<String>,
    model: Option<String>,
    description: Option<String>,
    tags: Option<String>,
    category: Option<String>,
    entities: Option<String>,
    keywords: Option<String>,
    save_reason: Option<String>,
    analyzed_at: Option<i64>,
}

fn ai_columns(conn: &Connection, id: i64) -> AiColumns {
    conn.query_row(
        "SELECT ai_status, ai_model, ai_description, ai_tags_json, ai_category,
                ai_entities_json, ai_keywords_json, ai_save_reason, ai_analyzed_at
         FROM posts WHERE id = ?1",
        [id],
        |r| {
            Ok(AiColumns {
                status: r.get(0)?,
                model: r.get(1)?,
                description: r.get(2)?,
                tags: r.get(3)?,
                category: r.get(4)?,
                entities: r.get(5)?,
                keywords: r.get(6)?,
                save_reason: r.get(7)?,
                analyzed_at: r.get(8)?,
            })
        },
    )
    .unwrap()
}

/// `(norm, form, source, tier)` of a post's tag rows.
fn tag_rows(conn: &Connection, id: i64) -> Vec<(String, String, String, Option<String>)> {
    conn.prepare(
        "SELECT tag_norm, tag_form, source, tier FROM post_tags WHERE post_id = ?1
         ORDER BY tag_norm, source",
    )
    .unwrap()
    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn entity_rows(conn: &Connection, id: i64) -> Vec<(String, String)> {
    conn.prepare(
        "SELECT ent_norm, ent_form FROM post_entities WHERE post_id = ?1 ORDER BY ent_norm",
    )
    .unwrap()
    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn updated_at(conn: &Connection, id: i64) -> i64 {
    conn.query_row("SELECT updated_at FROM posts WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .unwrap()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn assert_index_consistent(conn: &Connection, after: &str) {
    assert_eq!(
        index::verify(conn).unwrap(),
        Vec::<i64>::new(),
        "the index differs from the posts after {after}"
    );
}

#[test]
fn the_ai_patch_writes_only_the_fields_it_has() {
    let conn = library();
    let id = posts::insert(&conn, &bare_post("ig_1", Platform::Instagram, NOW), NOW).unwrap();
    let full = AiPatch {
        status: Some(Some("done".into())),
        model: Some(Some("model-a".into())),
        description: Some(Some("A lamp".into())),
        tags: Some(Some(strings(&["Lamp", "glass", " LAMP "]))),
        category: Some(Some("interior".into())),
        entities: Some(Some(strings(&["Murano"]))),
        keywords: Some(Some(strings(&["blown glass"]))),
        save_reason: Some(Some("ideas".into())),
        ..AiPatch::default()
    };
    assert!(posts::update_ai(&conn, id, &full, NOW + 1).unwrap());
    let columns = ai_columns(&conn, id);
    assert_eq!(columns.status.as_deref(), Some("done"));
    assert_eq!(
        columns.tags.as_deref(),
        Some(r#"["Lamp","glass"," LAMP "]"#)
    );
    assert_eq!(columns.analyzed_at, Some(NOW + 1), "done stamps the time");
    assert_eq!(updated_at(&conn, id), NOW + 1);
    assert_eq!(
        tag_rows(&conn, id),
        [
            ("glass".into(), "glass".into(), "ai".into(), None),
            ("lamp".into(), "Lamp".into(), "ai".into(), None),
        ]
    );
    assert_eq!(entity_rows(&conn, id), [("murano".into(), "Murano".into())]);

    // Only the description changes; the rest is untouched.
    let description = AiPatch {
        description: Some(Some("Another lamp".into())),
        ..AiPatch::default()
    };
    assert!(posts::update_ai(&conn, id, &description, NOW + 2).unwrap());
    let after = ai_columns(&conn, id);
    assert_eq!(after.description.as_deref(), Some("Another lamp"));
    assert_eq!(after.analyzed_at, Some(NOW + 1), "no status, no new stamp");
    assert_eq!(
        AiColumns {
            description: columns.description.clone(),
            ..after
        },
        columns
    );
    assert_eq!(tag_rows(&conn, id).len(), 2);

    // `null` writes NULL; an empty list stays an empty JSON array.
    let clear = AiPatch {
        status: Some(None),
        description: Some(None),
        tags: Some(Some(Vec::new())),
        entities: Some(None),
        ..AiPatch::default()
    };
    assert!(posts::update_ai(&conn, id, &clear, NOW + 3).unwrap());
    let cleared = ai_columns(&conn, id);
    assert_eq!(cleared.status, None);
    assert_eq!(cleared.description, None);
    assert_eq!(cleared.tags.as_deref(), Some("[]"));
    assert_eq!(cleared.entities, None);
    assert_eq!(cleared.keywords.as_deref(), Some(r#"["blown glass"]"#));
    assert!(tag_rows(&conn, id).is_empty());
    assert!(entity_rows(&conn, id).is_empty());

    // An explicit time wins over the stamp; an empty patch writes nothing.
    let explicit = AiPatch {
        status: Some(Some("done".into())),
        analyzed_at: Some(Some(42)),
        ..AiPatch::default()
    };
    posts::update_ai(&conn, id, &explicit, NOW + 4).unwrap();
    assert_eq!(ai_columns(&conn, id).analyzed_at, Some(42));
    assert!(!posts::update_ai(&conn, id, &AiPatch::default(), NOW + 5).unwrap());
    assert_eq!(updated_at(&conn, id), NOW + 4);
    assert!(matches!(
        posts::update_ai(&conn, 9999, &explicit, NOW),
        Err(RepoError::NotFound)
    ));
    assert_index_consistent(&conn, "AI patches");
}

#[test]
fn tiers_and_aliases_follow_the_desktop() {
    let conn = library();
    conn.execute(
        "INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status, created_at)
         VALUES ('lampade', 'lampada', 'Lampada', 'accepted', ?1),
                ('vetri', 'vetro', 'Vetro', 'proposed', ?1)",
        [NOW],
    )
    .unwrap();
    let id = posts::insert(&conn, &bare_post("ig_1", Platform::Instagram, NOW), NOW).unwrap();
    let patch = AiPatch {
        tags: Some(Some(strings(&["lampade", "Vetri", "Design", "lampada"]))),
        general_tags: Some(strings(&["design", "LAMPADE"])),
        specific_tags: Some(strings(&["lampada"])),
        ..AiPatch::default()
    };
    posts::update_ai(&conn, id, &patch, NOW).unwrap();
    assert_eq!(
        tag_rows(&conn, id),
        [
            (
                "design".into(),
                "Design".into(),
                "ai".into(),
                Some("general".into())
            ),
            // An accepted alias canonicalizes, and specific wins over general.
            (
                "lampada".into(),
                "Lampada".into(),
                "ai".into(),
                Some("specific".into())
            ),
            // A proposed alias does not; without a tier list entry, no tier.
            ("vetri".into(), "Vetri".into(), "ai".into(), None),
        ]
    );
}

#[test]
fn a_manual_edit_is_done_and_attributed_to_the_user() {
    let conn = library();
    let mut post = bare_post("ig_1", Platform::Instagram, NOW);
    post.ai = Some(AiLayer {
        status: Some("failed".into()),
        model: Some("model-a".into()),
        description: Some("Wrong".into()),
        tags: strings(&["wrong"]),
        category: Some("food".into()),
        ..AiLayer::default()
    });
    let id = posts::insert(&conn, &post, NOW).unwrap();
    let edit = AiPatch {
        description: Some(Some("A blown-glass lamp".into())),
        tags: Some(Some(strings(&["Lamp", "Glass"]))),
        save_reason: Some(Some("for the hall".into())),
        ..AiPatch::default()
    }
    .manual();
    posts::update_ai(&conn, id, &edit, NOW + DAY).unwrap();
    let columns = ai_columns(&conn, id);
    assert_eq!(columns.status.as_deref(), Some("done"));
    assert_eq!(columns.model.as_deref(), Some(MANUAL_AI_MODEL));
    assert_eq!(columns.analyzed_at, Some(NOW + DAY));
    assert_eq!(columns.category.as_deref(), Some("food"), "not in the edit");
    let detail = posts::get(&conn, "ig_1").unwrap().unwrap();
    assert_eq!(detail.summary.ai_tags, ["Lamp", "Glass"]);
    assert_eq!(
        detail.summary.ai_save_reason.as_deref(),
        Some("for the hall")
    );
    assert_index_consistent(&conn, "a manual edit");
    let hits = posts::list_ids(
        &conn,
        &PostFilter {
            q: Some("blown".into()),
            ..PostFilter::default()
        },
    )
    .unwrap();
    assert_eq!(hits, [id], "the new description is searchable");
}

/// Plan §1.2 #3: an AI tag and a manual tag with the same name are two rows;
/// clearing either layer keeps the other.
#[test]
fn an_ai_tag_and_a_manual_tag_of_one_name_coexist() {
    let conn = library();
    let id = posts::insert(&conn, &bare_post("ig_1", Platform::Instagram, NOW), NOW).unwrap();
    posts::update_user_content(
        &conn,
        id,
        &UserContentPatch {
            note: None,
            tags: Some(strings(&["Lamp"])),
        },
        NOW,
    )
    .unwrap();
    let edit = AiPatch {
        tags: Some(Some(strings(&["lamp", "glass"]))),
        ..AiPatch::default()
    }
    .manual();
    posts::update_ai(&conn, id, &edit, NOW).unwrap();
    assert_eq!(
        tag_rows(&conn, id),
        [
            ("glass".into(), "glass".into(), "ai".into(), None),
            ("lamp".into(), "lamp".into(), "ai".into(), None),
            ("lamp".into(), "Lamp".into(), "manual".into(), None),
        ]
    );
    let tagged = |tag: &str| {
        posts::count(
            &conn,
            &PostFilter {
                tag: Some(tag.into()),
                ..PostFilter::default()
            },
        )
        .unwrap()
    };
    let ai_tagged = || {
        posts::count(
            &conn,
            &PostFilter {
                ai_tagged: Some(true),
                ..PostFilter::default()
            },
        )
        .unwrap()
    };
    assert_eq!((tagged("lamp"), ai_tagged()), (1, 1));

    // Clearing the AI tags keeps the manual one, and the reverse.
    posts::update_ai(
        &conn,
        id,
        &AiPatch {
            tags: Some(None),
            ..AiPatch::default()
        },
        NOW,
    )
    .unwrap();
    assert_eq!(
        tag_rows(&conn, id),
        [("lamp".into(), "Lamp".into(), "manual".into(), None)]
    );
    assert_eq!((tagged("lamp"), ai_tagged()), (1, 0));
    posts::update_ai(&conn, id, &edit, NOW).unwrap();
    posts::update_user_content(
        &conn,
        id,
        &UserContentPatch {
            note: None,
            tags: Some(Vec::new()),
        },
        NOW,
    )
    .unwrap();
    assert_eq!(tag_rows(&conn, id).len(), 2);
    assert!(tag_rows(&conn, id).iter().all(|row| row.2 == "ai"));
    assert_eq!((tagged("lamp"), ai_tagged()), (1, 1));
    assert_index_consistent(&conn, "edits of both tag layers");
}

#[test]
fn lookup_finds_saved_posts_by_the_ids_pages_show() {
    let conn = library();
    fixture_library(&conn);
    // ig_3101 has the stored shortcode "C0ffeeAbCdE"; a second post is found
    // through the pk its shortcode decodes to.
    let pk = shelfy_core::ids::ig::MediaPk::from_shortcode("DAbcdEfgHiJ").unwrap();
    let mut decoded = bare_post(&format!("ig_{}", pk.as_str()), Platform::Instagram, NOW);
    decoded.native_id = pk.as_str().to_owned();
    let decoded_id = posts::insert(&conn, &decoded, NOW).unwrap();
    posts::trash(&conn, &[decoded_id], NOW).unwrap();

    let keys = strings(&[
        "missing",
        "C0ffeeAbCdE",
        "DAbcdEfgHiJ",
        "3101",
        &format!("{}_42", pk.as_str()),
        "C0ffeeAbCdE",
        "",
        "1800000000000000001",
    ]);
    let hits = posts::lookup(&conn, Platform::Instagram, &keys).unwrap();
    let found: Vec<(&str, &str, bool)> = hits
        .iter()
        .map(|h| (h.key.as_str(), h.post_key.as_str(), h.trashed))
        .collect();
    let decoded_key = format!("ig_{}", pk.as_str());
    let composite = format!("{}_42", pk.as_str());
    assert_eq!(
        found,
        [
            ("C0ffeeAbCdE", "ig_3101", false),
            ("DAbcdEfgHiJ", decoded_key.as_str(), true),
            ("3101", "ig_3101", false),
            (composite.as_str(), decoded_key.as_str(), true),
        ],
        "in request order, once per key; a tweet id is no Instagram key"
    );
    let tweets = posts::lookup(&conn, Platform::Twitter, &keys).unwrap();
    assert_eq!(tweets.len(), 1);
    assert_eq!(tweets[0].post_key, "x_1800000000000000001");
    assert!(
        posts::lookup(&conn, Platform::Pinterest, &keys)
            .unwrap()
            .is_empty()
    );
    assert!(
        posts::lookup(&conn, Platform::Instagram, &[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn keys_of_follows_the_ids() {
    let conn = library();
    let ids = insert_all(
        &conn,
        &[
            bare_post("ig_1", Platform::Instagram, NOW),
            bare_post("x_2", Platform::Twitter, NOW),
        ],
    );
    assert_eq!(
        posts::keys_of(&conn, &[ids[1], 777, ids[0]]).unwrap(),
        ["x_2", "ig_1"]
    );
    assert!(posts::keys_of(&conn, &[]).unwrap().is_empty());
}

fn named(conn: &Connection, name: &str) -> i64 {
    collections::create(
        conn,
        &NewCollection {
            name: name.into(),
            ..NewCollection::default()
        },
        NOW,
    )
    .unwrap()
    .id
}

fn order(conn: &Connection) -> Vec<(String, Option<i64>)> {
    collections::list(conn)
        .unwrap()
        .into_iter()
        .map(|c| (c.name, c.position))
        .collect()
}

#[test]
fn moving_a_collection_renumbers_the_manual_order() {
    let conn = library();
    let a = named(&conn, "a");
    let b = named(&conn, "b");
    let c = named(&conn, "c");
    assert_eq!(
        order(&conn),
        [("a".into(), None), ("b".into(), None), ("c".into(), None)]
    );
    let moved = collections::move_to(&conn, c, 0).unwrap();
    assert_eq!(moved.position, Some(0));
    assert_eq!(
        order(&conn),
        [
            ("c".into(), Some(0)),
            ("a".into(), Some(1)),
            ("b".into(), Some(2))
        ]
    );
    collections::move_to(&conn, c, 99).unwrap();
    assert_eq!(
        order(&conn),
        [
            ("a".into(), Some(0)),
            ("b".into(), Some(1)),
            ("c".into(), Some(2))
        ]
    );
    // A new collection comes last until it is moved; moving one where it
    // already is writes nothing.
    let d = named(&conn, "d");
    assert_eq!(order(&conn).last().unwrap(), &("d".into(), None));
    collections::move_to(&conn, b, 1).unwrap();
    let changes = conn.total_changes();
    collections::move_to(&conn, b, 1).unwrap();
    assert_eq!(conn.total_changes(), changes);
    collections::move_to(&conn, d, 1).unwrap();
    assert_eq!(
        order(&conn),
        [
            ("a".into(), Some(0)),
            ("d".into(), Some(1)),
            ("b".into(), Some(2)),
            ("c".into(), Some(3))
        ]
    );
    assert!(matches!(
        collections::move_to(&conn, 404, 0),
        Err(RepoError::NotFound)
    ));
    let _ = a;
}

#[test]
fn adding_by_selector_skips_trash_and_members() {
    let conn = library();
    let ids = insert_all(&conn, &synthetic_posts(300, 7));
    let folder = named(&conn, "folder");
    posts::trash(&conn, &ids[..10], NOW).unwrap();

    let instagram = PostFilter {
        platform: Some(Platform::Instagram),
        ..PostFilter::default()
    };
    let expected = posts::list_ids(&conn, &instagram).unwrap();
    let added = collections::add_selected(&conn, folder, &Selector::filter(instagram.clone()), NOW)
        .unwrap();
    let mut sorted_added = added.clone();
    sorted_added.sort_unstable();
    let mut sorted_expected = expected.clone();
    sorted_expected.sort_unstable();
    assert_eq!(sorted_added, sorted_expected);
    assert_eq!(
        collections::get(&conn, folder).unwrap().unwrap().count,
        expected.len() as u64
    );
    // Again: everything is already a member.
    assert!(
        collections::add_selected(&conn, folder, &Selector::filter(instagram), NOW)
            .unwrap()
            .is_empty()
    );
    // By keys: a trashed post and an unknown key are skipped.
    let trashed_key = posts::keys_of(&conn, &ids[..1]).unwrap().remove(0);
    let other = posts::list_ids(
        &conn,
        &PostFilter {
            platform: Some(Platform::Twitter),
            ..PostFilter::default()
        },
    )
    .unwrap()[0];
    let other_key = posts::keys_of(&conn, &[other]).unwrap().remove(0);
    let added = collections::add_selected(
        &conn,
        folder,
        &Selector::keys([trashed_key.as_str(), other_key.as_str(), "ig_missing"]),
        NOW,
    )
    .unwrap();
    assert_eq!(added, [other]);
    let members = collections::member_keys(&conn, folder, 1_000).unwrap();
    assert_eq!(members.len(), expected.len() + 1);
    assert_eq!(collections::member_keys(&conn, folder, 5).unwrap().len(), 5);
    assert!(matches!(
        collections::add_selected(&conn, 404, &Selector::keys(["x"]), NOW),
        Err(RepoError::NotFound)
    ));
    let over = Selector::Keys(vec!["ig_1".into(); MAX_KEYS + 1]);
    assert!(matches!(
        collections::add_selected(&conn, folder, &over, NOW),
        Err(RepoError::Invalid { field: "keys", .. })
    ));
    assert_index_consistent(&conn, "membership changes");
}

/// A selection by filter is exactly what the list returns with that filter:
/// both use the same condition.
#[test]
fn a_filter_selects_what_the_list_lists() {
    let conn = library();
    let ids = insert_all(&conn, &synthetic_posts(500, 11));
    fixture_library(&conn);
    posts::trash(&conn, &ids[..25], NOW).unwrap();
    let folder = named(&conn, "f");
    collections::add_posts(&conn, &ids[20..120], &[folder], NOW).unwrap();
    let filters = [
        PostFilter::default(),
        PostFilter {
            trash: true,
            ..PostFilter::default()
        },
        PostFilter {
            platform: Some(Platform::Twitter),
            media_types: strings(&["video", "images"]),
            ..PostFilter::default()
        },
        PostFilter {
            collection_id: Some(folder),
            ..PostFilter::default()
        },
        PostFilter {
            q: Some("lampada design".into()),
            ..PostFilter::default()
        },
        PostFilter {
            q: Some("photography".into()),
            concepts: strings(&["kitchen"]),
            concept_mode: posts::Mode::And,
            ..PostFilter::default()
        },
        PostFilter {
            tag: Some("Lighting".into()),
            ..PostFilter::default()
        },
        PostFilter {
            stored: Some(false),
            ai_tagged: Some(false),
            date_from: Some(NOW - 400 * DAY),
            ..PostFilter::default()
        },
    ];
    for filter in filters {
        let listed = posts::list_ids(&conn, &filter).unwrap();
        let selector = Selector::filter(filter.clone());
        assert_eq!(
            selector::ids(&conn, &selector).unwrap(),
            listed,
            "{filter:?}"
        );
        assert_eq!(
            selector::count(&conn, &selector).unwrap(),
            posts::count(&conn, &filter).unwrap(),
            "{filter:?}"
        );
        // Exceptions leave out exactly those posts.
        let except: Vec<i64> = listed.iter().copied().step_by(3).collect();
        let except_keys = posts::keys_of(&conn, &except).unwrap();
        let rest: Vec<i64> = listed
            .iter()
            .copied()
            .filter(|id| !except.contains(id))
            .collect();
        let selector = Selector::filter_except(filter.clone(), except_keys);
        assert_eq!(selector::ids(&conn, &selector).unwrap(), rest, "{filter:?}");
        assert_eq!(
            selector::count(&conn, &selector).unwrap(),
            rest.len() as u64
        );
    }
    // Keys select in or out of the trash, and skip unknown keys.
    let mut keys = posts::keys_of(&conn, &[ids[0], ids[30]]).unwrap();
    keys.push("ig_unknown".into());
    let by_keys = Selector::Keys(keys);
    let mut selected = selector::ids(&conn, &by_keys).unwrap();
    selected.sort_unstable();
    let mut expected = vec![ids[0], ids[30]];
    expected.sort_unstable();
    assert_eq!(selected, expected);
    assert_eq!(selector::count(&conn, &by_keys).unwrap(), 2);
    let over =
        Selector::filter_except(PostFilter::default(), vec!["x".into(); MAX_EXCEPT_KEYS + 1]);
    assert!(matches!(
        selector::count(&conn, &over),
        Err(RepoError::Invalid {
            field: "exceptKeys",
            ..
        })
    ));
}

#[test]
fn every_kind_of_write_leaves_the_index_consistent() {
    let conn = library();
    let ids = insert_all(&conn, &synthetic_posts(200, 3));
    fixture_library(&conn);
    assert_index_consistent(&conn, "inserts");
    let id = ids[5];
    posts::update_user_content(
        &conn,
        id,
        &UserContentPatch {
            note: Some(Some("brutalist concrete".into())),
            tags: None,
        },
        NOW,
    )
    .unwrap();
    assert_index_consistent(&conn, "a note");
    posts::update_user_content(
        &conn,
        id,
        &UserContentPatch {
            note: None,
            tags: Some(strings(&["Concrete", "stone"])),
        },
        NOW,
    )
    .unwrap();
    assert_index_consistent(&conn, "manual tags");
    let edit = AiPatch {
        description: Some(Some("A concrete house".into())),
        tags: Some(Some(strings(&["house"]))),
        entities: Some(Some(strings(&["Tadao Ando"]))),
        ..AiPatch::default()
    }
    .manual();
    posts::update_ai(&conn, id, &edit, NOW).unwrap();
    assert_index_consistent(&conn, "a manual AI edit");
    posts::clear_ai(&conn, id, NOW).unwrap();
    assert_index_consistent(&conn, "clearing the AI layer");
    let folder = named(&conn, "folder");
    collections::add_selected(
        &conn,
        folder,
        &Selector::keys(posts::keys_of(&conn, &ids[..50]).unwrap()),
        NOW,
    )
    .unwrap();
    collections::move_to(&conn, folder, 0).unwrap();
    collections::remove_post(&conn, ids[0], folder).unwrap();
    collections::delete(&conn, folder, collections::DeleteMode::KeepPosts, NOW).unwrap();
    assert_index_consistent(&conn, "folder writes");
    posts::trash(&conn, &ids[10..20], NOW).unwrap();
    assert_index_consistent(&conn, "trashing");
    posts::update_user_content(
        &conn,
        ids[10],
        &UserContentPatch {
            note: Some(Some("edited in the trash".into())),
            tags: None,
        },
        NOW,
    )
    .unwrap();
    assert_index_consistent(&conn, "an edit in the trash");
    posts::restore(&conn, &ids[10..15], NOW).unwrap();
    assert_index_consistent(&conn, "restoring");
    posts::purge(&conn, &ids[15..20], NOW).unwrap();
    assert_index_consistent(&conn, "purging");
}

#[test]
fn the_check_finds_a_stale_index() {
    let conn = library();
    let mut a = bare_post("ig_1", Platform::Instagram, NOW);
    a.caption = Some("red apple".into());
    let mut b = bare_post("ig_2", Platform::Instagram, NOW);
    b.caption = Some("blue sky".into());
    let ids = insert_all(&conn, &[a, b]);
    assert!(index::verify(&conn).unwrap().is_empty());

    // Text changed behind the index's back, and a row left for a trashed post.
    conn.execute(
        "UPDATE posts SET caption = 'green apple' WHERE id = ?1",
        [ids[0]],
    )
    .unwrap();
    conn.execute(
        "UPDATE posts SET deleted_at = ?2 WHERE id = ?1",
        params![ids[1], NOW],
    )
    .unwrap();
    assert_eq!(index::verify(&conn).unwrap(), ids);
    index::reindex_post(&conn, ids[0]).unwrap();
    index::remove_post(&conn, ids[1]).unwrap();
    assert!(index::verify(&conn).unwrap().is_empty());
    // The check leaves nothing behind.
    let temp: i64 = conn
        .query_row("SELECT count(*) FROM temp.sqlite_schema", [], |r| r.get(0))
        .unwrap();
    assert_eq!(temp, 0);
}
