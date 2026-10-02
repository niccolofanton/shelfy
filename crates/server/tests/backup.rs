//! Backups and restores (plan §3.5, §3.8): `admin snapshot --changed`,
//! `admin verify`, `admin user lock | unlock | restore-db`, `admin
//! install-snapshots`, the 423 of a locked user, and the library upgrade
//! sweep after boot. All data is synthetic.
//!
//! `seed_rehearsal_library` (ignored) builds the synthetic library of the
//! local backup rehearsal, `deploy/rehearse-backups.sh`.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use axum::routing::post as post_route;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use shelfy_core::db::{
    DbError, LOCK_FILE_NAME, UserDb, UserDbConfig, lock_library, unlock_library,
};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::posts;
use shelfy_media::refs::{self, ObjectMeta, Origin, Role};
use shelfy_media::store::{IngestLimits, MediaStore};
use shelfy_media::{Digest, MediaKind, Rendition};
use shelfy_server::admin::install::install_snapshots;
use shelfy_server::admin::owner::create_owner;
use shelfy_server::admin::snapshot::{CopyStatus, SnapshotOptions, snapshot};
use shelfy_server::admin::user::{lock, restore_db, unlock};
use shelfy_server::admin::verify::{VerifyOptions, verify};
use shelfy_server::auth::bearer::{Scope, TokenUser, scopes};
use shelfy_server::config::{DataDir, create_private_dir};
use shelfy_server::error::{ErrorCode, USER_LOCKED_RETRY_AFTER_SECS};
use shelfy_server::events::model::JobState;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::jobs::{JobContext, JobError, Kind, KindSpec, Outcome, Registry};
use shelfy_server::limits::RouteLimits;
use shelfy_server::serve::{SweepReport, upgrade_libraries};
use shelfy_server::tokens::{SecretToken, hash_token};
use shelfy_server::{app, routes};
use support::auth::{OWNER_EMAIL, post, sign_in, spa, with_session};
use support::library::{ALICE, NOW, synthetic_posts};
use support::{TestState, get, problem, send};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use utoipa_axum::router::OpenApiRouter;

/// A data directory with the owner; returns it and the owner's id.
fn data_dir() -> (TempDir, DataDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path().join("shelfy")).unwrap();
    let owner = create_owner(&data, OWNER_EMAIL)
        .unwrap()
        .user_id()
        .to_owned();
    (dir, data, owner)
}

/// Bytes a sniffer takes for a JPEG, distinct per `(seed, n)`.
fn fake_jpeg(seed: u64, n: u8) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.extend_from_slice(&seed.to_be_bytes());
    bytes.push(n);
    bytes.resize(2048 + usize::from(n) * 64, n);
    bytes
}

/// A small WebP-shaped rendition (the store does not decode renditions).
const FAKE_WEBP: &[u8] = b"RIFF\x1a\x00\x00\x00WEBPVP8 synthetic rendition";

/// Fills `user`'s library with `posts` synthetic posts and `objects` stored
/// images (real files in the store; the even ones with a g480 rendition),
/// each the cover of one of the first posts. Returns the images' digests.
fn seed_library(data: &DataDir, user: &str, posts: usize, objects: u8, seed: u64) -> Vec<Digest> {
    let path = data.library_db(user);
    create_private_dir(path.parent().unwrap()).unwrap();
    let db = UserDb::open(&path, &UserDbConfig::default()).unwrap();
    let media = MediaStore::new(data.users_dir()).user(user).unwrap();
    let staged: Vec<_> = (0..objects)
        .map(|n| {
            media
                .ingest(&fake_jpeg(seed, n)[..], IngestLimits::ARCHIVE_IMAGE)
                .unwrap()
        })
        .collect();
    db.write(|tx| {
        let mut ids = Vec::new();
        let mut digests = Vec::new();
        for (n, staged) in staged.into_iter().enumerate() {
            let renditions: Vec<(Rendition, &[u8])> = if n % 2 == 0 {
                vec![(Rendition::G480, FAKE_WEBP)]
            } else {
                Vec::new()
            };
            let meta = ObjectMeta::new(Role::Image, Origin::Server);
            let (id, stored) =
                refs::publish_and_record(tx, &media, staged, &renditions, &meta, NOW)?;
            ids.push(id);
            digests.push(stored.digest);
        }
        let mut new_posts = synthetic_posts(posts, seed, &ids);
        for (post, id) in new_posts.iter_mut().zip(&ids) {
            post.cover_object = Some(*id);
        }
        for post in &new_posts {
            posts::insert(tx, post, NOW)?;
        }
        Ok::<_, RepoError>(digests)
    })
    .unwrap()
}

/// Adds `n` more synthetic posts to `user`'s library.
fn add_posts(data: &DataDir, user: &str, n: usize, seed: u64) {
    let db = UserDb::open(data.library_db(user), &UserDbConfig::default()).unwrap();
    db.write(|tx| {
        for post in synthetic_posts(n, seed, &[]) {
            posts::insert(tx, &post, NOW)?;
        }
        Ok::<_, RepoError>(())
    })
    .unwrap();
}

fn post_count(path: &Path) -> i64 {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
        .unwrap()
}

fn all() -> SnapshotOptions<'static> {
    SnapshotOptions::default()
}

fn changed() -> SnapshotOptions<'static> {
    SnapshotOptions {
        users: &[],
        changed: true,
    }
}

fn status_of(report: &shelfy_server::admin::snapshot::SnapshotReport, name: &str) -> CopyStatus {
    report
        .files
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} not in the report"))
        .status
}

/// Waits out the window in which a fresh modification time is not trusted.
fn settle() {
    std::thread::sleep(Duration::from_millis(2100));
}

#[test]
fn snapshot_changed_copies_only_the_libraries_that_changed() {
    let (_dir, data, owner) = data_dir();
    let other = "01J9Z3B8K4QW6TFX0V7G2N5RCB";
    seed_library(&data, &owner, 40, 4, 1);
    seed_library(&data, other, 20, 2, 2);
    let out = data.root().join("backup-staging/db");
    let owner_copy = format!("users/{owner}.sqlite");
    let other_copy = format!("users/{other}.sqlite");

    // The first run copies everything, `--changed` or not.
    settle();
    let first = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(first.files.len(), 3);
    assert!(first.files.iter().all(|f| f.status == CopyStatus::Copied));
    assert!(out.join("snapshot-state.json").is_file());

    // Nothing changed: only the control database is copied.
    let second = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(status_of(&second, "control.sqlite"), CopyStatus::Copied);
    assert_eq!(status_of(&second, &owner_copy), CopyStatus::Unchanged);
    assert_eq!(status_of(&second, &other_copy), CopyStatus::Unchanged);

    // A write to one library copies that one again, with the new rows.
    add_posts(&data, &owner, 5, 3);
    let third = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(status_of(&third, &owner_copy), CopyStatus::Copied);
    assert_eq!(status_of(&third, &other_copy), CopyStatus::Unchanged);
    assert_eq!(post_count(&out.join(&owner_copy)), 45);
    // The write was moments ago: the record is not trusted, so the next run
    // copies it again even though nothing moved.
    let fourth = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(status_of(&fourth, &owner_copy), CopyStatus::Copied);
    settle();
    let fifth = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(status_of(&fifth, &owner_copy), CopyStatus::Copied);
    let sixth = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(status_of(&sixth, &owner_copy), CopyStatus::Unchanged);

    // A copy that disappeared from the directory is taken again.
    std::fs::remove_file(out.join(&other_copy)).unwrap();
    let seventh = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(status_of(&seventh, &other_copy), CopyStatus::Copied);

    // Without --changed everything is copied.
    let full = snapshot(&data, &out, &all()).unwrap();
    assert!(full.files.iter().all(|f| f.status == CopyStatus::Copied));
}

#[test]
fn snapshot_keeps_locked_copies_and_drops_deleted_users() {
    let (_dir, data, owner) = data_dir();
    let gone = "01J9Z3B8K4QW6TFX0V7G2N5RCB";
    seed_library(&data, &owner, 10, 1, 1);
    seed_library(&data, gone, 10, 1, 2);
    let out = data.root().join("backup-staging/db");
    snapshot(&data, &out, &all()).unwrap();

    // Locked: skipped, previous copy kept, even when its library changes.
    lock(&data, &owner, "test").unwrap();
    add_posts(&data, &owner, 3, 9);
    let report = snapshot(&data, &out, &all()).unwrap();
    let owner_copy = format!("users/{owner}.sqlite");
    assert_eq!(status_of(&report, &owner_copy), CopyStatus::Locked);
    assert_eq!(post_count(&out.join(&owner_copy)), 10);
    unlock(&data, &owner).unwrap();
    let report = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(status_of(&report, &owner_copy), CopyStatus::Copied);
    assert_eq!(post_count(&out.join(&owner_copy)), 13);

    // A deleted account: its copy goes too, so a restore cannot revive it.
    std::fs::remove_dir_all(data.users_dir().join(gone)).unwrap();
    let report = snapshot(&data, &out, &changed()).unwrap();
    assert_eq!(report.removed, [format!("users/{gone}.sqlite")]);
    assert!(!out.join(format!("users/{gone}.sqlite")).exists());
    assert_eq!(report.files.len(), 2);

    // A snapshot already writing to the directory keeps a second one out.
    let held = std::fs::File::open(out.join(".lock")).unwrap();
    held.lock().unwrap();
    let err = snapshot(&data, &out, &all()).unwrap_err();
    assert!(err.to_string().contains("another snapshot"), "{err:#}");
}

#[test]
fn verify_accepts_a_fresh_snapshot_and_reports_damage() {
    let (_dir, data, owner) = data_dir();
    let digests = seed_library(&data, &owner, 200, 6, 1);
    let out = data.root().join("snap");
    snapshot(&data, &out, &all()).unwrap();

    let report = verify(&data, &out, &VerifyOptions::default()).unwrap();
    assert!(report.is_ok(), "{report:#?}");
    assert_eq!(report.databases.len(), 2);
    let library = &report.databases[1];
    assert_eq!(library.name, format!("users/{owner}.sqlite"));
    let media = library.media.as_ref().unwrap();
    assert_eq!(media.referenced, 6);
    let posts = library.tables.iter().find(|t| t.table == "posts").unwrap();
    assert_eq!((posts.copy, posts.live), (Some(200), Some(200)));

    // The live library moved on: exact counts fail, a 10 % allowance passes.
    add_posts(&data, &owner, 15, 7);
    let exact = verify(&data, &out, &VerifyOptions::default()).unwrap();
    assert!(!exact.is_ok());
    assert!(
        exact.databases[1].problems[0].contains("posts 200 vs 215"),
        "{:?}",
        exact.databases[1].problems
    );
    let drill = VerifyOptions {
        users: std::slice::from_ref(&owner),
        max_drift: 10,
    };
    assert!(verify(&data, &out, &drill).unwrap().is_ok());

    // Volatile tables never fail: sign-ins add sessions and audit rows.
    let control = Connection::open(data.control_db()).unwrap();
    control
        .execute(
            "INSERT INTO audit_log (at, action) VALUES (?1, 'session.create')",
            params![NOW],
        )
        .unwrap();
    assert!(verify(&data, &out, &drill).unwrap().is_ok());

    // A missing object, a truncated one and a missing rendition are listed.
    let media = MediaStore::new(data.users_dir()).user(&owner).unwrap();
    std::fs::remove_file(media.object_path(&digests[1], MediaKind::Jpeg)).unwrap();
    std::fs::write(
        media.object_path(&digests[3], MediaKind::Jpeg),
        b"\xFF\xD8\xFF",
    )
    .unwrap();
    std::fs::remove_file(media.rendition_path(&digests[0], Rendition::G480)).unwrap();
    let damaged = verify(&data, &out, &drill).unwrap();
    let check = damaged.databases[1].media.as_ref().unwrap();
    assert_eq!(check.broken.len(), 2, "{check:?}");
    assert_eq!(check.missing_renditions.len(), 1);
    assert_eq!(damaged.problem_count(), 2);

    // A damaged copy fails its integrity check.
    let copy = out.join(format!("users/{owner}.sqlite"));
    corrupt(&copy);
    let broken = verify(&data, &out, &drill).unwrap();
    assert!(
        broken.databases[1]
            .problems
            .iter()
            .any(|p| p.contains("integrity check failed")),
        "{:?}",
        broken.databases[1].problems
    );

    // Asking for a library the directory has no copy of is an error.
    let missing = ["01J9Z3B8K4QW6TFX0V7G2N5RCZ".to_owned()];
    let options = VerifyOptions {
        users: &missing,
        max_drift: 0,
    };
    assert!(verify(&data, &out, &options).is_err());
}

/// Overwrites the cells of the `posts` table's root page with garbage, as a
/// torn write or a bad disk would.
fn corrupt(path: &Path) {
    let (root, page_size) = {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let root: i64 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE name = 'posts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap();
        (
            usize::try_from(root).unwrap(),
            usize::try_from(page_size).unwrap(),
        )
    };
    let mut bytes = std::fs::read(path).unwrap();
    let start = (root - 1) * page_size;
    for b in &mut bytes[start + 12..start + page_size] {
        *b = 0x5a;
    }
    std::fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn a_locked_user_gets_423_until_unlocked() {
    let t = TestState::new();
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let data = t.data_dir();
    let owner = support::auth::owner(&t);

    let response = send(&app, with_session(get("/api/v1/posts"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(t.state.user_dbs().is_open(&owner));

    assert!(lock(&data, &owner, "restore").unwrap());
    assert!(!lock(&data, &owner, "again").unwrap(), "already locked");
    for uri in [
        "/api/v1/posts",
        "/api/v1/stats",
        "/api/v1/me",
        "/api/v1/events",
        "/media/0000000000000000000000000000000000000000000000000000000000000000.jpg",
    ] {
        let response = send(&app, with_session(get(uri), &cookie)).await;
        assert_eq!(
            response.headers()[header::RETRY_AFTER],
            USER_LOCKED_RETRY_AFTER_SECS.to_string(),
            "{uri}"
        );
        let refused = problem(response, StatusCode::LOCKED).await;
        assert_eq!(refused.code, ErrorCode::UserLocked, "{uri}");
    }
    // The refusal released the library at once.
    t.state.user_dbs().run_maintenance();
    assert!(!t.state.user_dbs().is_open(&owner));
    // Public routes still answer: the user can sign out.
    let response = send(&app, spa(&t, post("/api/v1/auth/logout"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Unlocked, the user is back (with a new session).
    assert!(unlock(&data, &owner).unwrap());
    assert!(!unlock(&data, &owner).unwrap(), "not locked any more");
    let cookie = sign_in(&app, &t).await;
    let response = send(&app, with_session(get("/api/v1/posts"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);

    // Both changes are in the audit log, without the reason.
    let control = Connection::open(data.control_db()).unwrap();
    let actions: Vec<String> = control
        .prepare("SELECT action || ' ' || target || ' ' || meta_json FROM audit_log WHERE action LIKE 'user.%' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        actions,
        [
            format!(r#"user.lock {owner} {{"via":"cli"}}"#),
            format!(r#"user.unlock {owner} {{"via":"cli"}}"#),
        ]
    );
    // Locking needs an existing user and a safe id.
    assert!(lock(&data, "01J9Z3B8K4QW6TFX0V7G2N5RCZ", "x").is_err());
    assert!(lock(&data, "../x", "x").is_err());
}

async fn token_route(user: TokenUser<scopes::Lookup>) -> String {
    user.id().to_owned()
}

#[tokio::test]
async fn a_locked_user_gets_423_on_token_routes_too() {
    let t = TestState::new();
    let owner = support::auth::owner(&t);
    let token = format!("shx_{}", SecretToken::generate().expose());
    Connection::open(t.data_dir().control_db())
        .unwrap()
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, 'extension', ?3, 'lookup', ?4)",
            params![new_ulid(), owner, hash_token(&token).as_slice(), now_ms()],
        )
        .unwrap();
    let routes = OpenApiRouter::new().route("/test/token", post_route(token_route));
    let access = routes::access().token(Method::POST, "/test/token", Scope::Lookup, false);
    let app = app::build_with_access(
        t.state.clone(),
        routes::router().merge(RouteLimits::STANDARD.apply(routes)),
        access,
    );
    let request = || {
        let mut request = post("/test/token");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        request
    };
    assert_eq!(send(&app, request()).await.status(), StatusCode::OK);
    lock_library(&t.data_dir().users_dir(), &owner, "").unwrap();
    let refused = problem(send(&app, request()).await, StatusCode::LOCKED).await;
    assert_eq!(refused.code, ErrorCode::UserLocked);
}

#[tokio::test(start_paused = true)]
async fn a_locked_users_jobs_back_off_and_run_after_the_unlock() {
    let worker = |ctx: JobContext| async move {
        ctx.user_db(|db: &UserDb| {
            db.write(|tx| {
                collections::create(
                    tx,
                    &NewCollection {
                        name: "after the restore".into(),
                        ..NewCollection::default()
                    },
                    NOW,
                )
            })?;
            Ok(())
        })
        .await?;
        Ok::<_, JobError>(Outcome::Succeeded)
    };
    let t = TestState::with_jobs(
        Registry::new().register(Kind::new(KindSpec::new("test.library"), worker)),
    );
    t.add_user(ALICE);
    lock_library(&t.data_dir().users_dir(), ALICE, "").unwrap();
    let id = t.enqueue(ALICE, "test.library", json!({})).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());

    // While the library is locked, a try fails transiently: queued again.
    let held = t.wait_job(ALICE, id, |job| job.attempts == 1).await;
    assert_eq!(held.state, JobState::Queued, "retried, not failed");
    assert_eq!(held.error_code.as_deref(), Some("unavailable"));
    assert!(
        held.error_detail
            .as_deref()
            .is_some_and(|d| d.contains("locked")),
        "{:?}",
        held.error_detail
    );

    unlock_library(&t.data_dir().users_dir(), ALICE).unwrap();
    let done = t.wait_job(ALICE, id, |job| job.state.is_final()).await;
    assert_eq!(done.state, JobState::Succeeded);
    let names: Vec<String> = t
        .state
        .user_db(ALICE)
        .await
        .unwrap()
        .read(collections::list)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["after the restore"]);
}

#[test]
fn restore_db_swaps_a_locked_library_and_keeps_the_old_one() {
    let (_dir, data, owner) = data_dir();
    seed_library(&data, &owner, 30, 2, 1);
    let out = data.root().join("snap");
    snapshot(&data, &out, &all()).unwrap();
    let copy = out.join(format!("users/{owner}.sqlite"));
    add_posts(&data, &owner, 12, 5);
    let live = data.library_db(&owner);
    assert_eq!(post_count(&live), 42);

    // Only a locked user's library is replaced.
    let err = restore_db(&data, &owner, &copy, Duration::ZERO).unwrap_err();
    assert!(err.to_string().contains("not locked"), "{err:#}");
    lock(&data, &owner, "restore").unwrap();

    // A damaged copy is refused before anything changes.
    let damaged = data.root().join("damaged.sqlite");
    std::fs::copy(&copy, &damaged).unwrap();
    corrupt(&damaged);
    let err = restore_db(&data, &owner, &damaged, Duration::ZERO).unwrap_err();
    assert!(err.to_string().contains("cannot be restored"), "{err:#}");
    assert_eq!(post_count(&live), 42);

    // A process holding the library open is waited for (the server releases
    // it within its maintenance interval).
    let held = UserDb::open(&live, &UserDbConfig::default()).unwrap();
    held.read(|_| Ok::<_, DbError>(())).unwrap();
    let err = restore_db(&data, &owner, &copy, Duration::ZERO).unwrap_err();
    assert!(err.to_string().contains("still open"), "{err:#}");
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        drop(held);
    });
    let installed = restore_db(&data, &owner, &copy, Duration::from_secs(10)).unwrap();
    holder.join().unwrap();

    assert_eq!(post_count(&live), 30, "the restored library is live");
    let kept = installed.kept.expect("the previous library is kept");
    assert!(
        kept.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("library.pre-restore-")
    );
    assert_eq!(post_count(&kept), 42);
    assert!(!PathBuf::from(format!("{}-wal", live.display())).exists());
    assert!(
        data.users_dir().join(&owner).join(LOCK_FILE_NAME).exists(),
        "still locked"
    );
    // The restored library opens and upgrades like any other.
    let db = UserDb::open(&live, &UserDbConfig::default()).unwrap();
    let integrity: String = db
        .read(|c| {
            c.query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(db);

    let control = Connection::open(data.control_db()).unwrap();
    let meta: String = control
        .query_row(
            "SELECT meta_json FROM audit_log WHERE action = 'library.restore' AND target = ?1",
            [&owner],
            |r| r.get(0),
        )
        .unwrap();
    assert!(meta.contains(r#""keptPrevious":true"#), "{meta}");

    // A library SQLite cannot read any more (the usual reason to restore) is
    // replaced as it is, and kept under a name of its own.
    let junk = vec![0x5a_u8; 8192];
    std::fs::write(&live, &junk).unwrap();
    let again = restore_db(&data, &owner, &copy, Duration::ZERO).unwrap();
    assert_eq!(post_count(&live), 30);
    let junk_kept = again.kept.unwrap();
    assert_ne!(junk_kept, kept);
    assert_eq!(std::fs::read(&junk_kept).unwrap(), junk);
    assert_eq!(post_count(&kept), 42, "the earlier kept file is untouched");
}

#[test]
fn install_snapshots_restores_a_whole_host() {
    let (_dir, data, owner) = data_dir();
    let digests = seed_library(&data, &owner, 50, 3, 1);
    let out = data.root().join("snap");
    snapshot(&data, &out, &all()).unwrap();

    // A new host: the media set restored, the databases not yet.
    let new_dir = tempfile::tempdir().unwrap();
    let host = DataDir::new(new_dir.path().join("shelfy")).unwrap();
    let old_media = MediaStore::new(data.users_dir()).user(&owner).unwrap();
    let new_media = MediaStore::new(host.users_dir()).user(&owner).unwrap();
    for digest in &digests {
        let from = old_media.object_path(digest, MediaKind::Jpeg);
        let to = new_media.object_path(digest, MediaKind::Jpeg);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(&from, &to).unwrap();
        let rendition = old_media.rendition_path(digest, Rendition::G480);
        if rendition.exists() {
            std::fs::copy(
                &rendition,
                new_media.rendition_path(digest, Rendition::G480),
            )
            .unwrap();
        }
    }

    // One damaged copy installs nothing.
    let bad = tempfile::tempdir().unwrap();
    copy_tree(&out, bad.path());
    corrupt(&bad.path().join(format!("users/{owner}.sqlite")));
    let err = install_snapshots(&host, bad.path(), false).unwrap_err();
    assert!(err.to_string().contains("nothing was installed"), "{err:#}");
    assert!(!host.control_db().exists());

    let report = install_snapshots(&host, &out, false).unwrap();
    assert!(report.verify.is_ok(), "{:#?}", report.verify);
    assert_eq!(report.installed.len(), 2);
    assert!(report.installed.iter().all(|i| i.kept.is_none()));
    assert_eq!(post_count(&host.library_db(&owner)), 50);

    // Installing over existing data needs --force, which keeps the old files.
    let err = install_snapshots(&host, &out, false).unwrap_err();
    assert!(err.to_string().contains("--force"), "{err:#}");
    let report = install_snapshots(&host, &out, true).unwrap();
    assert!(report.installed.iter().all(|i| i.kept.is_some()));
    assert!(report.verify.is_ok());

    // The installed host serves: the server state opens it.
    let state = shelfy_server::state::AppState::open(shelfy_server::config::Config::with_data_dir(
        host.clone(),
    ))
    .unwrap();
    let users: i64 = state
        .control()
        .read(|c| {
            c.query_row("SELECT count(*) FROM users", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(users, 1);
    // With the server running, another install is refused.
    let err = install_snapshots(&host, &out, true).unwrap_err();
    assert!(format!("{err:#}").contains("still open"), "{err:#}");
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn the_server_upgrades_an_older_control_database_at_boot() {
    // The control database as release v1 of the schema left it.
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/fixtures/schema/control-v1.sql");
    let sql = std::fs::read_to_string(fixture).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path().join("shelfy")).unwrap();
    data.create_layout().unwrap();
    Connection::open(data.control_db())
        .unwrap()
        .execute_batch(&sql)
        .unwrap();
    let users_before: i64 = Connection::open(data.control_db())
        .unwrap()
        .query_row("SELECT count(*) FROM users", [], |r| r.get(0))
        .unwrap();

    let state = shelfy_server::state::AppState::open(shelfy_server::config::Config::with_data_dir(
        data.clone(),
    ))
    .unwrap();
    let latest = shelfy_core::schema::Kind::Control.latest_version();
    assert!(latest > 1, "a later control schema exists to upgrade to");
    assert_eq!(
        state.control().schema_upgrade(),
        shelfy_core::schema::Upgrade::Upgraded {
            from: 1,
            to: latest
        }
    );
    let (version, users): (i64, i64) = state
        .control()
        .read(|c| {
            c.query_row(
                "SELECT (SELECT user_version FROM pragma_user_version), count(*) FROM users",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(DbError::from)
        })
        .unwrap();
    assert_eq!(usize::try_from(version).unwrap(), latest);
    assert_eq!(users, users_before, "the accounts survive the upgrade");
}

#[tokio::test]
async fn the_sweep_upgrades_libraries_after_boot() {
    let t = TestState::new();
    let users = t.data_dir().users_dir();
    // A library left behind (here: version 0, an empty file), a current one
    // and a locked one.
    let behind = "01J9Z3B8K4QW6TFX0V7G2N5RCA";
    std::fs::create_dir_all(users.join(behind)).unwrap();
    std::fs::write(users.join(behind).join("library.sqlite"), b"").unwrap();
    let current = "01J9Z3B8K4QW6TFX0V7G2N5RCB";
    t.state.user_db(current).await.unwrap();
    let locked = "01J9Z3B8K4QW6TFX0V7G2N5RCC";
    t.state.user_db(locked).await.unwrap();
    lock_library(&users, locked, "").unwrap();

    let report = upgrade_libraries(
        t.state.clone(),
        CancellationToken::new(),
        Duration::ZERO,
        Duration::ZERO,
    )
    .await;
    assert_eq!(
        report,
        SweepReport {
            checked: 3,
            upgraded: 1,
            ahead: 0,
            locked: 1,
            failed: 0,
        }
    );
    assert!(
        !t.state.user_dbs().is_open(behind),
        "the sweep keeps the cache free"
    );
    let version: i64 = Connection::open(users.join(behind).join("library.sqlite"))
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert!(version > 0);

    // A sweep cancelled before its start does nothing.
    let token = CancellationToken::new();
    token.cancel();
    let report = upgrade_libraries(
        t.state.clone(),
        token,
        Duration::from_secs(60),
        Duration::ZERO,
    )
    .await;
    assert_eq!(report, SweepReport::default());
}

fn admin(data: &DataDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shelfy-server"))
        .arg("admin")
        .args(args)
        .env("SHELFY_DATA_DIR", data.root())
        .env_remove("SHELFY_OWNER_EMAIL")
        .env_remove("RUST_LOG")
        .output()
        .expect("run shelfy-server")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn the_binary_runs_the_backup_and_restore_commands() {
    let (_dir, data, owner) = data_dir();
    seed_library(&data, &owner, 20, 2, 1);
    let out = data.root().join("snap");
    let out_arg = out.to_str().unwrap();

    let text = stdout(&admin(&data, &["snapshot", "--changed", "--out", out_arg]));
    assert!(text.contains("control.sqlite\t"), "{text}");
    assert!(text.contains(&format!("users/{owner}.sqlite\t")), "{text}");
    assert!(text.contains("snapshot of 2 databases"), "{text}");
    assert!(text.contains("2 copied, 0 unchanged, 0 locked"), "{text}");

    let text = stdout(&admin(&data, &["verify", out_arg]));
    assert!(
        text.contains(&format!("users/{owner}.sqlite: ok")),
        "{text}"
    );
    assert!(
        text.ends_with("verify: 2 databases checked, 0 problems\n"),
        "{text}"
    );

    let text = stdout(&admin(
        &data,
        &["user", "lock", &owner, "--reason", "drill"],
    ));
    assert!(text.starts_with(&format!("locked user {owner}")), "{text}");
    let marker =
        std::fs::read_to_string(data.users_dir().join(&owner).join(LOCK_FILE_NAME)).unwrap();
    assert!(marker.contains(r#""reason":"drill""#), "{marker}");
    let copy = out.join(format!("users/{owner}.sqlite"));
    let text = stdout(&admin(
        &data,
        &[
            "user",
            "restore-db",
            &owner,
            copy.to_str().unwrap(),
            "--wait-secs",
            "5",
        ],
    ));
    assert!(text.contains("the previous library is kept as"), "{text}");
    let text = stdout(&admin(&data, &["user", "unlock", &owner]));
    assert_eq!(text, format!("unlocked user {owner}\n"));

    // Failures: the reason on stderr, nothing on stdout, a non-zero exit.
    add_posts(&data, &owner, 30, 4);
    let failed = admin(&data, &["verify", out_arg]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stdout).contains("PROBLEM: row counts differ"));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("checks failed"));
    let refused = admin(
        &data,
        &["user", "restore-db", &owner, copy.to_str().unwrap()],
    );
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("is not locked"));
    let refused = admin(&data, &["install-snapshots", out_arg]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--force"));
}

/// Builds the synthetic library of the local backup rehearsal
/// (`deploy/rehearse-backups.sh`) in `$SHELFY_REHEARSAL_DATA_DIR`: the owner
/// and a second user, each with a few thousand posts and stored images.
#[test]
#[ignore = "run by deploy/rehearse-backups.sh"]
fn seed_rehearsal_library() {
    let root = std::env::var_os("SHELFY_REHEARSAL_DATA_DIR")
        .expect("set SHELFY_REHEARSAL_DATA_DIR to the data directory to fill");
    let data = DataDir::new(root).unwrap();
    let owner = create_owner(&data, "owner@example.test")
        .unwrap()
        .user_id()
        .to_owned();
    seed_library(&data, &owner, 6_000, 120, 1);
    let member = "01J9Z3B8K4QW6TFX0V7G2N5RCB";
    Connection::open(data.control_db())
        .unwrap()
        .execute(
            "INSERT INTO users (id, email, role, quota_bytes, created_at) \
             VALUES (?1, 'member@example.test', 'member', 0, ?2)",
            params![member, NOW],
        )
        .unwrap();
    seed_library(&data, member, 1_500, 40, 2);
    println!(
        "seeded {} with users {owner} and {member}",
        data.root().display()
    );
}
