//! Suggestion-specific invariants; synthetic SQLite only.
use rusqlite::{Connection, params};
use shelfy_core::{
    ai::suggest,
    repo::{
        Platform,
        posts::{self, AiLayer, NewPost, SourceBucket},
    },
    schema::{self, Kind},
};
fn setup() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    schema::migrate(&mut c, Kind::Library).unwrap();
    for (key, platform, tags) in [
        (
            "social",
            Platform::Instagram,
            vec![
                "desk lamp",
                "glass",
                "design",
                "wood",
                "lighting",
                "brass",
                "interior",
                "minimal",
                "music",
            ],
        ),
        ("site", Platform::Web, vec!["website", "typography"]),
        ("trash", Platform::Instagram, vec!["trashed"]),
    ] {
        let mut p = NewPost::new(key, platform, key, "image", 0);
        p.ai = Some(AiLayer {
            tags: tags.into_iter().map(str::to_owned).collect(),
            ..AiLayer::default()
        });
        posts::insert(&c, &p, 0).unwrap();
    }
    c.execute("UPDATE posts SET deleted_at=1 WHERE key='trash'", [])
        .unwrap();
    c.execute("INSERT INTO tag_alias(alias_norm,canonical_norm,canonical_form,status,created_at) VALUES('lamps','desk lamp','Desk Lamp','accepted',0)", []).unwrap();
    c
}
#[test]
fn exact_alias_fuzzy_scope_and_trash_never_produce_unknown_tags() {
    let c = setup();
    let tags = vec![
        "glass",
        "lamps",
        "DESK LAMP",
        "lamp",
        "website",
        "trashed",
        "fabricated",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert_eq!(
        suggest::intersect(&c, Some(SourceBucket::Social), &tags).unwrap(),
        vec!["glass", "Desk Lamp"]
    );
    assert_eq!(
        suggest::intersect(&c, Some(SourceBucket::Web), &tags).unwrap(),
        vec!["website"]
    );
    let many = vec![
        "desk lamp",
        "glass",
        "design",
        "wood",
        "lighting",
        "brass",
        "interior",
        "minimal",
        "music",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert_eq!(suggest::intersect(&c, None, &many).unwrap().len(), 8);
    c.execute("UPDATE posts SET deleted_at=2", []).unwrap();
    assert!(suggest::intersect(&c, None, &many).unwrap().is_empty());
}
#[test]
fn cache_generation_ignores_cache_writes_but_tracks_scope_aliases_and_live_vocabulary() {
    let c = setup();
    let key = suggest::cache_key(&c, " desk   LAMP ", None).unwrap();
    assert_eq!(key, suggest::cache_key(&c, "DESK LAMP", None).unwrap());
    assert_ne!(
        key,
        suggest::cache_key(&c, "desk lamp", Some(SourceBucket::Web)).unwrap()
    );
    c.execute(
        "INSERT INTO ai_cache(kind,key_hash,value_json,created_at) VALUES('suggest',?1,'[]',0)",
        params![key],
    )
    .unwrap();
    assert_eq!(key, suggest::cache_key(&c, "desk lamp", None).unwrap());
    c.execute("UPDATE tag_alias SET status='proposed'", [])
        .unwrap();
    let alias_changed = suggest::cache_key(&c, "desk lamp", None).unwrap();
    assert_ne!(key, alias_changed);
    c.execute("UPDATE posts SET deleted_at=2 WHERE key='social'", [])
        .unwrap();
    assert_ne!(
        alias_changed,
        suggest::cache_key(&c, "desk lamp", None).unwrap()
    );
}
#[test]
fn malformed_suggest_output_is_rejected_without_reinterpreting_free_text() {
    assert_eq!(
        suggest::parse(r#"{"tags":["lamp","glass"]}"#),
        Some(vec!["lamp".into(), "glass".into()])
    );
    for bad in [
        "lamp, glass",
        r#"{"tags":[null]}"#,
        r#"{"tags":"lamp"}"#,
        r#"{"tags":[],"reply":"text"}"#,
    ] {
        assert!(suggest::parse(bad).is_none(), "{bad}");
    }
}
