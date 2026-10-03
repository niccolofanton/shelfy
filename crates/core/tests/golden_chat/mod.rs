use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shelfy_core::{
    ai::chat,
    repo::{
        Platform,
        posts::{self, AiLayer, NewPost},
    },
    schema::{self, Kind},
    search::vocab::Vocabulary,
};
use std::collections::HashSet;
fn setup() -> (Connection, Vocabulary) {
    let mut conn = Connection::open_in_memory().unwrap();
    schema::migrate(&mut conn, Kind::Library).unwrap();
    let rows = [
        (
            "a",
            "Walnut desk lamp design",
            vec!["design"],
            vec!["desk lamp", "walnut"],
            vec!["walnut desk lamp"],
        ),
        (
            "b",
            "Glass desk lamp",
            vec!["design"],
            vec!["desk lamp", "glass"],
            vec!["glass desk lamp"],
        ),
        (
            "c",
            "Garden chair design",
            vec!["design"],
            vec!["chair", "garden"],
            vec!["garden chair"],
        ),
        (
            "d",
            "Città architecture",
            vec!["architecture"],
            vec!["città"],
            vec!["urban architecture"],
        ),
        ("e", "Empty caption", vec![], vec!["lamp shade"], vec![]),
    ];
    for (i, (key, caption, general, specific, keywords)) in rows.into_iter().enumerate() {
        let mut post = NewPost::new(key, Platform::Instagram, key, "image", i as i64);
        post.caption = Some(caption.into());
        post.posted_at = Some(i as i64);
        post.ai = Some(AiLayer {
            tags: general
                .iter()
                .chain(&specific)
                .map(|s| (*s).into())
                .collect(),
            general_tags: Some(general.into_iter().map(str::to_owned).collect()),
            specific_tags: Some(specific.into_iter().map(str::to_owned).collect()),
            keywords: keywords.into_iter().map(str::to_owned).collect(),
            ..AiLayer::default()
        });
        posts::insert(&conn, &post, i as i64).unwrap();
    }
    for (alias, norm, form, status) in [
        ("lamps", "desk lamp", "Desk Lamp", "accepted"),
        ("seats", "chair", "Chair", "proposed"),
    ] {
        conn.execute("INSERT INTO tag_alias(alias_norm,canonical_norm,canonical_form,status,created_at) VALUES (?1,?2,?3,?4,0)",params![alias,norm,form,status]).unwrap();
    }
    let vocab = Vocabulary::load(&conn).unwrap();
    (conn, vocab)
}
#[test]
fn tags() {
    super::check(
        "ai/chat/parseTagBlock",
        |(text, open, close, vocab): (Value, String, String, HashSet<String>)| {
            chat::parse_tag_block(text.as_str().unwrap_or(""), &open, &close, &vocab)
        },
    );
}
#[test]
fn keywords() {
    super::check(
        "ai/chat/parseKeywordBlock",
        |(text, open, close): (Value, String, String)| {
            chat::parse_keyword_block(text.as_str().unwrap_or(""), &open, &close)
        },
    );
}
#[test]
fn deterministic_keywords() {
    super::check("ai/chat/deterministicKeywords", |(text,): (String,)| {
        chat::deterministic_keywords(&text)
    });
}
#[test]
fn intersect() {
    super::check(
        "ai/chat/intersectWithVocab",
        |(candidates,): (Vec<String>,)| {
            let (conn, vocab) = setup();
            vocab.intersect(&conn, &candidates).unwrap()
        },
    );
}
#[test]
fn expand() {
    super::check(
        "ai/chat/expandQueryToVocab",
        |(text, limit): (String, usize)| {
            let (conn, vocab) = setup();
            vocab.expand(&conn, &text, limit).unwrap()
        },
    );
}
#[test]
fn deterministic_tags() {
    super::check(
        "ai/chat/deterministicTagMatches",
        |(text, active, broad): (String, Vec<String>, Vec<String>)| {
            let (conn, vocab) = setup();
            let result =
                chat::deterministic_tag_matches(&conn, &vocab, &text, &active, &broad).unwrap();
            json!({"broad":result.broad,"specific":result.specific})
        },
    );
}
