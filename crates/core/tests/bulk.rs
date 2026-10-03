//! Bulk actions by selector (P1-11): what each action changes, one stamp per
//! delete, the undo of a delete by that stamp, the restore round trip that
//! leaves both search indexes and the folders identical, and chunks that cut
//! a selection into parts that together equal the whole.

mod support;

use rusqlite::{Connection, params};
use shelfy_core::bulk::{self, Action, CHUNK, MAX_COLLECTIONS};
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::posts::{self, AiLayer, PostFilter, UserContentPatch};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_core::search::index;
use shelfy_core::selector::{self, Selector};
use support::{DAY, NOW, fixture_library, insert_all, library, synthetic_posts};

fn folder(conn: &Connection, name: &str) -> i64 {
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

fn keys(conn: &Connection, ids: &[i64]) -> Vec<String> {
    posts::keys_of(conn, ids).unwrap()
}

fn trashed(conn: &Connection) -> Vec<(i64, i64)> {
    conn.prepare("SELECT id, deleted_at FROM posts WHERE deleted_at IS NOT NULL ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn members(conn: &Connection, collection: i64) -> Vec<i64> {
    conn.prepare("SELECT post_id FROM post_collections WHERE collection_id = ?1 ORDER BY post_id")
        .unwrap()
        .query_map([collection], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn assert_index_consistent(conn: &Connection, after: &str) {
    assert_eq!(
        index::verify(conn).unwrap(),
        Vec::<i64>::new(),
        "the index differs from the posts after {after}"
    );
}

/// Every token of a search index: term, post, column and offset.
fn vocab(conn: &Connection, table: &str) -> Vec<(String, i64, String, i64)> {
    let vocab = format!("vocab_{table}");
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS temp.{vocab} USING fts5vocab(main, {table}, instance);"
    ))
    .unwrap();
    conn.prepare(&format!(
        "SELECT term, doc, col, offset FROM temp.{vocab} ORDER BY term, doc, col, offset"
    ))
    .unwrap()
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// A post's id, key, caption, note, AI description and trash stamp.
type PostRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// What a restore must bring back as it was: both search indexes token by
/// token, the folder memberships, the tag and entity rows, and the post
/// columns that carry text, with the trash stamp.
#[derive(Debug, PartialEq)]
struct Library {
    fts: Vec<(String, i64, String, i64)>,
    infix: Vec<(String, i64, String, i64)>,
    memberships: Vec<(i64, i64, i64)>,
    tags: Vec<(i64, String, String, String)>,
    entities: Vec<(i64, String)>,
    posts: Vec<PostRow>,
}

fn snapshot(conn: &Connection) -> Library {
    let rows = |sql: &str| conn.prepare(sql).unwrap();
    Library {
        fts: vocab(conn, "posts_fts"),
        infix: vocab(conn, "posts_infix"),
        memberships: rows(
            "SELECT post_id, collection_id, added_at FROM post_collections ORDER BY 1, 2",
        )
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap(),
        tags: rows("SELECT post_id, tag_norm, tag_form, source FROM post_tags ORDER BY 1, 2, 4")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap(),
        entities: rows("SELECT post_id, ent_norm FROM post_entities ORDER BY 1, 2")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap(),
        posts: rows(
            "SELECT id, key, caption, user_note, ai_description, deleted_at FROM posts ORDER BY id",
        )
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap(),
    }
}

/// 600 synthetic posts plus the fixture library, with notes, manual tags,
/// AI layers and folders spread over them.
fn rich_library() -> (Connection, Vec<i64>, [i64; 2]) {
    let conn = library();
    let ids = insert_all(&conn, &synthetic_posts(600, 21));
    fixture_library(&conn);
    let (a, b) = (folder(&conn, "a"), folder(&conn, "b"));
    collections::add_posts(&conn, &ids[..300], &[a], NOW).unwrap();
    collections::add_posts(&conn, &ids[200..400], &[b], NOW).unwrap();
    for &id in ids.iter().step_by(7) {
        posts::update_user_content(
            &conn,
            id,
            &UserContentPatch {
                note: Some(Some(format!("note {id} brutalist concrete"))),
                tags: Some(vec!["Concrete".into(), format!("tag{}", id % 5)]),
            },
            NOW,
        )
        .unwrap();
    }
    for &id in ids.iter().step_by(11) {
        posts::set_ai(
            &conn,
            id,
            &AiLayer {
                status: Some("done".into()),
                description: Some(format!("description of post {id}")),
                tags: vec!["lamp".into(), "Glass".into()],
                entities: vec!["Murano".into()],
                ..AiLayer::default()
            },
            NOW,
        )
        .unwrap();
    }
    (conn, ids, [a, b])
}

#[test]
fn a_delete_stamps_its_posts_once_and_its_stamp_undoes_it() {
    let (conn, ids, _) = rich_library();
    let before_trash = trashed(&conn);
    assert_eq!(before_trash.len(), 1, "the fixture's trashed post");

    // Delete the Instagram posts but a few, stamped NOW + 5.
    let instagram = PostFilter {
        platform: Some(Platform::Instagram),
        ..PostFilter::default()
    };
    let listed = posts::list_ids(&conn, &instagram).unwrap();
    let except = keys(&conn, &listed[..3]);
    let selection = Selector::filter_except(instagram.clone(), except.clone());
    let applied = bulk::apply(&conn, &selection, &Action::Delete, NOW + 5).unwrap();
    assert_eq!(applied.selected, listed.len() as u64 - 3);
    assert_eq!(applied.changed.len() as u64, applied.selected);
    let mut expected: Vec<i64> = listed[3..].to_vec();
    expected.sort_unstable();
    assert_eq!(applied.changed, expected);
    let stamps: Vec<i64> = trashed(&conn)
        .into_iter()
        .filter(|(id, _)| expected.contains(id))
        .map(|(_, at)| at)
        .collect();
    assert!(stamps.iter().all(|&at| at == NOW + 5), "one stamp");
    assert_index_consistent(&conn, "a bulk delete");

    // A second delete of the same selection changes nothing, and keeps the
    // stamp; deleting a trashed post by key does not restamp it.
    let again = bulk::apply(&conn, &selection, &Action::Delete, NOW + 9).unwrap();
    assert_eq!(again.selected, 0, "the filter hides the trash");
    assert!(again.changed.is_empty());
    let by_key = Selector::Keys(keys(&conn, &expected[..2]));
    let again = bulk::apply(&conn, &by_key, &Action::Delete, NOW + 9).unwrap();
    assert_eq!(again.selected, 2, "keys reach the trash");
    assert!(again.changed.is_empty());
    assert!(
        trashed(&conn)
            .iter()
            .filter(|(id, _)| expected.contains(id))
            .all(|&(_, at)| at == NOW + 5)
    );
    // The exceptions are untouched.
    let live = posts::list_ids(&conn, &instagram).unwrap();
    assert_eq!(keys(&conn, &live), except);

    // Another delete, then the undo of the first one by its stamp.
    let other = Selector::Keys(keys(&conn, &[ids[1], ids[2]]));
    bulk::apply(&conn, &other, &Action::Delete, NOW + 7).unwrap();
    let undo = Selector::TrashedAt(NOW + 5);
    assert_eq!(bulk::count(&conn, &undo).unwrap(), expected.len() as u64);
    let restored = bulk::apply(&conn, &undo, &Action::Restore, NOW + 8).unwrap();
    assert_eq!(restored.changed, expected);
    let left: Vec<i64> = trashed(&conn).into_iter().map(|(id, _)| id).collect();
    let mut still = vec![before_trash[0].0];
    still.extend(
        [ids[1], ids[2]]
            .into_iter()
            .filter(|id| !expected.contains(id)),
    );
    still.sort_unstable();
    assert_eq!(left, still, "only the first delete was undone");
    assert_index_consistent(&conn, "an undo");
    assert_eq!(bulk::count(&conn, &undo).unwrap(), 0);
}

/// A job's chunk stamps its posts with the request's stamp, the undo key,
/// and dates them by the chunk: when they really entered the trash (P1-11
/// review H1, M4).
#[test]
fn a_chunk_stamps_with_the_request_and_dates_with_its_own_time() {
    let (conn, ids, _) = rich_library();
    let chunk = Selector::Keys(keys(&conn, &ids[..20]));
    let applied = bulk::apply_stamped(&conn, &chunk, &Action::Delete, NOW + 5, NOW + 900).unwrap();
    assert_eq!(applied.changed, ids[..20]);
    let dated: Vec<(i64, i64)> = conn
        .prepare("SELECT deleted_at, updated_at FROM posts WHERE id <= ?1 ORDER BY id")
        .unwrap()
        .query_map([ids[19]], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(dated, vec![(NOW + 5, NOW + 900); 20]);
    // `apply` is the same with the stamp at the time of the change.
    let inline = Selector::Keys(keys(&conn, &ids[20..22]));
    bulk::apply(&conn, &inline, &Action::Delete, NOW + 7).unwrap();
    assert_eq!(
        bulk::count(&conn, &Selector::TrashedAt(NOW + 7)).unwrap(),
        2
    );
}

/// Trash then restore: both indexes, the folders, the tags and the post
/// columns come back exactly, whether the restore runs inline or in chunks.
#[test]
fn a_restore_round_trip_leaves_the_indexes_and_folders_identical() {
    let (conn, ids, [a, b]) = rich_library();
    let before = snapshot(&conn);
    assert!(!before.fts.is_empty() && !before.infix.is_empty());

    // Inline, by keys: posts in folders, with notes, tags and AI layers.
    let some = Selector::Keys(keys(&conn, &ids[190..410]));
    let applied = bulk::apply(&conn, &some, &Action::Delete, NOW + 1).unwrap();
    assert_eq!(applied.changed.len(), 220);
    let trashed_state = snapshot(&conn);
    assert!(
        trashed_state
            .fts
            .iter()
            .all(|(_, doc, _, _)| !applied.changed.contains(doc)),
        "trashed posts leave posts_fts"
    );
    assert!(
        trashed_state
            .infix
            .iter()
            .all(|(_, doc, _, _)| !applied.changed.contains(doc)),
        "and posts_infix"
    );
    assert_eq!(
        trashed_state.memberships, before.memberships,
        "memberships stay in the trash"
    );
    // The folders count only live posts meanwhile.
    let count = |id| collections::get(&conn, id).unwrap().unwrap().count;
    assert_eq!(count(a), 190);
    assert_eq!(count(b), 0);
    bulk::apply(&conn, &some, &Action::Restore, NOW + 2).unwrap();
    assert_eq!(snapshot(&conn), before);
    assert_eq!((count(a), count(b)), (300, 200));

    // In chunks, by filter: everything but the fixture's trashed post.
    let all = Selector::filter(PostFilter::default());
    bulk::apply(&conn, &all, &Action::Delete, NOW + 3).unwrap();
    assert!(vocab(&conn, "posts_fts").is_empty());
    assert!(vocab(&conn, "posts_infix").is_empty());
    let in_trash = Selector::TrashedAt(NOW + 3);
    let mut after = 0;
    let mut chunks = 0;
    while let Some(chunk) = bulk::next_chunk(&conn, &in_trash, after, 100).unwrap() {
        assert!(chunk.len <= 100);
        bulk::apply(&conn, &chunk.selector, &Action::Restore, NOW + 4).unwrap();
        after = chunk.last;
        chunks += 1;
    }
    assert_eq!(chunks, 7, "605 posts in chunks of 100");
    assert_eq!(snapshot(&conn), before);
    assert_index_consistent(&conn, "a chunked restore");
}

#[test]
fn chunks_cover_the_selection_once_in_id_order() {
    let conn = library();
    let ids = insert_all(&conn, &synthetic_posts(1_234, 5));
    posts::trash(&conn, &ids[..34], NOW).unwrap();
    let filter = PostFilter {
        platform: Some(Platform::Twitter),
        ..PostFilter::default()
    };
    let listed = posts::list_ids(&conn, &filter).unwrap();
    let except = keys(&conn, &listed[..10]);
    let selection = Selector::filter_except(filter, except);
    let mut expected = selector::ids(&conn, &selection).unwrap();
    expected.sort_unstable();

    let mut seen = Vec::new();
    let mut after = 0;
    let mut chunks = 0;
    while let Some(chunk) = bulk::next_chunk(&conn, &selection, after, 50).unwrap() {
        assert!(chunk.len <= 50);
        chunks += 1;
        let Selector::Keys(chunk_keys) = &chunk.selector else {
            panic!("a chunk is a list of keys");
        };
        assert_eq!(chunk_keys.len(), chunk.len);
        let mut chunk_ids = selector::ids(&conn, &chunk.selector).unwrap();
        chunk_ids.sort_unstable();
        assert_eq!(chunk_ids.last(), Some(&chunk.last));
        assert!(chunk_ids.iter().all(|&id| id > after));
        seen.extend(chunk_ids);
        after = chunk.last;
    }
    assert_eq!(seen, expected);
    assert_eq!(chunks, expected.len().div_ceil(50));
    // A chunk never holds more posts than a selector of keys may.
    assert!(
        bulk::next_chunk(&conn, &Selector::filter(PostFilter::default()), 0, 10_000)
            .unwrap()
            .is_some_and(|c| c.len == CHUNK)
    );
}

/// Running an action in chunks ends where running it inline ends.
#[test]
fn chunked_and_inline_runs_agree() {
    let actions = |a: i64| {
        [
            Action::AddToCollections(vec![a]),
            Action::ClearAiTags,
            Action::RemoveFromCollection(a),
            Action::ClearAiDescription,
            Action::Delete,
            Action::Restore,
        ]
    };
    let selection = |conn: &Connection| {
        let filter = PostFilter {
            media_types: vec!["video".into(), "carousel".into()],
            ..PostFilter::default()
        };
        let listed = posts::list_ids(conn, &filter).unwrap();
        Selector::filter_except(filter, keys(conn, &listed[..5]))
    };
    let (inline, _, [a, _]) = rich_library();
    let (chunked, _, [a2, _]) = rich_library();
    assert_eq!(a, a2);
    for (step, action) in actions(a).into_iter().enumerate() {
        let now = NOW + 10 + step as i64;
        // The filter hides the trash: the restore selects the delete's posts
        // (step 4) by their stamp.
        let selector = |conn: &Connection| {
            if action == Action::Restore {
                Selector::TrashedAt(NOW + 14)
            } else {
                selection(conn)
            }
        };
        let whole = bulk::apply(&inline, &selector(&inline), &action, now).unwrap();
        let selector = selector(&chunked);
        let mut changed = Vec::new();
        let mut after = 0;
        while let Some(chunk) = bulk::next_chunk(&chunked, &selector, after, 37).unwrap() {
            changed.extend(
                bulk::apply(&chunked, &chunk.selector, &action, now)
                    .unwrap()
                    .changed,
            );
            after = chunk.last;
        }
        changed.sort_unstable();
        assert_eq!(changed, whole.changed, "{action:?}");
        assert!(!changed.is_empty(), "{action:?} changed something");
        assert_eq!(snapshot(&chunked), snapshot(&inline), "{action:?}");
    }
}

#[test]
fn collections_gain_and_lose_the_selected_posts() {
    let (conn, ids, [a, b]) = rich_library();
    let c = folder(&conn, "c");
    posts::trash(&conn, &ids[500..510], NOW).unwrap();
    let selection = Selector::Keys(keys(&conn, &ids[250..510]));

    let added = bulk::apply(
        &conn,
        &selection,
        &Action::AddToCollections(vec![a, c]),
        NOW,
    )
    .unwrap();
    assert_eq!(added.selected, 260);
    // `a` already had 250..300; trashed posts are skipped.
    let mut expected: Vec<i64> = ids[250..500].to_vec();
    expected.sort_unstable();
    assert_eq!(added.changed, expected);
    let mut in_a: Vec<i64> = ids[..500].to_vec();
    in_a.sort_unstable();
    assert_eq!(members(&conn, a), in_a);
    assert_eq!(members(&conn, c).len(), 250);
    // Again: nothing new.
    let again = bulk::apply(
        &conn,
        &selection,
        &Action::AddToCollections(vec![a, c]),
        NOW,
    )
    .unwrap();
    assert!(again.changed.is_empty());

    // Removing takes out members only, trashed ones too.
    collections::add_posts(&conn, &ids[..5], &[b], NOW).unwrap();
    let in_b_before = members(&conn, b).len();
    let trashed_member = ids[505];
    conn.execute(
        "INSERT INTO post_collections (post_id, collection_id, added_at) VALUES (?1, ?2, ?3)",
        params![trashed_member, b, NOW],
    )
    .unwrap();
    let removed = bulk::apply(&conn, &selection, &Action::RemoveFromCollection(b), NOW).unwrap();
    let mut expected: Vec<i64> = ids[250..400].to_vec();
    expected.push(trashed_member);
    expected.sort_unstable();
    assert_eq!(removed.changed, expected);
    assert_eq!(members(&conn, b).len(), in_b_before - 150);

    // A missing collection fails before anything changes.
    for action in [
        Action::AddToCollections(vec![a, 404]),
        Action::RemoveFromCollection(404),
    ] {
        let before = snapshot(&conn);
        assert!(matches!(
            bulk::apply(&conn, &selection, &action, NOW),
            Err(RepoError::NotFound)
        ));
        assert_eq!(snapshot(&conn), before);
    }
    let too_many = Action::AddToCollections((1..=MAX_COLLECTIONS as i64 + 1).collect());
    assert!(matches!(
        bulk::apply(&conn, &selection, &too_many, NOW),
        Err(RepoError::Invalid {
            field: "collectionIds",
            ..
        })
    ));
    assert_index_consistent(&conn, "membership changes");
}

#[test]
fn clearing_ai_fields_follows_the_desktop() {
    let (conn, ids, _) = rich_library();
    let analyzed: Vec<i64> = ids.iter().copied().step_by(11).collect();
    let with_note = ids[0]; // also analyzed: 0 is a multiple of 7 and 11
    let selection = Selector::Keys(keys(&conn, &ids[..100]));
    let ai = |id: i64| -> (Option<String>, Option<String>, Option<String>) {
        conn.query_row(
            "SELECT ai_status, ai_description, ai_tags_json FROM posts WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    let tag_rows = |id: i64, source: &str| -> i64 {
        conn.query_row(
            "SELECT count(*) FROM post_tags WHERE post_id = ?1 AND source = ?2",
            params![id, source],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(tag_rows(with_note, "manual"), 2);
    assert_eq!(tag_rows(with_note, "ai"), 2);

    let cleared = bulk::apply(&conn, &selection, &Action::ClearAiDescription, NOW + 1).unwrap();
    let expected: Vec<i64> = analyzed
        .iter()
        .copied()
        .filter(|id| ids[..100].contains(id))
        .collect();
    assert_eq!(cleared.changed, expected);
    let (status, description, tags) = ai(with_note);
    assert_eq!((status, description), (None, None), "not analyzed again");
    assert!(tags.is_some(), "the AI tags stay");
    assert_eq!(tag_rows(with_note, "ai"), 2);

    // Clearing tags then: the AI tag rows go, the manual ones stay; the
    // status was already cleared, the tags were not.
    let cleared = bulk::apply(&conn, &selection, &Action::ClearAiTags, NOW + 2).unwrap();
    assert_eq!(cleared.changed, expected);
    assert_eq!(ai(with_note), (None, None, None));
    assert_eq!(tag_rows(with_note, "ai"), 0);
    assert_eq!(tag_rows(with_note, "manual"), 2);
    // Nothing left to clear.
    for action in [Action::ClearAiDescription, Action::ClearAiTags] {
        assert!(
            bulk::apply(&conn, &selection, &action, NOW + 3)
                .unwrap()
                .changed
                .is_empty()
        );
    }
    // Clearing reaches trashed posts given by key, as PATCH does.
    let in_trash = analyzed[analyzed.len() - 1];
    posts::trash(&conn, &[in_trash], NOW).unwrap();
    let by_key = Selector::Keys(keys(&conn, &[in_trash]));
    let cleared = bulk::apply(&conn, &by_key, &Action::ClearAiTags, NOW + 4).unwrap();
    assert_eq!(cleared.changed, [in_trash]);
    assert_index_consistent(&conn, "clearing AI fields");
}

#[test]
fn unknown_keys_and_empty_selections_change_nothing() {
    let (conn, _, [a, _]) = rich_library();
    let before = snapshot(&conn);
    let none = Selector::Keys(vec!["ig_missing".into(), "x_0".into()]);
    let empty = Selector::Keys(Vec::new());
    let nothing = Selector::filter(PostFilter {
        q: Some("zzzzzz".into()),
        ..PostFilter::default()
    });
    for selection in [&none, &empty, &nothing] {
        for action in [
            Action::Delete,
            Action::Restore,
            Action::AddToCollections(vec![a]),
            Action::RemoveFromCollection(a),
            Action::ClearAiDescription,
            Action::ClearAiTags,
        ] {
            let applied = bulk::apply(&conn, selection, &action, NOW).unwrap();
            assert_eq!(applied.selected, 0, "{selection:?} {action:?}");
            assert!(applied.changed.is_empty());
        }
        assert!(
            bulk::next_chunk(&conn, selection, 0, CHUNK)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(snapshot(&conn), before);
    // A stamp no delete used selects nothing.
    assert_eq!(
        bulk::count(&conn, &Selector::TrashedAt(NOW - DAY)).unwrap(),
        0
    );
}
