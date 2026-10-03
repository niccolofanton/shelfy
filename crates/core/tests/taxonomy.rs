//! Taxonomy state transitions with synthetic data, both layers and live/trash.
mod support;
use rusqlite::{Connection, params};
use serde_json::json;
use shelfy_core::repo::{RepoError, posts};
use shelfy_core::schema::{self, Kind};
use shelfy_core::tags::{Status, aliases, clusters, graph};
use support::library;
fn post(conn: &Connection, id: i64) {
    conn.execute("INSERT INTO posts (id,key,platform,native_id,media_type,imported_at,sort_ts,updated_at) VALUES (?1,?2,'instagram',?2,'image',1,1,1)",params![id,format!("ig_{id}")]).unwrap();
}
fn pair(a: &str, c: &str) -> aliases::AliasPair {
    aliases::AliasPair {
        alias_norm: a.into(),
        alias_form: a.into(),
        canonical_norm: c.into(),
        canonical_form: c.into(),
    }
}
fn group(label: &str, tags: &[&str]) -> clusters::RefinedGroup {
    clusters::RefinedGroup {
        label: label.into(),
        tags: tags.iter().map(|s| (*s).into()).collect(),
    }
}
fn norms(conn: &Connection, id: i64) -> Vec<(String, String, Option<String>)> {
    conn.prepare(
        "SELECT tag_norm,source,tier FROM post_tags WHERE post_id=?1 ORDER BY source,tag_norm",
    )
    .unwrap()
    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}
#[test]
fn acceptance_merges_layers_keeps_json_reindexes_and_canonicalizes_later_writes() {
    let mut c = library();
    post(&c, 1);
    post(&c, 2);
    posts::update_ai(
        &c,
        1,
        &posts::AiPatch {
            tags: Some(Some(vec!["lamps".into(), "canonicalunique".into()])),
            general_tags: Some(vec!["canonicalunique".into()]),
            specific_tags: Some(vec!["lamps".into()]),
            ..Default::default()
        },
        1,
    )
    .unwrap();
    posts::update_user_content(
        &c,
        1,
        &posts::UserContentPatch {
            tags: Some(vec!["lamps".into(), "canonicalunique".into()]),
            ..Default::default()
        },
        1,
    )
    .unwrap();
    posts::update_ai(
        &c,
        2,
        &posts::AiPatch {
            tags: Some(Some(vec!["lamps".into()])),
            ..Default::default()
        },
        1,
    )
    .unwrap();
    let before: String = c
        .query_row("SELECT ai_tags_json FROM posts WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let tx = c.transaction().unwrap();
    assert_eq!(
        aliases::save_proposals(&tx, &[pair("lamps", "canonicalunique")], 1).unwrap(),
        1
    );
    assert!(norms(&tx, 1).iter().any(|r| r.0 == "lamps"));
    let accepted = aliases::accept(&tx, "LAMPS ").unwrap();
    assert_eq!(accepted.accepted, 1);
    assert_eq!(accepted.post_ids, vec![1, 2]);
    tx.commit().unwrap();
    assert_eq!(
        norms(&c, 1),
        vec![
            (
                "canonicalunique".into(),
                "ai".into(),
                Some("specific".into())
            ),
            ("canonicalunique".into(), "manual".into(), None)
        ]
    );
    assert_eq!(
        c.query_row("SELECT ai_tags_json FROM posts WHERE id=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM posts_fts WHERE posts_fts MATCH 'canonicalunique'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    posts::update_user_content(
        &c,
        2,
        &posts::UserContentPatch {
            tags: Some(vec!["lamps".into()]),
            ..Default::default()
        },
        2,
    )
    .unwrap();
    posts::update_ai(
        &c,
        2,
        &posts::AiPatch {
            tags: Some(Some(vec!["lamps".into()])),
            specific_tags: Some(vec!["lamps".into()]),
            ..Default::default()
        },
        2,
    )
    .unwrap();
    assert!(norms(&c, 2).iter().all(|r| r.0 == "canonicalunique"));
    assert_eq!(aliases::canonical_vocab(&c, 300).unwrap()[0].count, 2);
}
#[test]
fn chains_flatten_cycle_rolls_back_and_stale_proposals_cannot_change_accepted_decisions() {
    let mut c = library();
    post(&c, 1);
    posts::update_user_content(
        &c,
        1,
        &posts::UserContentPatch {
            tags: Some(vec!["a".into(), "b".into()]),
            ..Default::default()
        },
        1,
    )
    .unwrap();
    let tx = c.transaction().unwrap();
    aliases::save_proposals(&tx, &[pair("a", "b"), pair("b", "root")], 1).unwrap();
    let r = aliases::accept_all(&tx).unwrap();
    assert_eq!(r.accepted, 2);
    assert_eq!(
        aliases::save_proposals(&tx, &[pair("a", "wrong")], 2).unwrap(),
        0
    );
    assert!(matches!(
        aliases::dismiss(&tx, "a"),
        Err(RepoError::Conflict(_))
    ));
    assert_eq!(aliases::accept(&tx, "a").unwrap().accepted, 0);
    assert!(
        aliases::list(&tx, Some(Status::Accepted))
            .unwrap()
            .iter()
            .all(|a| a.canonical_norm == "root")
    );
    tx.commit().unwrap();
    assert_eq!(norms(&c, 1), vec![("root".into(), "manual".into(), None)]);
    {
        let tx = c.transaction().unwrap();
        aliases::save_proposals(&tx, &[pair("x", "y"), pair("y", "x")], 3).unwrap();
        assert!(matches!(
            aliases::accept_all(&tx),
            Err(RepoError::Conflict(_))
        ));
        // Caller dropping failed library write rolls back its earlier accept too.
    }
    assert_eq!(aliases::list(&c, None).unwrap().len(), 2);
}
#[test]
fn aliases_are_library_scoped_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut c = Connection::open(&path).unwrap();
    schema::migrate(&mut c, Kind::Library).unwrap();
    let tx = c.transaction().unwrap();
    aliases::save_proposals(&tx, &[pair("lamps", "lighting")], 1).unwrap();
    aliases::accept_all(&tx).unwrap();
    tx.commit().unwrap();
    drop(c);
    let c = Connection::open(&path).unwrap();
    post(&c, 1);
    posts::update_user_content(
        &c,
        1,
        &posts::UserContentPatch {
            tags: Some(vec!["lamps".into()]),
            ..Default::default()
        },
        2,
    )
    .unwrap();
    let other = library();
    post(&other, 1);
    posts::update_user_content(
        &other,
        1,
        &posts::UserContentPatch {
            tags: Some(vec!["lamps".into()]),
            ..Default::default()
        },
        2,
    )
    .unwrap();
    assert_eq!(norms(&c, 1)[0].0, "lighting");
    assert_eq!(norms(&other, 1)[0].0, "lamps");
}
#[test]
fn cluster_runs_preserve_accepted_memberships_skip_overlap_and_never_recycle_review_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut c = Connection::open(&path).unwrap();
    c.pragma_update(None, "foreign_keys", "ON").unwrap();
    schema::migrate(&mut c, Kind::Library).unwrap();
    let tx = c.transaction().unwrap();
    clusters::save_run(
        &tx,
        &[group("Kept", &["a", "b"]), group("Old", &["x", "y"])],
        1,
        1,
    )
    .unwrap();
    clusters::review(&tx, 1, true, None, 1).unwrap();
    let saved = clusters::save_run(
        &tx,
        &[
            group("New", &["a", "b", "c", "d"]),
            group("Overlap", &["c", "d", "e"]),
            group("Other", &["f", "g"]),
        ],
        2,
        2,
    )
    .unwrap();
    assert_eq!(saved.count, 2);
    assert!(matches!(
        clusters::review(&tx, 2, true, None, 2),
        Err(RepoError::NotFound)
    ));
    let rows = clusters::list(&tx, 24).unwrap();
    assert_eq!(
        rows.iter().find(|r| r.label == "Kept").unwrap().tags,
        vec!["a", "b"]
    );
    assert!(rows.iter().all(|c| c.tags.len() >= 2));
    for row in &rows {
        clusters::dismiss(&tx, row.id).unwrap();
    }
    tx.commit().unwrap();
    drop(c);
    let mut c = Connection::open(&path).unwrap();
    let tx = c.transaction().unwrap();
    clusters::save_run(&tx, &[group("Later", &["h", "i"])], 3, 3).unwrap();
    assert!(clusters::list(&tx, 24).unwrap()[0].id > rows.iter().map(|r| r.id).max().unwrap());
}
#[test]
fn graph_deduplicates_layers_excludes_trash_and_attaches_leftovers() {
    let mut c = library();
    for id in 1..=3 {
        post(&c, id);
        let tags = if id == 3 {
            vec!["trash".into(), "a".into()]
        } else {
            vec!["a".into(), "b".into()]
        };
        posts::update_ai(
            &c,
            id,
            &posts::AiPatch {
                tags: Some(Some(tags.clone())),
                ..Default::default()
            },
            1,
        )
        .unwrap();
        posts::update_user_content(
            &c,
            id,
            &posts::UserContentPatch {
                tags: Some(tags),
                ..Default::default()
            },
            1,
        )
        .unwrap();
    }
    c.execute("UPDATE posts SET deleted_at=2 WHERE id=3", [])
        .unwrap();
    let groups = graph::candidate_groups(&c, None, graph::Options::default()).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].tags, vec!["a", "b"]);
    let freq = [("a".into(), 10), ("b".into(), 10), ("tail".into(), 20)]
        .into_iter()
        .collect();
    let edges = [
        graph::Edge {
            a: "a".into(),
            b: "b".into(),
            c: 10,
        },
        graph::Edge {
            a: "a".into(),
            b: "tail".into(),
            c: 2,
        },
    ];
    let groups = graph::candidate_groups_from_graph(&freq, &edges, None, graph::Options::default());
    assert_eq!(groups[0].tags, vec!["a", "b", "tail"]);
    let tx = c.transaction().unwrap();
    assert!(
        graph::candidate_groups(
            &tx,
            None,
            graph::Options {
                max_group_size: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
}
#[test]
fn dismiss_proposal_leaves_posts_and_candidate_limits_are_enforced() {
    let mut c = library();
    post(&c, 1);
    let tx = c.transaction().unwrap();
    for n in 0..450 {
        let name = format!("tag-{n:03}");
        tx.execute(
            "INSERT INTO post_tags (post_id,tag_norm,tag_form,source) VALUES (1,?1,?1,'manual')",
            [name],
        )
        .unwrap();
    }
    assert_eq!(aliases::unaliased_tags(&tx, 1000).unwrap().len(), 400);
    assert_eq!(aliases::canonical_vocab(&tx, 1000).unwrap().len(), 300);
    aliases::save_proposals(&tx, &[pair("tag-000", "tag-001")], 1).unwrap();
    let before = norms(&tx, 1);
    aliases::dismiss(&tx, "tag-000").unwrap();
    assert_eq!(norms(&tx, 1), before);
    assert!(
        clusters::validate_refined_groups(&["a".into(), "b".into()], &json!({"groups":[]}))
            .is_empty()
    );
}
