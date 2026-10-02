//! The trash (P1-11): its list, most recently trashed first with keyset
//! paging; what a purge selects (a cutoff, oldest trash first, the 30-day
//! retention); and the purge itself, idempotent.

mod support;

use rusqlite::Connection;
use shelfy_core::repo::posts::{self, PostFilter};
use shelfy_core::repo::{Platform, media};
use shelfy_core::schema::Kind;
use shelfy_core::search::index;
use shelfy_core::trash::{self, Position, RETENTION_MS};
use support::{
    DAY, NOW, bare_post, dump, fixture_library, insert_all, library, object, synthetic_posts,
};

/// The trash as the list must order it: newest `deleted_at` first, then the
/// larger id.
fn expected_order(conn: &Connection) -> Vec<String> {
    conn.prepare(
        "SELECT key FROM posts WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC, id DESC",
    )
    .unwrap()
    .query_map([], |r| r.get(0))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// Every page of the trash, `limit` at a time.
fn pages(conn: &Connection, limit: u32) -> Vec<String> {
    let mut keys = Vec::new();
    let mut after = None;
    loop {
        let page = trash::page(conn, limit, after).unwrap();
        assert!(page.items.len() <= limit as usize);
        assert!(page.items.iter().all(|p| p.deleted_at.is_some()));
        keys.extend(page.items.into_iter().map(|p| p.key));
        match page.next {
            Some(next) => after = Some(next),
            None => return keys,
        }
    }
}

#[test]
fn the_trash_lists_the_latest_deletes_first_by_keyset() {
    let conn = library();
    let ids = insert_all(&conn, &synthetic_posts(300, 3));
    // Three deletes; each stamps its posts alike, so ids break the ties.
    posts::trash(&conn, &ids[..40], NOW - 3 * DAY).unwrap();
    posts::trash(&conn, &ids[100..130], NOW - DAY).unwrap();
    posts::trash(&conn, &ids[200..203], NOW).unwrap();
    assert_eq!(trash::count(&conn).unwrap(), 73);
    let expected = expected_order(&conn);
    assert_eq!(expected.len(), 73);
    for limit in [1, 7, 73, 200] {
        assert_eq!(pages(&conn, limit), expected, "limit {limit}");
    }
    let first = trash::page(&conn, 73, None).unwrap();
    assert!(first.next.is_none(), "no empty last page");

    // Between two pages, a restore and a new delete shift nothing: the next
    // page starts after the last post shown.
    let page = trash::page(&conn, 10, None).unwrap();
    let shown: Vec<String> = page.items.iter().map(|p| p.key.clone()).collect();
    assert_eq!(shown, expected[..10]);
    let gone = posts::id_for_key(&conn, &expected[20]).unwrap().unwrap();
    posts::restore(&conn, &[gone], NOW + 1).unwrap();
    posts::trash(&conn, &ids[250..252], NOW + 2).unwrap();
    let mut rest = Vec::new();
    let mut after = page.next;
    while let Some(position) = after {
        let next = trash::page(&conn, 10, Some(position)).unwrap();
        rest.extend(next.items.into_iter().map(|p| p.key));
        after = next.next;
    }
    let mut wanted: Vec<String> = expected[10..].to_vec();
    wanted.retain(|k| *k != expected[20]);
    assert_eq!(rest, wanted);

    // A position past the end is an empty last page.
    let end = trash::page(
        &conn,
        10,
        Some(Position {
            deleted_at: i64::MIN,
            id: 0,
        }),
    )
    .unwrap();
    assert!(end.items.is_empty() && end.next.is_none());
    // The trash is what the list shows with `trash`.
    let listed = posts::count(
        &conn,
        &PostFilter {
            trash: true,
            ..PostFilter::default()
        },
    )
    .unwrap();
    assert_eq!(trash::count(&conn).unwrap(), listed);
}

#[test]
fn a_purge_takes_the_oldest_trash_up_to_its_cutoff() {
    let conn = library();
    let ids = insert_all(&conn, &synthetic_posts(50, 9));
    posts::trash(&conn, &ids[10..20], NOW - 40 * DAY).unwrap();
    posts::trash(&conn, &ids[..5], NOW - 31 * DAY).unwrap();
    posts::trash(&conn, &ids[30..33], NOW - 29 * DAY).unwrap();
    posts::trash(&conn, &ids[40..42], NOW).unwrap();

    // The nightly cutoff: trashed at least 30 days ago.
    let cutoff = trash::retention_cutoff(NOW);
    assert_eq!(cutoff, NOW - RETENTION_MS);
    assert_eq!(trash::count_purgeable(&conn, cutoff).unwrap(), 15);
    let mut oldest_first: Vec<i64> = ids[10..20].to_vec();
    oldest_first.extend(&ids[..5]);
    assert_eq!(trash::purgeable(&conn, cutoff, 100).unwrap(), oldest_first);
    assert_eq!(
        trash::purgeable(&conn, cutoff, 4).unwrap(),
        oldest_first[..4]
    );
    // Emptying the trash at NOW: everything trashed until then.
    assert_eq!(trash::count_purgeable(&conn, NOW).unwrap(), 20);
    assert_eq!(trash::count_purgeable(&conn, NOW - 1).unwrap(), 18);
    // Live posts are never purgeable.
    assert_eq!(trash::count_purgeable(&conn, i64::MAX).unwrap(), 20);
}

#[test]
fn a_purge_is_idempotent() {
    let conn = library();
    fixture_library(&conn);
    let ids = insert_all(&conn, &synthetic_posts(30, 4));
    // A shared cover keeps one object alive after the purge.
    let shared = media::upsert_object(&conn, &object(1, "image", "jpg"), NOW).unwrap();
    let mut keeper = bare_post("ig_77", Platform::Instagram, NOW);
    keeper.cover_object = Some(shared);
    posts::insert(&conn, &keeper, NOW).unwrap();

    let carousel = posts::id_for_key(&conn, "ig_3101").unwrap().unwrap();
    let site = posts::id_for_key(&conn, "web_00a1b2c3d4e5f6a7b8c9")
        .unwrap()
        .unwrap();
    let mut doomed = vec![carousel, site];
    doomed.extend(&ids[..10]);
    posts::trash(&conn, &doomed, NOW - 40 * DAY).unwrap();
    let through = trash::retention_cutoff(NOW);
    let mut purged = Vec::new();
    loop {
        let chunk = trash::purgeable(&conn, through, 5).unwrap();
        if chunk.is_empty() {
            break;
        }
        purged.extend(trash::purge(&conn, &chunk, NOW).unwrap());
    }
    assert_eq!(purged.len(), 12);
    assert!(purged.contains(&"ig_3101".to_owned()));
    assert!(posts::get(&conn, "ig_3101").unwrap().is_none());
    assert_eq!(index::verify(&conn).unwrap(), Vec::<i64>::new());
    let released: Vec<(i64, Option<i64>)> = conn
        .prepare("SELECT id, unreferenced_since FROM media_objects ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    // The cover is shared; the slide, poster, video, hero, favicon and band
    // lost their last reference.
    assert_eq!(
        released,
        [
            (1, None),
            (2, Some(NOW)),
            (3, Some(NOW)),
            (4, Some(NOW)),
            (5, Some(NOW)),
            (6, Some(NOW)),
            (7, Some(NOW))
        ]
    );

    // Again, later: nothing to purge, and nothing changes, not even the
    // objects' stamps.
    let before = dump(&conn, Kind::Library, "after the purge");
    assert!(trash::purgeable(&conn, i64::MAX, 100).unwrap().len() == 1);
    assert!(trash::purge(&conn, &doomed, NOW + DAY).unwrap().is_empty());
    assert!(trash::purgeable(&conn, through, 100).unwrap().is_empty());
    assert_eq!(dump(&conn, Kind::Library, "after the purge"), before);
}
