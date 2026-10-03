//! Synthetic live counts, layered rewrites, alias/cluster integrity and rollback.
mod support;
use rusqlite::Connection;
use shelfy_core::repo::{Platform, posts};
use shelfy_core::tags::{aliases, explore, facets, health, merge};
fn post(c: &Connection, id: i64, ai: &[&str], manual: &[&str]) {
    let mut p = posts::NewPost::new(
        format!("ig_{id}"),
        Platform::Instagram,
        id.to_string(),
        "image",
        1,
    );
    p.posted_at = Some(id);
    p.ai = Some(posts::AiLayer {
        status: Some("done".into()),
        category: Some("design".into()),
        content_type: Some("photo".into()),
        language: Some("it".into()),
        tags: ai.iter().map(|s| (*s).into()).collect(),
        general_tags: Some(vec!["lamp".into()]),
        specific_tags: Some(vec!["lamps".into()]),
        entities: vec![if id == 1 { "Studio" } else { "studio" }.into()],
        ..Default::default()
    });
    p.user_tags = manual.iter().map(|s| (*s).into()).collect();
    posts::insert(c, &p, 1).unwrap();
}
fn rows(c: &Connection) -> Vec<(String, String, Option<String>)> {
    c.prepare("SELECT tag_norm,source,tier FROM post_tags WHERE post_id=1 ORDER BY source,tag_norm")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
#[test]
fn live_counts_deduplicate_layers_keep_tiers_and_resolve_forms() {
    let c = support::library();
    post(&c, 1, &["lamp", "lamps"], &["lamp", "desk"]);
    post(&c, 2, &["lamp"], &[]);
    post(&c, 3, &["trashunique"], &[]);
    c.execute("UPDATE posts SET deleted_at=1 WHERE id=3", [])
        .unwrap();
    let overview = explore::overview(&c).unwrap();
    assert_eq!(
        (
            overview.total,
            overview.analyzed,
            overview.unique_tags,
            overview.tagged_posts
        ),
        (2, 2, 3, 2)
    );
    let all = explore::stats(&c, explore::Tier::All, 500).unwrap();
    assert_eq!(
        (all[0].tag.as_str(), all[0].count, all[0].last_used),
        ("lamp", 2, Some(2))
    );
    assert_eq!(all[0].categories[0].count, 2);
    assert_eq!(
        explore::stats(&c, explore::Tier::General, 200).unwrap()[0].count,
        2
    );
    assert_eq!(
        explore::stats(&c, explore::Tier::Specific, 200).unwrap()[0].tag,
        "lamps"
    );
    assert_eq!(
        explore::stats(&c, explore::Tier::Manual, 200)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        explore::related(&c, "LAMP ", 12)
            .unwrap()
            .iter()
            .map(|t| t.count)
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    let e = explore::entities(&c, 60).unwrap();
    assert_eq!((e[0].entity.as_str(), e[0].count), ("Studio", 2));
    let h = health::health(&c).unwrap();
    assert_eq!((h.rare_tags, h.orphan_tags.len()), (3, 2));
    let f = facets::facets(&c).unwrap();
    assert_eq!(f.status[0].value, "done");
    assert_eq!(f.status[0].count, 2);
}
#[test]
fn merge_preserves_both_layers_stronger_tier_json_membership_and_indexes() {
    let mut c = support::library();
    post(&c, 1, &["lamp", "lamps"], &["lamps", "desk"]);
    post(&c, 2, &["lamps"], &[]);
    c.execute("UPDATE posts SET deleted_at=1 WHERE id=2", [])
        .unwrap();
    c.execute("INSERT INTO tag_cluster(id,label,status,run_id,created_at,updated_at) VALUES(1,'one','accepted',1,1,1),(2,'two','accepted',1,1,1)",[]).unwrap();
    c.execute(
        "INSERT INTO tag_cluster_membership(cluster_id,tag_norm) VALUES(1,'lamps'),(2,'lamp')",
        [],
    )
    .unwrap();
    let tx = c.transaction().unwrap();
    let m = merge::merge(&tx, &["LAMPS ".into()], "lamp", 10).unwrap();
    assert_eq!(m.updated, 2);
    tx.commit().unwrap();
    assert_eq!(
        rows(&c),
        vec![
            ("lamp".into(), "ai".into(), Some("specific".into())),
            ("desk".into(), "manual".into(), None),
            ("lamp".into(), "manual".into(), None)
        ]
    );
    let layer: (String, String) = c
        .query_row(
            "SELECT ai_tags_json,user_tags_json FROM posts WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(layer, ("[\"lamp\"]".into(), "[\"lamp\",\"desk\"]".into()));
    assert_eq!(
        c.query_row(
            "SELECT cluster_id FROM tag_cluster_membership WHERE tag_norm='lamp'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM tag_cluster_membership WHERE tag_norm='lamps'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM posts_fts WHERE posts_fts MATCH 'lamps'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM posts_fts WHERE posts_fts MATCH 'lamp'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let tx = c.transaction().unwrap();
    let before = tx.total_changes();
    assert_eq!(merge::rename(&tx, "lamp", "lamp", 11).unwrap().updated, 0);
    assert_eq!(tx.total_changes(), before);
}
#[test]
fn failure_rolls_back_json_rows_membership_and_indexes() {
    let mut c = support::library();
    post(&c, 1, &["lamps"], &["lamps"]);
    post(&c, 2, &["lamps"], &[]);
    let before = support::dump(&c, shelfy_core::schema::Kind::Library, "rollback");
    c.execute_batch("CREATE TEMP TRIGGER fail_second BEFORE UPDATE OF ai_tags_json ON posts WHEN NEW.id=2 BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    {
        let tx = c.transaction().unwrap();
        assert!(merge::rename(&tx, "lamps", "newunique", 10).is_err());
    }
    assert_eq!(
        support::dump(&c, shelfy_core::schema::Kind::Library, "rollback"),
        before
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM posts_fts WHERE posts_fts MATCH 'newunique'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
#[test]
fn accepted_alias_roots_are_rewritten_and_retargeted_with_keys() {
    let mut c = support::library();
    post(&c, 1, &["lamps"], &[]);
    let tx = c.transaction().unwrap();
    aliases::save_proposals(
        &tx,
        &[aliases::AliasPair {
            alias_norm: "lamps".into(),
            alias_form: "lamps".into(),
            canonical_norm: "lamp".into(),
            canonical_form: "Lamp".into(),
        }],
        1,
    )
    .unwrap();
    aliases::accept(&tx, "lamps").unwrap();
    tx.commit().unwrap();
    assert_eq!(
        explore::post_keys(
            &c,
            &["LAMPS".into(), "lamp".into()],
            explore::MatchMode::And
        )
        .unwrap()
        .keys,
        vec!["ig_1"]
    );
    let tx = c.transaction().unwrap();
    merge::rename(&tx, "lamp", "Lighting", 20).unwrap();
    tx.commit().unwrap();
    assert_eq!(rows(&c)[0].0, "lighting");
    assert_eq!(
        c.query_row(
            "SELECT canonical_norm FROM tag_alias WHERE alias_norm='lamps'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "lighting"
    );
    assert_eq!(
        explore::post_keys(&c, &["lamps".into()], explore::MatchMode::Or)
            .unwrap()
            .keys,
        vec!["ig_1"]
    );
}
#[test]
fn post_keys_are_bounded_and_health_distinguishes_missing_and_empty_analysis() {
    let c = support::library();
    c.execute_batch("WITH RECURSIVE n(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM n WHERE id<10001) INSERT INTO posts(id,key,platform,native_id,media_type,imported_at,sort_ts,updated_at) SELECT id,'ig_'||id,'instagram',id,'image',1,1,1 FROM n; INSERT INTO post_tags(post_id,tag_norm,tag_form,source) SELECT id,'bulk','bulk','ai' FROM posts;").unwrap();
    let result = explore::post_keys(&c, &["bulk".into()], explore::MatchMode::Or).unwrap();
    assert_eq!(result.keys.len(), 10000);
    assert!(result.truncated);
    c.execute("DELETE FROM post_tags WHERE post_id IN (1,2)", [])
        .unwrap();
    c.execute("UPDATE posts SET ai_status='done' WHERE id=1", [])
        .unwrap();
    let h = health::health(&c).unwrap();
    assert_eq!((h.unanalyzed_posts, h.untagged_posts), (10000, 1));
    assert_eq!(facets::facets(&c).unwrap().status[0].value, "none");
}
#[test]
fn suggestions_fold_accents_and_use_transitive_utf16_distance_groups() {
    let c = support::library();
    for (id, tag) in [
        (1, "café"),
        (2, "cafe"),
        (3, "aaaa"),
        (4, "aabb"),
        (5, "bbbb"),
        (6, "distantlong"),
    ] {
        post(&c, id, &[tag], &[]);
    }
    let groups = merge::suggestions(&c).unwrap();
    assert!(
        groups
            .iter()
            .any(|g| g.canonical == "aaaa" && g.variants == vec!["aabb", "bbbb"])
    );
    assert!(
        groups
            .iter()
            .any(|g| g.canonical == "cafe" && g.variants == vec!["café"])
    );
    assert!(
        !groups
            .iter()
            .any(|g| g.variants.iter().any(|t| t == "distantlong"))
    );
    let tx = c.unchecked_transaction().unwrap();
    assert!(merge::merge(&tx, &[], "x", 1).is_err());
    assert!(merge::rename(&tx, "cafe", " ", 1).is_err());
}
