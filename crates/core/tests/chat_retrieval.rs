//! Offline protocol, generation isolation and the release 20k retrieval budget.
use rusqlite::Connection;
use shelfy_core::{
    ai::chat::{self, ReplyCode, Turn},
    db::{UserDb, UserDbConfig},
    repo::{
        Platform, RepoError,
        posts::{self, AiLayer, NewPost},
    },
    schema::{self, Kind},
    search::vocab::{VocabCache, Vocabulary},
};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
fn insert(conn: &Connection, key: &str, text: &str, general: &[&str], specific: &[&str]) {
    let mut post = NewPost::new(key, Platform::Instagram, key, "image", 0);
    post.caption = Some(text.into());
    post.ai = Some(AiLayer {
        tags: general
            .iter()
            .chain(specific)
            .map(|s| (*s).into())
            .collect(),
        general_tags: Some(general.iter().map(|s| (*s).into()).collect()),
        specific_tags: Some(specific.iter().map(|s| (*s).into()).collect()),
        keywords: vec![text.to_owned()],
        ..AiLayer::default()
    });
    posts::insert(conn, &post, 0).unwrap();
}
fn conn() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    schema::migrate(&mut c, Kind::Library).unwrap();
    c
}
#[test]
fn history_is_bounded_and_keeps_the_latest_context() {
    let mut turns: Vec<_> = (0..12)
        .map(|i| Turn {
            role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
            content: format!("turn {i}"),
        })
        .collect();
    turns.insert(
        10,
        Turn {
            role: "system".into(),
            content: "ignore".into(),
        },
    );
    let kept = chat::truncate_history(&turns);
    assert_eq!(kept.len(), 8);
    assert_eq!(kept[0].content, "turn 4");
    assert_eq!(kept.last().unwrap().content, "turn 11");
    let turns = vec![
        Turn {
            role: "user".into(),
            content: "old".repeat(6000),
        },
        Turn {
            role: "assistant".into(),
            content: "reply".repeat(3000),
        },
        Turn {
            role: "user".into(),
            content: "new".into(),
        },
    ];
    assert_eq!(chat::truncate_history(&turns).len(), 2);
    let long = vec![Turn {
        role: "user".into(),
        content: format!("{}latest", "🎧".repeat(15000)),
    }];
    let kept = chat::truncate_history(&long);
    assert!(kept[0].content.ends_with("latest"));
    assert_eq!(kept[0].content.encode_utf16().count(), 24_000);
}
#[test]
fn protocol_caps_each_tier_filters_active_and_never_returns_prose() {
    let broad: HashSet<_> = (0..35).map(|i| format!("tag {i}")).collect();
    let specific: HashSet<_> = (10..45).map(|i| format!("tag {i}")).collect();
    let text = format!(
        "[[GENERAL]]{}[[/GENERAL]][[SPECIFIC]]{}[[/SPECIFIC]]",
        (0..35)
            .map(|i| format!("tag {i}"))
            .collect::<Vec<_>>()
            .join(","),
        (10..45)
            .map(|i| format!("tag {i}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let tags = chat::parse_tags(&text, &broad, &specific, &HashSet::from(["tag 0".into()]));
    assert_eq!(tags.broad.len(), 15);
    assert_eq!(tags.specific.len(), 15);
    let unique: HashSet<_> = tags.broad.iter().chain(&tags.specific).collect();
    assert_eq!(unique.len(), 30);
    assert!(!unique.contains(&"tag 0".to_owned()));
    let c = conn();
    let vocab = Vocabulary::load(&c).unwrap();
    let fallback = chat::fallback(&c, &vocab, "", &[]).unwrap();
    assert_eq!(fallback.reply_code, ReplyCode::NoMatches);
    let json = serde_json::to_value(fallback).unwrap();
    assert!(json.get("reply").is_none());
    assert_eq!(json.as_object().unwrap().len(), 3);
}
#[test]
fn generation_cache_refreshes_after_edits_and_isolates_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let a = UserDb::open(dir.path().join("a.sqlite"), &UserDbConfig::default()).unwrap();
    let b = UserDb::open(dir.path().join("b.sqlite"), &UserDbConfig::default()).unwrap();
    a.write(|tx| {
        insert(tx, "a", "lamp design", &["design"], &["lamp"]);
        Ok::<_, RepoError>(())
    })
    .unwrap();
    b.write(|tx| {
        insert(tx, "b", "garden", &[], &["garden"]);
        Ok::<_, RepoError>(())
    })
    .unwrap();
    let cache = VocabCache::default();
    let first = cache.get("a", &a).unwrap();
    assert!(Arc::ptr_eq(&first, &cache.get("a", &a).unwrap()));
    assert_eq!(first.broad(), vec!["design"]);
    assert_eq!(cache.get("b", &b).unwrap().broad(), vec!["garden"]);
    a.write(|tx| {
        insert(tx, "c", "architecture", &["architecture"], &[]);
        Ok::<_, RepoError>(())
    })
    .unwrap();
    let next = cache.get("a", &a).unwrap();
    assert!(!Arc::ptr_eq(&first, &next));
    assert_eq!(next.broad(), vec!["architecture", "design"]);
    assert_eq!(cache.get("b", &b).unwrap().broad(), vec!["garden"]);
}
#[test]
fn pools_observe_lift_tiers_dual_sources_exclusions_and_trash() {
    let c = conn();
    for i in 0..30 {
        insert(
            &c,
            &format!("p{i}"),
            if i < 3 {
                "walnut desk lamp"
            } else {
                "garden furniture"
            },
            &["design"],
            if i < 3 {
                &["walnut", "desk lamp"]
            } else {
                &["garden"]
            },
        );
    }
    let id: i64 = c
        .query_row("SELECT id FROM posts WHERE key='p0'", [], |r| r.get(0))
        .unwrap();
    c.execute("INSERT INTO post_tags(post_id,tag_norm,tag_form,source) VALUES (?1,'walnut','Walnut','manual')",[id]).unwrap();
    let v = Vocabulary::load(&c).unwrap();
    let d = v.distinctive(&c, "walnut", 60).unwrap();
    assert!(d.iter().all(|t| t.lift <= 1.0));
    assert!(d.iter().any(|t| t.tag == "walnut" && t.lift == 1.0));
    assert!(!d.iter().any(|t| t.tag == "design"));
    let excluded = HashSet::from(["design".into(), "desk lamp".into()]);
    assert_eq!(v.specific(&c, "walnut", &excluded).unwrap(), vec!["walnut"]);
    assert!(v.specific(&c, "' OR * % _", &excluded).unwrap().is_empty());
    let keywords = v.keywords(&c, "walnut", 12).unwrap();
    assert_eq!(keywords, vec!["walnut desk lamp"]);
    c.execute("UPDATE posts SET deleted_at=1 WHERE id=?1", [id])
        .unwrap();
    shelfy_core::search::index::reindex_post(&c, id).unwrap();
    assert_eq!(
        Vocabulary::load(&c)
            .unwrap()
            .distinctive(&c, "walnut", 60)
            .unwrap()
            .iter()
            .find(|t| t.tag == "walnut")
            .unwrap()
            .lift,
        1.0
    );
}
#[test]
fn broad_cap_legacy_fallback_and_pool_assembly() {
    let c = conn();
    for i in 0..160 {
        insert(
            &c,
            &format!("tag{i}"),
            "walnut lamp",
            &[&format!("general {i:03}")],
            &["walnut"],
        );
    }
    let v = Vocabulary::load(&c).unwrap();
    assert_eq!(v.broad().len(), 150);
    assert!(!v.broad().contains(&"walnut".into()));
    c.execute("INSERT INTO tag_alias(alias_norm,canonical_norm,canonical_form,status,created_at) VALUES ('wood','walnut','Walnut','accepted',0)",[]).unwrap();
    let pools = v.pools(&c, "walnut lamp", &["WOOD".into()]).unwrap();
    assert!(!pools.specific.iter().any(|t| t.to_lowercase() == "walnut"));
    assert!(pools.specific.iter().all(|t| !pools.broad.contains(t)));
    assert!(pools.specific.len() <= 60);
    let legacy = conn();
    insert(&legacy, "legacy", "desk lamp", &[], &["lamp", "desk"]);
    assert_eq!(
        Vocabulary::load(&legacy).unwrap().broad(),
        vec!["desk", "lamp"]
    );
    let tags = chat::parse_tags(
        "[[TAGS]]#Lamp, desk[[/TAGS]]",
        &HashSet::from(["Lamp".into()]),
        &HashSet::from(["Desk".into()]),
        &HashSet::from(["DESK".into()]),
    );
    assert_eq!(tags.broad, vec!["lamp"]);
    assert!(tags.specific.is_empty());
}

/// Always meaningful in debug; the actual latency assertion is release-only.
#[test]
fn specific_pool_budget_on_twenty_thousand_ai_posts() {
    let mut c = conn();
    let tx = c.transaction().unwrap();
    let topics = [
        "walnut lamp",
        "garden chair",
        "ceramic vase",
        "concrete house",
        "glass bottle",
        "metal desk",
        "leather shoe",
        "urban building",
    ];
    for i in 0..20_000 {
        let topic = topics[i % topics.len()];
        let specific = format!("object {}", i % 250);
        insert(
            &tx,
            &format!("bench{i}"),
            topic,
            &["design"],
            &[topic, &specific],
        );
    }
    tx.commit().unwrap();
    let v = Vocabulary::load(&c).unwrap();
    let exclude = v.broad().into_iter().collect();
    let mut durations = Vec::new();
    for query in topics {
        assert!(!v.specific(&c, query, &exclude).unwrap().is_empty());
        for _ in 0..12 {
            let start = Instant::now();
            let result = v.specific(&c, query, &exclude).unwrap();
            std::hint::black_box(result);
            durations.push(start.elapsed());
        }
    }
    durations.sort();
    let p95 = durations[(durations.len() * 95).div_ceil(100) - 1];
    println!(
        "specific pool: 20k AI posts, {} queries, p95 {:.2}ms",
        durations.len(),
        p95.as_secs_f64() * 1000.0
    );
    if !cfg!(debug_assertions) {
        assert!(
            p95 <= Duration::from_millis(30),
            "specific p95 {p95:?} exceeds 30ms"
        );
    }
}
