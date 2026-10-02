//! `media_objects` rows and reference counting: recording with dedupe,
//! publishing inside a transaction, the reference list against the schema,
//! stamping and the garbage collection the P4 GC builds on.

mod support;

use std::collections::BTreeSet;

use image::DynamicImage;
use rusqlite::Connection;
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, NewMedia, NewPost};
use shelfy_media::refs::{self, ObjectMeta, Origin, REFERENCES, Role};
use shelfy_media::render::{self, RenderSpec};
use shelfy_media::store::{IngestLimits, StoredObject, UserMedia};
use shelfy_media::{Digest, MediaKind, Rendition, Variants};
use support::{BLUE, DAY, NOW, RED, TempStore, USER, halves, jpeg, library, photo, png};

fn stored(seed: u64) -> StoredObject {
    let bytes = jpeg(&photo(16, 16, seed), 90, None);
    StoredObject {
        digest: Digest::of(&bytes),
        kind: MediaKind::Jpeg,
        size: bytes.len() as u64,
        deduplicated: false,
    }
}

fn image_meta() -> ObjectMeta {
    ObjectMeta::new(Role::Image, Origin::Server)
}

/// Inserts a post whose cover is `cover` and whose slides are `slides`.
fn post(conn: &Connection, n: u32, cover: Option<i64>, slides: &[i64]) -> i64 {
    let mut post = NewPost::new(
        format!("x_{n}"),
        Platform::Twitter,
        n.to_string(),
        "image",
        NOW,
    );
    post.cover_object = cover;
    post.media = slides
        .iter()
        .map(|&id| NewMedia {
            kind: "image".into(),
            object_id: Some(id),
            ..NewMedia::default()
        })
        .collect();
    posts::insert(conn, &post, NOW).unwrap()
}

/// Publishes a small JPEG with a fake rendition and records it.
fn publish(seed: u64, conn: &Connection, media: &UserMedia) -> (i64, StoredObject) {
    let bytes = jpeg(&photo(24, 24, seed), 90, None);
    let staged = media
        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap();
    let renditions = [(Rendition::G480, &b"webp"[..])];
    refs::publish_and_record(conn, media, staged, &renditions, &image_meta(), NOW).unwrap()
}

fn stamp(conn: &Connection, id: i64, at: i64) {
    conn.execute(
        "UPDATE media_objects SET unreferenced_since = ?2 WHERE id = ?1",
        [id, at],
    )
    .unwrap();
}

fn unreferenced_since(conn: &Connection, id: i64) -> Option<i64> {
    conn.query_row(
        "SELECT unreferenced_since FROM media_objects WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn recording_the_same_content_reuses_its_row() {
    let conn = library();
    let object = stored(1);
    let id = refs::record(&conn, &object, &image_meta(), NOW).unwrap();
    let row = refs::find(&conn, &object.digest).unwrap().unwrap();
    assert_eq!(row.id, id);
    assert_eq!(row.kind(), Some(MediaKind::Jpeg));
    assert_eq!((row.ext.as_str(), row.mime.as_str()), ("jpg", "image/jpeg"));
    assert_eq!(row.size, i64::try_from(object.size).unwrap());
    assert_eq!((row.width, row.height), (None, None));
    assert_eq!(row.variants, Variants::NONE);

    // Recorded again by another path: same row, which learns what it lacked.
    let richer = ObjectMeta {
        width: Some(1080),
        height: Some(1350),
        variants: Variants::NONE.with(Rendition::G480),
        ..ObjectMeta::new(Role::Poster, Origin::Extension)
    };
    assert_eq!(refs::record(&conn, &object, &richer, NOW + 1).unwrap(), id);
    let row = refs::find(&conn, &object.digest).unwrap().unwrap();
    assert_eq!((row.width, row.height), (Some(1080), Some(1350)));
    assert!(row.variants.contains(Rendition::G480));
    let (role, origin): (String, String) = conn
        .query_row(
            "SELECT role, origin FROM media_objects WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (role.as_str(), origin.as_str()),
        ("image", "server"),
        "first record wins"
    );

    // Known dimensions are not overwritten.
    let other = ObjectMeta {
        width: Some(1),
        height: Some(1),
        ..image_meta()
    };
    refs::record(&conn, &object, &other, NOW + 2).unwrap();
    let row = refs::find(&conn, &object.digest).unwrap().unwrap();
    assert_eq!((row.width, row.height), (Some(1080), Some(1350)));
    assert_eq!(refs::find(&conn, &stored(2).digest).unwrap(), None);
}

#[test]
fn publish_and_record_write_files_and_row_in_one_transaction() {
    let t = TempStore::new();
    let media = t.user(USER);
    let mut conn = library();
    let source = png(&DynamicImage::ImageRgb8(halves(900, 600, RED, BLUE)));
    let staged = media
        .ingest(source.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap();
    let rendered = render::render_file(staged.path(), RenderSpec::G480).unwrap();
    let meta = ObjectMeta {
        width: Some(rendered.source_width),
        height: Some(rendered.source_height),
        ..image_meta()
    };

    let tx = conn.transaction().unwrap();
    let renditions = [(Rendition::G480, rendered.webp.as_slice())];
    let (id, object) =
        refs::publish_and_record(&tx, &media, staged, &renditions, &meta, NOW).unwrap();
    let cover_post = post(&tx, 1, Some(id), &[id]);
    assert_eq!(
        refs::set_cover_thumbhash(&tx, id, &rendered.thumbhash, NOW).unwrap(),
        1
    );
    tx.commit().unwrap();

    assert!(media.contains(&object.digest, MediaKind::Png));
    assert_eq!(
        std::fs::read(media.rendition_path(&object.digest, Rendition::G480)).unwrap(),
        rendered.webp
    );
    let row = refs::find(&conn, &object.digest).unwrap().unwrap();
    assert_eq!(row.id, id);
    assert_eq!((row.width, row.height), (Some(900), Some(600)));
    assert!(row.variants.contains(Rendition::G480));
    let thumbhash: Vec<u8> = conn
        .query_row(
            "SELECT thumbhash FROM posts WHERE id = ?1",
            [cover_post],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(thumbhash, rendered.thumbhash);
    assert_eq!(
        refs::reference_count(&conn, id).unwrap(),
        2,
        "cover and slide 0"
    );
}

#[test]
fn record_rendition_adds_a_rendition_to_a_recorded_object() {
    let t = TempStore::new();
    let media = t.user(USER);
    let conn = library();
    let object = stored(3);
    let id = refs::record(&conn, &object, &image_meta(), NOW).unwrap();
    assert!(
        refs::record_rendition(&conn, &media, id, &object.digest, Rendition::G480, b"webp")
            .unwrap()
    );
    assert!(
        refs::find(&conn, &object.digest)
            .unwrap()
            .unwrap()
            .variants
            .contains(Rendition::G480)
    );
    assert!(
        media
            .rendition_path(&object.digest, Rendition::G480)
            .is_file()
    );
    assert!(!refs::add_variants(&conn, id + 100, Variants::NONE.with(Rendition::G480)).unwrap());
}

#[test]
fn the_reference_list_covers_every_foreign_key_to_media_objects() {
    let conn = library();
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")
        .unwrap();
    let names: Vec<String> = tables
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut schema = BTreeSet::new();
    for table in names {
        let mut keys = conn
            .prepare("SELECT \"table\", \"from\" FROM pragma_foreign_key_list(?1)")
            .unwrap();
        let rows = keys
            .query_map([&table], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap();
        for row in rows {
            let (target, column) = row.unwrap();
            if target == "media_objects" {
                schema.insert((table.clone(), column));
            }
        }
    }
    let listed: BTreeSet<(String, String)> = REFERENCES
        .iter()
        .map(|(t, c)| ((*t).to_owned(), (*c).to_owned()))
        .collect();
    assert_eq!(listed, schema);
}

#[test]
fn objects_are_stamped_when_their_last_reference_goes() {
    let conn = library();
    let a = refs::record(&conn, &stored(10), &image_meta(), NOW).unwrap();
    let b = refs::record(&conn, &stored(11), &image_meta(), NOW).unwrap();
    let first = post(&conn, 1, Some(a), &[a, b]);
    let second = post(&conn, 2, None, &[b]);
    assert_eq!(refs::reference_count(&conn, a).unwrap(), 2);
    assert_eq!(refs::reference_count(&conn, b).unwrap(), 2);

    // Still referenced: nothing to stamp.
    assert_eq!(refs::stamp_unreferenced(&conn, &[a, b], NOW).unwrap(), 0);

    // The core's purge stamps what it orphans; `b` is still on the second post.
    posts::purge(&conn, &[first], NOW + DAY).unwrap();
    assert_eq!(refs::reference_count(&conn, a).unwrap(), 0);
    assert_eq!(unreferenced_since(&conn, a), Some(NOW + DAY));
    assert_eq!(unreferenced_since(&conn, b), None);

    // A write path that drops a reference directly stamps it itself.
    conn.execute("DELETE FROM post_media WHERE post_id = ?1", [second])
        .unwrap();
    assert_eq!(
        refs::stamp_unreferenced(&conn, &[b], NOW + 2 * DAY).unwrap(),
        1
    );
    assert_eq!(unreferenced_since(&conn, b), Some(NOW + 2 * DAY));
    assert_eq!(
        refs::stamp_unreferenced(&conn, &[b], NOW + 3 * DAY).unwrap(),
        0,
        "stamped once"
    );
    assert_eq!(refs::stamp_unreferenced(&conn, &[], NOW).unwrap(), 0);
}

#[test]
fn restamp_repairs_missed_stamps_both_ways() {
    let conn = library();
    let orphan = refs::record(&conn, &stored(20), &image_meta(), NOW).unwrap();
    let revived = refs::record(&conn, &stored(21), &image_meta(), NOW).unwrap();
    stamp(&conn, revived, NOW - DAY);
    // Referenced again by a path that did not clear the stamp.
    post(&conn, 1, Some(revived), &[]);

    assert_eq!(refs::restamp(&conn, NOW).unwrap(), (1, 1));
    assert_eq!(unreferenced_since(&conn, orphan), Some(NOW));
    assert_eq!(unreferenced_since(&conn, revived), None);
    assert_eq!(refs::restamp(&conn, NOW + 1).unwrap(), (0, 0), "idempotent");
}

#[test]
fn garbage_collection_deletes_rows_and_files_past_the_grace_period() {
    let t = TempStore::new();
    let media = t.user(USER);
    let mut conn = library();

    let tx = conn.transaction().unwrap();
    let (old, old_object) = publish(30, &tx, &media);
    let (recent, recent_object) = publish(31, &tx, &media);
    let (kept, kept_object) = publish(32, &tx, &media);
    let (relinked, relinked_object) = publish(33, &tx, &media);
    post(&tx, 1, Some(kept), &[]);
    tx.commit().unwrap();
    stamp(&conn, old, NOW - 2 * DAY);
    stamp(&conn, recent, NOW - DAY / 2);
    stamp(&conn, relinked, NOW - 2 * DAY);
    post(&conn, 2, Some(relinked), &[]); // linked again without clearing the stamp

    let tx = conn.transaction().unwrap();
    let garbage = refs::collect_garbage(&tx, &media, NOW - DAY, 100).unwrap();
    tx.commit().unwrap();

    assert_eq!(garbage.len(), 1);
    assert_eq!(garbage[0].id, old);
    assert_eq!(garbage[0].digest, old_object.digest);
    assert_eq!(garbage[0].kind, Some(MediaKind::Jpeg));
    assert!(refs::find(&conn, &old_object.digest).unwrap().is_none());
    assert!(!media.contains(&old_object.digest, MediaKind::Jpeg));
    assert!(
        !media
            .rendition_path(&old_object.digest, Rendition::G480)
            .exists()
    );
    for object in [recent_object, kept_object, relinked_object] {
        assert!(refs::find(&conn, &object.digest).unwrap().is_some());
        assert!(media.contains(&object.digest, MediaKind::Jpeg));
    }

    // A rolled-back collection keeps every row (the files may already be gone,
    // which the next ingest of the same bytes repairs).
    stamp(&conn, recent, NOW - 2 * DAY);
    let tx = conn.transaction().unwrap();
    assert_eq!(
        refs::collect_garbage(&tx, &media, NOW - DAY, 100)
            .unwrap()
            .len(),
        1
    );
    drop(tx);
    assert!(refs::find(&conn, &recent_object.digest).unwrap().is_some());
    let bytes = jpeg(&photo(24, 24, 31), 90, None);
    let again = media
        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap()
        .publish()
        .unwrap();
    assert!(!again.deduplicated);
    assert!(media.contains(&recent_object.digest, MediaKind::Jpeg));
}

#[test]
fn garbage_collection_honours_its_limit() {
    let t = TempStore::new();
    let media = t.user(USER);
    let conn = library();
    for seed in 40..45 {
        let id = refs::record(&conn, &stored(seed), &image_meta(), NOW).unwrap();
        refs::stamp_unreferenced(&conn, &[id], NOW - 3 * DAY).unwrap();
    }
    assert_eq!(
        refs::collect_garbage(&conn, &media, NOW - DAY, 2)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        refs::collect_garbage(&conn, &media, NOW - DAY, 10)
            .unwrap()
            .len(),
        3
    );
    assert!(
        refs::collect_garbage(&conn, &media, NOW - DAY, 10)
            .unwrap()
            .is_empty()
    );
}
