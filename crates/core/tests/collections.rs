//! Collections CRUD and membership (DATA-21 to DATA-27) and the library stats
//! (DATA-20).

mod support;

use shelfy_core::repo::collections::{
    self, CollectionPatch, DEFAULT_COLOR, DeleteMode, NewCollection,
};
use shelfy_core::repo::posts::{self, NewMedia, PostFilter};
use shelfy_core::repo::{Platform, RepoError, media, stats};
use support::{DAY, NOW, bare_post, fixture_library, insert_all, library, object};

fn new(name: &str) -> NewCollection {
    NewCollection {
        name: name.into(),
        ..NewCollection::default()
    }
}

#[test]
fn create_trims_defaults_and_validates() {
    let conn = library();
    let c = collections::create(&conn, &new("  Moodboard "), NOW).unwrap();
    assert_eq!(c.name, "Moodboard");
    assert_eq!(c.color, DEFAULT_COLOR);
    assert_eq!(c.count, 0);
    assert_eq!(c.created_at, NOW);
    assert!(c.platform.is_none());

    for bad in [new(""), new("   "), new(&"x".repeat(201))] {
        assert!(matches!(
            collections::create(&conn, &bad, NOW),
            Err(RepoError::Invalid { field: "name", .. })
        ));
    }
    let bad_color = NewCollection {
        color: Some("blue".into()),
        ..new("A")
    };
    assert!(matches!(
        collections::create(&conn, &bad_color, NOW),
        Err(RepoError::Invalid { field: "color", .. })
    ));
    let unlinked = NewCollection {
        platform: Some(Platform::Instagram),
        ..new("A")
    };
    assert!(matches!(
        collections::create(&conn, &unlinked, NOW),
        Err(RepoError::Invalid {
            field: "externalId",
            ..
        })
    ));
}

#[test]
fn a_platform_folder_links_to_one_collection() {
    let conn = library();
    let folder = |name: &str, platform| NewCollection {
        platform: Some(platform),
        external_id: Some("1790".into()),
        source_name: Some("saved folder".into()),
        ..new(name)
    };
    let linked = collections::create(&conn, &folder("Saved", Platform::Instagram), NOW).unwrap();
    assert_eq!(linked.platform, Some(Platform::Instagram));
    assert_eq!(linked.external_id.as_deref(), Some("1790"));
    assert!(matches!(
        collections::create(&conn, &folder("Again", Platform::Instagram), NOW),
        Err(RepoError::Conflict("collection"))
    ));
    // Same id on another platform, and any number of manual collections, are fine.
    collections::create(&conn, &folder("Board", Platform::Pinterest), NOW).unwrap();
    collections::create(&conn, &new("Manual"), NOW).unwrap();
    collections::create(&conn, &new("Manual"), NOW).unwrap();
}

#[test]
fn list_orders_by_creation_and_counts_live_posts() {
    let conn = library();
    let ids = insert_all(
        &conn,
        &[
            bare_post("ig_1", Platform::Instagram, NOW),
            bare_post("ig_2", Platform::Instagram, NOW),
            bare_post("ig_3", Platform::Instagram, NOW),
        ],
    );
    let b = collections::create(&conn, &new("B"), NOW).unwrap();
    let a = collections::create(&conn, &new("A"), NOW + 1).unwrap();
    collections::add_posts(&conn, &ids, &[b.id], NOW).unwrap();
    posts::trash(&conn, &[ids[0]], NOW).unwrap();
    let listed = collections::list(&conn).unwrap();
    assert_eq!(
        listed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["B", "A"]
    );
    assert_eq!(listed[0].count, 2, "trashed posts are not counted");
    assert_eq!(listed[1].count, 0);
    // A manual position wins over the creation order.
    conn.execute("UPDATE collections SET position = 0 WHERE id = ?1", [a.id])
        .unwrap();
    let listed = collections::list(&conn).unwrap();
    assert_eq!(listed[0].name, "A");
}

#[test]
fn update_keeps_blank_fields_and_the_link() {
    let conn = library();
    let linked = NewCollection {
        platform: Some(Platform::Instagram),
        external_id: Some("42".into()),
        source_name: Some("Lighting".into()),
        ..new("Lighting")
    };
    let c = collections::create(&conn, &linked, NOW).unwrap();
    let renamed = collections::update(
        &conn,
        c.id,
        &CollectionPatch {
            name: Some(" Lamps ".into()),
            color: Some(" ".into()),
        },
    )
    .unwrap();
    assert_eq!(renamed.name, "Lamps");
    assert_eq!(renamed.color, DEFAULT_COLOR);
    assert_eq!(renamed.external_id.as_deref(), Some("42"));
    assert_eq!(renamed.source_name.as_deref(), Some("Lighting"));
    let recolored = collections::update(
        &conn,
        c.id,
        &CollectionPatch {
            name: Some(String::new()),
            color: Some("#ABC".into()),
        },
    )
    .unwrap();
    assert_eq!(recolored.name, "Lamps");
    assert_eq!(recolored.color, "#abc");
    assert!(matches!(
        collections::update(&conn, 999, &CollectionPatch::default()),
        Err(RepoError::NotFound)
    ));
    assert!(matches!(
        collections::update(
            &conn,
            c.id,
            &CollectionPatch {
                color: Some("#12".into()),
                name: None
            }
        ),
        Err(RepoError::Invalid { field: "color", .. })
    ));
}

#[test]
fn delete_keeps_or_trashes_the_posts() {
    let conn = library();
    let ids = insert_all(
        &conn,
        &[
            bare_post("ig_1", Platform::Instagram, NOW),
            bare_post("ig_2", Platform::Instagram, NOW),
            bare_post("ig_3", Platform::Instagram, NOW),
        ],
    );
    let keep = collections::create(&conn, &new("Keep"), NOW).unwrap();
    let trash = collections::create(&conn, &new("Trash"), NOW).unwrap();
    collections::add_posts(&conn, &ids[..2], &[keep.id], NOW).unwrap();
    collections::add_posts(&conn, &ids[1..], &[trash.id], NOW).unwrap();

    assert_eq!(
        collections::delete(&conn, keep.id, DeleteMode::KeepPosts, NOW).unwrap(),
        0
    );
    assert_eq!(posts::count(&conn, &PostFilter::default()).unwrap(), 3);
    let memberships: i64 = conn
        .query_row("SELECT count(*) FROM post_collections", [], |r| r.get(0))
        .unwrap();
    assert_eq!(memberships, 2);

    assert_eq!(
        collections::delete(&conn, trash.id, DeleteMode::TrashPosts, NOW).unwrap(),
        2
    );
    assert_eq!(posts::count(&conn, &PostFilter::default()).unwrap(), 1);
    assert_eq!(
        posts::count(
            &conn,
            &PostFilter {
                trash: true,
                ..PostFilter::default()
            }
        )
        .unwrap(),
        2
    );
    assert!(collections::list(&conn).unwrap().is_empty());
    assert!(matches!(
        collections::delete(&conn, trash.id, DeleteMode::KeepPosts, NOW),
        Err(RepoError::NotFound)
    ));
}

#[test]
fn membership_is_idempotent_and_removable() {
    let conn = library();
    let ids = insert_all(
        &conn,
        &[
            bare_post("ig_1", Platform::Instagram, NOW),
            bare_post("ig_2", Platform::Instagram, NOW),
            bare_post("ig_3", Platform::Instagram, NOW),
        ],
    );
    posts::trash(&conn, &[ids[2]], NOW).unwrap();
    let a = collections::create(&conn, &new("A"), NOW).unwrap();
    let b = collections::create(&conn, &new("B"), NOW).unwrap();
    // Unknown (99) and trashed posts are skipped.
    assert_eq!(
        collections::add_posts(&conn, &[ids[0], ids[1], ids[2], 99], &[a.id, b.id], NOW).unwrap(),
        4
    );
    assert_eq!(
        collections::add_posts(&conn, &[ids[0], ids[1]], &[a.id], NOW).unwrap(),
        0
    );
    assert!(matches!(
        collections::add_posts(&conn, &[ids[0]], &[a.id, 999], NOW),
        Err(RepoError::NotFound)
    ));

    // DATA-27: remove one post from one collection.
    assert!(collections::remove_post(&conn, ids[0], a.id).unwrap());
    assert!(!collections::remove_post(&conn, ids[0], a.id).unwrap());
    let a_now = collections::get(&conn, a.id).unwrap().unwrap();
    assert_eq!(a_now.count, 1);
    let in_a = posts::list_ids(
        &conn,
        &PostFilter {
            collection_id: Some(a.id),
            ..PostFilter::default()
        },
    )
    .unwrap();
    assert_eq!(in_a, [ids[1]]);
    let in_b = posts::list_ids(
        &conn,
        &PostFilter {
            collection_id: Some(b.id),
            ..PostFilter::default()
        },
    )
    .unwrap();
    assert_eq!(in_b.len(), 2);
}

#[test]
fn stats_group_over_every_platform() {
    let conn = library();
    fixture_library(&conn);
    let s = stats::get(&conn).unwrap();
    assert_eq!(s.total, 5);
    assert_eq!(s.trashed, 1);
    let by_platform: Vec<(Platform, u64)> = s.by_platform.iter().map(|(p, n)| (*p, *n)).collect();
    assert_eq!(
        by_platform,
        [
            (Platform::Instagram, 1),
            (Platform::Twitter, 1),
            (Platform::Pinterest, 1),
            (Platform::Web, 1),
            (Platform::Manual, 1), // DATA-20: the desktop left manual bookmarks out
        ]
    );
    assert_eq!(s.by_platform.values().sum::<u64>(), s.total);
    let by_type: Vec<(&str, u64)> = s
        .by_media_type
        .iter()
        .map(|(t, n)| (t.as_str(), *n))
        .collect();
    assert_eq!(
        by_type,
        [
            ("carousel", 1),
            ("file", 1),
            ("image", 1),
            ("text", 1),
            ("website", 1)
        ]
    );
    assert_eq!(s.stored, 2);
    assert_eq!(
        (
            s.stored_by_kind.covers,
            s.stored_by_kind.images,
            s.stored_by_kind.videos
        ),
        (1, 1, 1)
    );

    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["byPlatform"]["manual"], 1);
    assert_eq!(json["storedByKind"]["covers"], 1);
}

#[test]
fn stats_of_an_empty_library_are_zero() {
    let conn = library();
    let s = stats::get(&conn).unwrap();
    assert_eq!(s.total, 0);
    assert_eq!(s.by_platform.len(), 5);
    assert!(s.by_platform.values().all(|n| *n == 0));
    assert!(s.by_media_type.is_empty());
    assert_eq!((s.stored, s.trashed), (0, 0));
}

#[test]
fn stats_count_kept_videos_and_image_slides_separately() {
    let conn = library();
    let video = media::upsert_object(&conn, &object(10, "video", "mp4"), NOW).unwrap();
    let image = media::upsert_object(&conn, &object(11, "image", "jpg"), NOW).unwrap();
    let mut v = bare_post("x_1", Platform::Twitter, NOW - DAY);
    v.media_type = "video".into();
    v.media = vec![NewMedia {
        kind: "video".into(),
        video_object_id: Some(video),
        ..NewMedia::default()
    }];
    let mut i = bare_post("x_2", Platform::Twitter, NOW);
    i.media = vec![NewMedia {
        kind: "image".into(),
        object_id: Some(image),
        ..NewMedia::default()
    }];
    insert_all(&conn, &[v, i, bare_post("x_3", Platform::Twitter, NOW)]);
    let s = stats::get(&conn).unwrap();
    assert_eq!(s.stored, 2);
    assert_eq!(
        (
            s.stored_by_kind.covers,
            s.stored_by_kind.images,
            s.stored_by_kind.videos
        ),
        (0, 1, 1)
    );
}
