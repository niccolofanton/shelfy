//! Notifications: creation and validation, newest-first pages, unread counts,
//! marking read, and the retention cap.

mod support;

use serde_json::{Map, Value, json};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::notifications::{
    self, KEEP, MAX_PARAMS_BYTES, NewNotification, ReadSelector,
};
use support::{NOW, library};

fn new(code: &str) -> NewNotification {
    NewNotification {
        kind: "job".into(),
        code: code.into(),
        ..NewNotification::default()
    }
}

fn params(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => unreachable!("an object"),
    }
}

#[test]
fn create_stores_every_field_unread() {
    let conn = library();
    let created = notifications::create(
        &conn,
        &NewNotification {
            kind: "migration".into(),
            code: "migration.done".into(),
            params: params(json!({ "posts": 6138, "merged": 3 })),
            target: Some("/settings/data".into()),
        },
        NOW,
    )
    .unwrap();
    assert!(created.id > 0);
    assert_eq!(created.read_at, None);
    assert_eq!(created.created_at, NOW);

    let page = notifications::list(&conn, None, 10).unwrap();
    assert_eq!(page.items, std::slice::from_ref(&created));
    assert_eq!(page.next_before, None);
    assert_eq!(page.items[0].params["posts"], 6138);
    assert_eq!(notifications::unread_count(&conn).unwrap(), 1);

    // No params are stored as NULL and read back as an empty object.
    let bare = notifications::create(&conn, &new("job.failed"), NOW).unwrap();
    let stored: Option<String> = conn
        .query_row(
            "SELECT params_json FROM notifications WHERE id = ?1",
            [bare.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, None);
    assert!(
        notifications::list(&conn, None, 1).unwrap().items[0]
            .params
            .is_empty()
    );
}

#[test]
fn create_validates_codes_targets_and_params() {
    let conn = library();
    for (bad, field) in [
        (new(""), "code"),
        (new("Job.Failed"), "code"),
        (new("job failed"), "code"),
        (new(&"a".repeat(101)), "code"),
        (
            NewNotification {
                kind: String::new(),
                ..new("job.failed")
            },
            "kind",
        ),
        (
            NewNotification {
                target: Some(String::new()),
                ..new("job.failed")
            },
            "target",
        ),
        (
            NewNotification {
                target: Some("x".repeat(501)),
                ..new("job.failed")
            },
            "target",
        ),
        (
            NewNotification {
                params: params(json!({ "text": "x".repeat(MAX_PARAMS_BYTES) })),
                ..new("job.failed")
            },
            "params",
        ),
    ] {
        match notifications::create(&conn, &bad, NOW) {
            Err(RepoError::Invalid { field: f, .. }) => assert_eq!(f, field),
            other => panic!("{bad:?}: {other:?}"),
        }
    }
    assert!(notifications::create(&conn, &new(&"a".repeat(100)), NOW).is_ok());
    assert!(notifications::create(&conn, &new("quota.exceeded_v2-b"), NOW).is_ok());
}

#[test]
fn pages_go_newest_first_without_repeats() {
    let conn = library();
    let ids: Vec<i64> = (0..7)
        .map(|n| {
            notifications::create(&conn, &new("job.failed"), NOW + n)
                .unwrap()
                .id
        })
        .collect();
    let mut seen = Vec::new();
    let mut before = None;
    loop {
        let page = notifications::list(&conn, before, 3).unwrap();
        assert!(page.items.len() <= 3);
        seen.extend(page.items.iter().map(|n| n.id));
        match page.next_before {
            Some(id) => before = Some(id),
            None => break,
        }
    }
    let newest_first: Vec<i64> = ids.iter().rev().copied().collect();
    assert_eq!(seen, newest_first);
    // Limits are clamped.
    assert_eq!(notifications::list(&conn, None, 0).unwrap().items.len(), 1);
    let all = notifications::list(&conn, None, 10_000).unwrap();
    assert_eq!(all.items.len(), 7);
    assert_eq!(all.next_before, None);
}

#[test]
fn marking_read_counts_only_changes() {
    let conn = library();
    let ids: Vec<i64> = (0..5)
        .map(|n| {
            notifications::create(&conn, &new("job.failed"), NOW + n)
                .unwrap()
                .id
        })
        .collect();
    let read = |selector| notifications::mark_read(&conn, &selector, NOW + 100).unwrap();

    assert_eq!(read(ReadSelector::Ids(vec![ids[0], ids[1], 9_999])), 2);
    assert_eq!(read(ReadSelector::Ids(vec![ids[0]])), 0, "already read");
    assert_eq!(notifications::unread_count(&conn).unwrap(), 3);
    assert_eq!(read(ReadSelector::UpTo(ids[3])), 2);
    assert_eq!(notifications::unread_count(&conn).unwrap(), 1);

    let page = notifications::list(&conn, None, 10).unwrap();
    let unread: Vec<i64> = page
        .items
        .iter()
        .filter(|n| n.read_at.is_none())
        .map(|n| n.id)
        .collect();
    assert_eq!(unread, [ids[4]]);
    assert!(
        page.items
            .iter()
            .filter(|n| n.id != ids[4])
            .all(|n| n.read_at == Some(NOW + 100))
    );
    assert_eq!(read(ReadSelector::Ids(Vec::new())), 0);
}

#[test]
fn only_the_newest_notifications_are_kept() {
    let conn = library();
    let total = i64::from(KEEP) + 5;
    let mut last = 0;
    for n in 0..total {
        last = notifications::create(&conn, &new("job.failed"), NOW + n)
            .unwrap()
            .id;
    }
    let count: i64 = conn
        .query_row("SELECT count(*) FROM notifications", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, i64::from(KEEP));
    let oldest: i64 = conn
        .query_row("SELECT min(id) FROM notifications", [], |r| r.get(0))
        .unwrap();
    assert_eq!(oldest, last - i64::from(KEEP) + 1);
    // New ids keep growing past the deleted ones.
    let next = notifications::create(&conn, &new("job.failed"), NOW + total)
        .unwrap()
        .id;
    assert_eq!(next, last + 1);
}
