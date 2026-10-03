//! Synthetic CAS GC: writer bounds, reference races, retention and operators.
mod support;

use std::fs::{self, File, FileTimes};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rusqlite::params;
use shelfy_core::db::{lock_library, unlock_library};
use shelfy_core::repo::{
    Platform, media,
    posts::{self, NewPost},
};
use shelfy_media::refs::{self, ObjectMeta, Origin, Role};
use shelfy_media::store::{IngestLimits, MediaStore, UserMedia};
use shelfy_media::{Digest, MediaKind, Rendition};
use shelfy_server::admin::gc::{self as admin, GcArgs};
use shelfy_server::events::model::JobState;
use shelfy_server::jobs::{Registry, gc};
use support::TestState;
use support::library::{ALICE, BOB, DAY, NOW, object};
use tokio_util::sync::CancellationToken;

fn media_store(t: &TestState, user: &str) -> UserMedia {
    MediaStore::new(t.data_dir().users_dir())
        .user(user)
        .unwrap()
}

async fn stored(t: &TestState, seed: u8, stamp: Option<i64>) -> (i64, Digest) {
    let media = media_store(t, ALICE);
    let bytes = [b"\xff\xd8\xff\xe0\0\x10JFIF\0".as_slice(), &[seed; 20]].concat();
    let staged = media.ingest(&bytes[..], IngestLimits::UPLOAD).unwrap();
    let digest = staged.digest();
    let id = t
        .write(ALICE, |tx| {
            let (id, _) = refs::publish_and_record(
                tx,
                &media,
                staged,
                &[(Rendition::G480, b"synthetic rendition")],
                &ObjectMeta::new(Role::Image, Origin::Server),
                NOW,
            )?;
            tx.execute(
                "UPDATE media_objects SET unreferenced_since=?2 WHERE id=?1",
                params![id, stamp],
            )?;
            Ok(id)
        })
        .await;
    (id, digest)
}

#[tokio::test]
async fn trash_references_and_reference_added_after_stamp_protect_files() {
    let t = TestState::new();
    t.add_user(ALICE);
    t.add_user(BOB);
    let (trashed, digest) = stored(&t, 1, Some(NOW - 2 * DAY)).await;
    let (raced, second) = stored(&t, 2, Some(NOW - 2 * DAY)).await;
    t.write(ALICE, |tx| {
        let mut p = NewPost::new("ig_1", Platform::Instagram, "1", "image", NOW);
        p.cover_object = Some(trashed);
        let id = posts::insert(tx, &p, NOW)?;
        posts::trash(tx, &[id], NOW)?;
        // A reference added since the stamp, without clearing it: collect must check again.
        let mut p = NewPost::new("ig_2", Platform::Instagram, "2", "image", NOW);
        p.cover_object = Some(raced);
        posts::insert(tx, &p, NOW)?;
        assert_eq!(refs::garbage_totals(tx, NOW - DAY)?, (0, 0));
        assert!(
            refs::collect_garbage(tx, &media_store(&t, ALICE), NOW - DAY, gc::CHUNK)?.is_empty()
        );
        Ok(())
    })
    .await;
    assert_eq!(
        gc::collect_user(&t.state, ALICE, NOW - DAY)
            .await
            .unwrap()
            .objects,
        0
    );
    let media = media_store(&t, ALICE);
    assert!(media.object_path(&digest, MediaKind::Jpeg).is_file());
    assert!(media.object_path(&second, MediaKind::Jpeg).is_file());
    assert!(!t.data_dir().library_db(BOB).exists());
}

#[tokio::test]
async fn collects_files_and_renditions_only_after_grace_and_recounts_usage() {
    let t = TestState::new();
    t.add_user(ALICE);
    let (_, old) = stored(&t, 3, Some(NOW - 2 * DAY)).await;
    let (_, fresh) = stored(&t, 4, Some(NOW - 1)).await;
    let (_, missed) = stored(&t, 5, None).await;
    shelfy_server::jobs::usage::recount(&t.state, ALICE)
        .await
        .unwrap();
    let report = gc::collect_user(&t.state, ALICE, NOW - DAY).await.unwrap();
    assert_eq!(report.objects, 1);
    assert_eq!(report.bytes, 31);
    let media = media_store(&t, ALICE);
    assert!(!media.object_path(&old, MediaKind::Jpeg).exists());
    assert!(media.object_path(&fresh, MediaKind::Jpeg).exists());
    assert!(media.object_path(&missed, MediaKind::Jpeg).exists());
    assert_eq!(
        fs::read_dir(media.root().join(old.shard()))
            .unwrap()
            .count(),
        0
    );
    let counted: i64 = t
        .control()
        .query_row(
            "SELECT usage_media_bytes FROM users WHERE id=?1",
            [ALICE],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(counted, 62);
    let again = gc::collect_user(&t.state, ALICE, NOW - DAY).await.unwrap();
    assert_eq!(again.objects, 0);
    assert_eq!(again.bytes, 0);
    // Reset's now cutoff includes newly restamped unreferenced objects.
    assert_eq!(
        gc::collect_user(&t.state, ALICE, t.state.jobs().clock().now_ms())
            .await
            .unwrap()
            .objects,
        2
    );
}

#[tokio::test]
async fn writer_never_deletes_more_than_five_hundred_objects_per_transaction() {
    let t = TestState::new();
    t.add_user(ALICE);
    t.write(ALICE, |tx| {
        for n in 0..1001_u32 {
            let mut row = object(1, "jpg", false);
            row.sha256[..4].copy_from_slice(&n.to_be_bytes());
            media::upsert_object(tx, &row, NOW)?;
        }
        tx.execute(
            "UPDATE media_objects SET unreferenced_since=?1",
            [NOW - 2 * DAY],
        )?;
        Ok(())
    })
    .await;
    let db = t.state.user_db(ALICE).await.unwrap();
    let counter = Arc::new(Mutex::new((db.generation(), 0_usize)));
    let weak = Arc::downgrade(&db);
    db.write(|tx| {
        tx.create_scalar_function("gc_test_chunk",0,rusqlite::functions::FunctionFlags::SQLITE_UTF8,move |_| {
            let generation = weak.upgrade().unwrap().generation();
            let mut count = counter.lock().unwrap();
            if count.0 != generation { *count = (generation,0); }
            count.1 += 1;
            Ok(count.1 <= 500)
        })?;
        tx.execute_batch("CREATE TEMP TRIGGER gc_chunk_limit BEFORE DELETE ON media_objects BEGIN SELECT CASE WHEN gc_test_chunk()=0 THEN RAISE(ABORT,'more than 500 objects in a transaction') END; END")?;
        Ok::<_, shelfy_core::repo::RepoError>(())
    }).unwrap();
    let handle = shelfy_server::telemetry::metrics::install();
    let report = gc::collect_user(&t.state, ALICE, NOW - DAY).await.unwrap();
    assert_eq!(report.objects, 1001);
    assert!(handle.render().contains("shelfy_gc_objects_deleted_total"));
    assert!(handle.render().contains("shelfy_gc_bytes_freed_total"));
}

#[tokio::test]
async fn temp_sweep_keeps_active_files_and_missing_libraries_missing() {
    let t = TestState::new();
    t.add_user(ALICE);
    t.add_user(BOB);
    t.write(ALICE, |_| Ok(())).await;
    let dir = media_store(&t, ALICE).root().join(".tmp");
    fs::create_dir_all(&dir).unwrap();
    let old = dir.join("old.part");
    let fresh = dir.join("active.part");
    File::create(&old)
        .unwrap()
        .set_times(
            FileTimes::new()
                .set_modified(SystemTime::now() - gc::RETENTION - Duration::from_secs(60)),
        )
        .unwrap();
    File::create(&fresh).unwrap();
    assert_eq!(
        gc::collect_user(&t.state, ALICE, NOW - DAY)
            .await
            .unwrap()
            .temporary_files,
        1
    );
    assert!(!old.exists());
    assert!(fresh.exists());
    assert_eq!(
        gc::collect_user(&t.state, BOB, NOW - DAY).await.unwrap(),
        gc::Report::default()
    );
    assert!(!t.data_dir().library_db(BOB).exists());
}

#[tokio::test]
async fn operator_dry_run_preserves_every_row_file_stamp_and_usage() {
    let t = TestState::new();
    t.add_user(ALICE);
    t.add_user(BOB);
    let (_, old) = stored(&t, 6, Some(NOW - 2 * DAY)).await;
    let (_, missed) = stored(&t, 7, None).await;
    let before: (i64, Option<i64>) = t
        .control()
        .query_row(
            "SELECT usage_bytes,usage_updated_at FROM users WHERE id=?1",
            [ALICE],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut out = Vec::new();
    admin::run(
        &t.data_dir(),
        &GcArgs {
            user: Some(ALICE.into()),
            dry_run: true,
        },
        &mut out,
    )
    .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "objects=1 bytes=31\n");
    let db = t.state.user_db(ALICE).await.unwrap();
    assert_eq!(
        db.read(|conn| refs::find(conn, &missed))
            .unwrap()
            .unwrap()
            .unreferenced_since,
        None
    );
    assert!(
        media_store(&t, ALICE)
            .object_path(&old, MediaKind::Jpeg)
            .exists()
    );
    let after = t
        .control()
        .query_row(
            "SELECT usage_bytes,usage_updated_at FROM users WHERE id=?1",
            [ALICE],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, after);
    let mut out = Vec::new();
    admin::run(
        &t.data_dir(),
        &GcArgs {
            user: None,
            dry_run: false,
        },
        &mut out,
    )
    .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "objects=1 bytes=31\n");
    assert!(
        !media_store(&t, ALICE)
            .object_path(&old, MediaKind::Jpeg)
            .exists()
    );
    assert!(!t.data_dir().library_db(BOB).exists());
    assert!(
        admin::run(
            &t.data_dir(),
            &GcArgs {
                user: Some("../invalid".into()),
                dry_run: true
            },
            &mut Vec::new()
        )
        .is_err()
    );
}

#[tokio::test]
async fn taxonomy_retention_follows_job_existence_including_cancelled_failed_markers() {
    let t = TestState::new();
    t.add_user(ALICE);
    let mut ids = Vec::new();
    for state in ["queued", "running", "failed", "cancelled", "succeeded"] {
        let id = t.enqueue(ALICE, "ai.run", serde_json::json!({})).await;
        t.control()
            .execute("UPDATE jobs SET state=?2 WHERE id=?1", params![id, state])
            .unwrap();
        ids.push(id);
    }
    t.write(ALICE, |tx| {
        for id in &ids { tx.execute("INSERT INTO ai_cache(kind,key_hash,value_json,created_at) VALUES('taxonomy.run',?1,'{}',?2)",params![id.to_be_bytes().as_slice(),NOW-100*DAY])?; }
        // More than one page of absent plans; live low IDs must not stop the scan.
        for id in 10000_i64..10510 { tx.execute("INSERT INTO ai_cache(kind,key_hash,value_json,created_at) VALUES('taxonomy.run',?1,'{}',?2)",params![id.to_be_bytes().as_slice(),NOW])?; }
        Ok(())
    }).await;
    assert_eq!(
        gc::collect_user(&t.state, ALICE, NOW - DAY)
            .await
            .unwrap()
            .taxonomy_plans,
        510
    );
    assert_eq!(
        t.write(ALICE, |tx| Ok(tx.query_row(
            "SELECT count(*) FROM ai_cache",
            [],
            |r| r.get::<_, i64>(0)
        )?))
        .await,
        5
    );
    t.control()
        .execute(
            "UPDATE jobs SET finished_at=?1 WHERE state IN ('failed','cancelled','succeeded')",
            [NOW - 15 * DAY],
        )
        .unwrap();
    assert_eq!(
        shelfy_server::control::jobs::prune_finished(&t.control(), NOW - 14 * DAY).unwrap(),
        3
    );
    assert_eq!(
        gc::collect_user(&t.state, ALICE, NOW - DAY)
            .await
            .unwrap()
            .taxonomy_plans,
        3
    );
}

#[tokio::test(start_paused = true)]
async fn locked_library_waits_without_using_a_try_then_collects_after_unlock() {
    struct Hold(std::sync::mpsc::Sender<()>);
    impl Drop for Hold {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let (release, wait) = std::sync::mpsc::channel();
    let _hold = Hold(release);
    let _blocking = tokio::task::spawn_blocking(move || {
        let _ = wait.recv();
    });
    let t = TestState::with_jobs(
        Registry::new()
            .register(gc::kind())
            .register(shelfy_server::jobs::usage::kind()),
    );
    t.add_user(ALICE);
    let (_, digest) = stored(&t, 8, Some(NOW - 2 * DAY)).await;
    lock_library(&t.data_dir().users_dir(), ALICE, "test restore").unwrap();
    let id = t.enqueue(ALICE, gc::KIND, serde_json::json!({})).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let started = Instant::now();
    loop {
        let row = t.job(ALICE, id).await;
        if row.state == JobState::Queued && row.run_at > t.state.jobs().clock().now_ms() {
            assert_eq!(row.attempts, 0);
            assert_eq!(row.run_at - t.state.jobs().clock().now_ms(), 60_000);
            assert_eq!(row.error_code, None);
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "{row:?}");
        tokio::task::yield_now().await;
    }
    assert!(
        media_store(&t, ALICE)
            .object_path(&digest, MediaKind::Jpeg)
            .exists()
    );
    unlock_library(&t.data_dir().users_dir(), ALICE).unwrap();
    tokio::time::advance(Duration::from_secs(60)).await;
    let started = Instant::now();
    loop {
        let row = t.job(ALICE, id).await;
        if row.state == JobState::Succeeded {
            assert_eq!(row.attempts, 0);
            break;
        }
        if row.state == JobState::Queued && row.run_at > t.state.jobs().clock().now_ms() {
            tokio::time::advance(Duration::from_millis(
                (row.run_at - t.state.jobs().clock().now_ms()) as u64,
            ))
            .await;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "{row:?}");
        tokio::task::yield_now().await;
    }
    assert!(
        !media_store(&t, ALICE)
            .object_path(&digest, MediaKind::Jpeg)
            .exists()
    );
}

#[test]
fn registry_schedules_gc_nightly_with_the_contract_limits() {
    let registry = shelfy_server::jobs::kinds::registry();
    let kind = registry.get(gc::KIND).unwrap();
    let spec = kind.spec();
    assert!(spec.nightly);
    assert_eq!((spec.global, spec.per_user, spec.max_attempts), (1, 1, 3));
    assert_eq!(spec.lease, Duration::from_secs(3600));
}

#[tokio::test]
async fn active_exports_pin_even_unreferenced_objects_for_operator_and_reset_collection() {
    let t = TestState::new();
    t.add_user(ALICE);
    t.add_user(BOB);
    let (_, digest) = stored(&t, 9, Some(NOW - 2 * DAY)).await;
    let export = t.enqueue(ALICE, "export", serde_json::json!({})).await;
    for state in ["queued", "running"] {
        t.control()
            .execute(
                "UPDATE jobs SET state=?2 WHERE id=?1",
                params![export, state],
            )
            .unwrap();
        let error = gc::collect_user(&t.state, ALICE, t.state.jobs().clock().now_ms())
            .await
            .unwrap_err();
        assert_eq!(error.code(), shelfy_server::error::ErrorCode::Unavailable);
        assert!(
            media_store(&t, ALICE)
                .object_path(&digest, MediaKind::Jpeg)
                .exists()
        );
        assert!(
            admin::run(
                &t.data_dir(),
                &GcArgs {
                    user: Some(ALICE.into()),
                    dry_run: false
                },
                &mut Vec::new()
            )
            .is_err()
        );
    }
    // A finished export owns its independent ZIP bytes, not live CAS references.
    t.control()
        .execute("UPDATE jobs SET state='succeeded' WHERE id=?1", [export])
        .unwrap();
    // Another user's active export must not delay this collection.
    t.enqueue(BOB, "export", serde_json::json!({})).await;
    assert_eq!(
        gc::collect_user(&t.state, ALICE, NOW - DAY)
            .await
            .unwrap()
            .objects,
        1
    );
}

#[tokio::test(start_paused = true)]
async fn gc_waits_for_export_without_spending_a_try_and_continues_after_cancel() {
    struct Hold(std::sync::mpsc::Sender<()>);
    impl Drop for Hold {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let (release, wait) = std::sync::mpsc::channel();
    let _hold = Hold(release);
    let _blocking = tokio::task::spawn_blocking(move || {
        let _ = wait.recv();
    });
    let t = TestState::with_jobs(
        Registry::new()
            .register(gc::kind())
            .register(shelfy_server::jobs::export::kind())
            .register(shelfy_server::jobs::usage::kind()),
    );
    t.add_user(ALICE);
    let (_, digest) = stored(&t, 10, Some(NOW - 2 * DAY)).await;
    t.state.jobs().pause(ALICE, "export").await.unwrap();
    let export = t.enqueue(ALICE, "export", serde_json::json!({})).await;
    let id = t.enqueue(ALICE, gc::KIND, serde_json::json!({})).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    for _ in 0..4 {
        let started = Instant::now();
        loop {
            let row = t.job(ALICE, id).await;
            if row.state == JobState::Queued && row.run_at > t.state.jobs().clock().now_ms() {
                assert_eq!(row.attempts, 0);
                assert_eq!(row.run_at - t.state.jobs().clock().now_ms(), 5_000);
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "{row:?}");
            tokio::task::yield_now().await;
        }
        assert!(
            media_store(&t, ALICE)
                .object_path(&digest, MediaKind::Jpeg)
                .exists()
        );
        tokio::time::advance(Duration::from_secs(5)).await;
    }
    t.state.jobs().cancel(ALICE, export).await.unwrap();
    let started = Instant::now();
    loop {
        let row = t.job(ALICE, id).await;
        if row.state == JobState::Succeeded {
            assert_eq!(row.attempts, 0);
            break;
        }
        if row.state == JobState::Queued && row.run_at > t.state.jobs().clock().now_ms() {
            tokio::time::advance(Duration::from_millis(
                (row.run_at - t.state.jobs().clock().now_ms()) as u64,
            ))
            .await;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "{row:?}");
        tokio::task::yield_now().await;
    }
    assert!(
        !media_store(&t, ALICE)
            .object_path(&digest, MediaKind::Jpeg)
            .exists()
    );
}
