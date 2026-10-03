//! The capture ingest pipeline as `POST /ingest/batches` runs it (plan §2.16,
//! P2 contract C5): the batch is sanitized, then merged with `upsert_batch`,
//! and the archive state of every post it inserted or changed is derived
//! again, in one `UserDb::write`. The answer maps the merge's summary:
//! inserted → `inserted`, changed → `updated`, merged → `known`.

use rusqlite::Connection;
use serde_json::{Value, json};
use shelfy_core::db::{UserDb, UserDbConfig};
use shelfy_core::ingest::archive::{
    ArchiveModes, ArchivePolicy, ArchiveState, Refreshed, Scope, refresh_states,
};
use shelfy_core::ingest::merge::{UpsertOptions, UpsertSummary, upsert_batch};
use shelfy_core::ingest::sanitize::{RejectCode, Rejected, sanitize_batch};
use shelfy_core::repo::{Platform, RepoError};

/// 2026-10-02T00:00:00Z.
const NOW: i64 = 1_790_899_200_000;

/// What the route answers, per C5.
#[derive(Debug, PartialEq, Eq)]
struct Answer {
    inserted: usize,
    updated: usize,
    known: usize,
    /// `(index, key, inserted, changed)` per accepted item.
    results: Vec<(usize, String, bool, bool)>,
    rejected: Vec<Rejected>,
}

fn ingest(db: &UserDb, platform: Platform, items: &[Value], now: i64) -> (Answer, Refreshed) {
    let batch = sanitize_batch(platform, items, now).unwrap();
    let (summary, refreshed): (UpsertSummary, Refreshed) = db
        .write(|tx| {
            let summary = upsert_batch(tx, &batch.posts, UpsertOptions::default(), now)?;
            let touched: Vec<i64> = summary
                .posts
                .iter()
                .filter(|p| p.inserted || p.changed)
                .map(|p| p.id)
                .collect();
            let policy = ArchivePolicy::read(tx, ArchiveModes::default())?;
            let refreshed = refresh_states(tx, Scope::Posts(&touched), &policy, now)?;
            Ok::<_, RepoError>((summary, refreshed))
        })
        .unwrap();
    let results = batch
        .indices
        .iter()
        .zip(&batch.posts)
        .zip(&summary.posts)
        .map(|((index, post), done)| (*index, post.key.clone(), done.inserted, done.changed))
        .collect();
    let answer = Answer {
        inserted: summary.inserted,
        updated: summary.changed,
        known: summary.merged,
        results,
        rejected: batch.rejected,
    };
    (answer, refreshed)
}

fn states(db: &UserDb) -> Vec<String> {
    db.read(|c: &Connection| {
        c.prepare("SELECT key || '=' || archive_state FROM posts ORDER BY key")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
            .map_err(shelfy_core::db::DbError::from)
    })
    .unwrap()
}

fn page(caption: &str) -> Vec<Value> {
    let valid = format!("oe={:X}", (NOW + 86_400_000) / 1_000);
    let expired = format!("oe={:X}", (NOW - 1_000) / 1_000);
    vec![
        json!({
            "id": "3191575067010950169_25025320",
            "shortcode": "CxKwJ0fLmQZ",
            "postUrl": "https://www.instagram.com/p/CxKwJ0fLmQZ/",
            "text": caption,
            "timestamp": "2023-09-14T09:56:58.000Z",
            "thumbnailUrl": format!("https://scontent.cdninstagram.com/v/1.jpg?{valid}"),
            "mediaType": "carousel",
            "media": [
                {"type": "image", "url": format!("https://scontent.cdninstagram.com/v/1.jpg?{valid}")},
                {"type": "image", "url": format!("https://scontent.cdninstagram.com/v/2.jpg?{valid}")},
            ],
        }),
        json!({"id": "not an id"}),
        json!({
            "id": "3191575067010950170",
            "thumbnailUrl": format!("https://scontent.cdninstagram.com/v/3.jpg?{expired}"),
            "mediaType": "image",
        }),
    ]
}

#[test]
fn a_batch_is_sanitized_merged_and_given_its_archive_states() {
    let dir = tempfile::tempdir().unwrap();
    let db = UserDb::open(dir.path().join("library.sqlite"), &UserDbConfig::default()).unwrap();

    let (answer, refreshed) = ingest(&db, Platform::Instagram, &page("A lamp"), NOW);
    assert_eq!(
        answer,
        Answer {
            inserted: 2,
            updated: 0,
            known: 0,
            results: vec![
                (0, "ig_3191575067010950169".to_owned(), true, true),
                (2, "ig_3191575067010950170".to_owned(), true, true),
            ],
            rejected: vec![Rejected {
                index: 1,
                code: RejectCode::BadId
            }],
        }
    );
    // The carousel waits for the server; the expired cover for the extension.
    assert_eq!(
        states(&db),
        [
            "ig_3191575067010950169=pending",
            "ig_3191575067010950170=client"
        ]
    );
    assert_eq!(refreshed.counts.server_work(), 1, "the drain has work");
    assert_eq!(refreshed.counts.get(ArchiveState::Client), 1);

    // The same batch again: every post is known, nothing changes, nothing is
    // written (the library's generation stays).
    let before = db.generation();
    let (answer, refreshed) = ingest(&db, Platform::Instagram, &page("A lamp"), NOW + 1_000);
    assert_eq!((answer.inserted, answer.updated, answer.known), (0, 0, 2));
    assert!(
        answer
            .results
            .iter()
            .all(|(_, _, inserted, changed)| !inserted && !changed)
    );
    assert_eq!(refreshed, Refreshed::default());
    assert_eq!(db.generation(), before);

    // A new caption changes one post.
    let (answer, refreshed) = ingest(&db, Platform::Instagram, &page("A glass lamp"), NOW + 2_000);
    assert_eq!((answer.inserted, answer.updated, answer.known), (0, 1, 2));
    assert_eq!(refreshed.changed, 0, "its state stays pending");
    assert_eq!(refreshed.counts.server_work(), 1);
    assert_ne!(db.generation(), before);
}
