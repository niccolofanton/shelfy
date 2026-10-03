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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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
use shelfy_server::admin::user::{lock, record_restore, restore_db, unlock};
use shelfy_server::admin::verify::{VerifyOptions, verify};
use shelfy_server::auth::bearer::{Scope, TokenUser, scopes};
use shelfy_server::config::{DataDir, create_private_dir};
use shelfy_server::error::{ErrorCode, USER_LOCKED_RETRY_AFTER_SECS};
use shelfy_server::events::model::JobState;
use shelfy_server::extension::VERSION_HEADER;
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

    // Locked: skipped, previous copy kept, even when its library changed.
    add_posts(&data, &owner, 3, 9);
    lock(&data, &owner, "test").unwrap();
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
        // An extension token's requests name its version (P2-03, C1).
        request
            .headers_mut()
            .insert(VERSION_HEADER, "0.2.0".parse().unwrap());
        request
    };
    assert_eq!(send(&app, request()).await.status(), StatusCode::OK);
    lock_library(&t.data_dir().users_dir(), &owner, "").unwrap();
    let refused = problem(send(&app, request()).await, StatusCode::LOCKED).await;
    assert_eq!(refused.code, ErrorCode::UserLocked);
}

#[tokio::test(start_paused = true)]
async fn a_locked_users_jobs_wait_for_the_unlock_without_using_their_tries() {
    // Review of P1-12, M2 (follow-up F3): every try that found the library
    // locked used one of the job's tries, so a restore of a few minutes
    // failed the user's jobs for good (3 tries: about 45–90 s).
    let tries = Arc::new(AtomicUsize::new(0));
    let worker = {
        let tries = Arc::clone(&tries);
        move |ctx: JobContext| {
            let tries = Arc::clone(&tries);
            async move {
                tries.fetch_add(1, Ordering::SeqCst);
                let name = format!("job {}", ctx.id());
                ctx.user_db(move |db: &UserDb| {
                    db.write(|tx| {
                        collections::create(
                            tx,
                            &NewCollection {
                                name,
                                ..NewCollection::default()
                            },
                            NOW,
                        )
                    })?;
                    Ok(())
                })
                .await?;
                Ok::<_, JobError>(Outcome::Succeeded)
            }
        }
    };
    let t = TestState::with_jobs(Registry::new().register(Kind::new(
        KindSpec::new("test.library").max_attempts(2),
        worker,
    )));
    t.add_user(ALICE);
    lock_library(&t.data_dir().users_dir(), ALICE, "restore").unwrap();
    let first = t.enqueue(ALICE, "test.library", json!({})).await;
    let second = t.enqueue(ALICE, "test.library", json!({})).await;
    let _scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());

    // A restore that takes ten minutes.
    tokio::time::sleep(Duration::from_secs(600)).await;
    for id in [first, second] {
        let job = t.job(ALICE, id).await;
        assert_eq!(job.state, JobState::Queued, "still waiting: {job:?}");
        assert_eq!(job.attempts, 0, "no try used: {job:?}");
    }
    // About one try a minute for the user, not one per job: the user's
    // other jobs wait while the library is locked.
    let tried = tries.load(Ordering::SeqCst);
    assert!((9..=11).contains(&tried), "{tried} tries in 10 minutes");

    unlock_library(&t.data_dir().users_dir(), ALICE).unwrap();
    for id in [first, second] {
        let done = t.wait_job(ALICE, id, |job| job.state.is_final()).await;
        assert_eq!(done.state, JobState::Succeeded);
        assert_eq!(done.attempts, 0);
    }
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
    assert_eq!(names.len(), 2, "{names:?}");
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
    // A handle opened before the lock: the server's, until it releases it.
    let held = UserDb::open(&live, &UserDbConfig::default()).unwrap();
    held.read(|_| Ok::<_, DbError>(())).unwrap();
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
    let err = restore_db(&data, &owner, &copy, Duration::ZERO).unwrap_err();
    assert!(err.to_string().contains("still open"), "{err:#}");
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        drop(held);
    });
    let installed = restore_db(&data, &owner, &copy, Duration::from_secs(10)).unwrap();
    record_restore(&data, &owner, &installed).unwrap();
    holder.join().unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", live.display()));
        assert!(!sidecar.exists(), "{suffix} left next to the library");
    }

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
    assert!(
        data.users_dir().join(&owner).join(LOCK_FILE_NAME).exists(),
        "still locked"
    );
    // The restored library is intact; the server cannot open it until the
    // unlock.
    assert_eq!(integrity(&Connection::open(&live).unwrap()), ["ok"]);
    assert!(matches!(
        UserDb::open(&live, &UserDbConfig::default()),
        Err(DbError::Locked)
    ));

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

/// A standalone library at `path` with `n` synthetic posts: a restored copy
/// with a history of its own.
fn library_copy(path: &Path, n: usize, seed: u64) {
    let db = UserDb::open(path, &UserDbConfig::default()).unwrap();
    db.write(|tx| {
        for post in synthetic_posts(n, seed, &[]) {
            posts::insert(tx, &post, NOW)?;
        }
        Ok::<_, RepoError>(())
    })
    .unwrap();
    db.checkpoint().unwrap();
}

/// `PRAGMA integrity_check`, first messages; `["ok"]` when intact.
fn integrity(conn: &Connection) -> Vec<String> {
    conn.prepare("PRAGMA integrity_check")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| r.get(0))?
                .take(3)
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap_or_else(|err| vec![format!("error: {err}")])
}

fn count(path: &Path, table: &str) -> i64 {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn new_collection(name: &str) -> NewCollection {
    NewCollection {
        name: name.into(),
        ..NewCollection::default()
    }
}

#[tokio::test]
async fn a_handle_from_before_the_lock_cannot_write_into_the_restored_library() {
    // Review of P1-12, H1: `restore-db` proved that no connection was open,
    // but a handle taken before the lock (a request or a job chunk in
    // flight) reopened the library by path after the server released it, so
    // its write landed in the restored library while the user was locked.
    let t = TestState::new();
    let data = t.data_dir();
    let owner = support::auth::owner(&t);
    let held = t.state.user_db(&owner).await.unwrap();
    held.write(|tx| collections::create(tx, &new_collection("before"), NOW))
        .unwrap();
    let out = data.root().join("snap");
    snapshot(&data, &out, &all()).unwrap();
    let copy = out.join(format!("users/{owner}.sqlite"));

    lock(&data, &owner, "restore").unwrap();
    t.state.user_dbs().run_maintenance(); // the server releases the library
    restore_db(&data, &owner, &copy, Duration::ZERO).unwrap();

    // The work in flight goes on with its handle, and is refused.
    let err = held
        .write(|tx| collections::create(tx, &new_collection("while locked"), NOW))
        .unwrap_err();
    assert!(matches!(&err, RepoError::Db(db) if db.is_locked()), "{err}");
    let live = data.library_db(&owner);
    assert_eq!(count(&live, "collections"), 1, "only the restored row");
    assert_eq!(held.open_connections(), (false, 0));

    // After the unlock, the user's next request serves the restored library.
    unlock(&data, &owner).unwrap();
    drop(held);
    let names: Vec<String> = t
        .state
        .user_db(&owner)
        .await
        .unwrap()
        .read(collections::list)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["before"]);
}

#[test]
fn a_connection_that_opened_the_library_before_a_restore_never_corrupts_it() {
    // Review of P1-12, H1: the restored copy was renamed over the live file.
    // A connection that had opened the old file (a released server handle
    // reopening it, an operator command) found its `-wal` and `-shm` by
    // name, next to the restored file: its write corrupted the restored
    // library ("Rowid out of order", "row missing from index"), for good.
    let (_dir, data, owner) = data_dir();
    seed_library(&data, &owner, 300, 0, 1);
    let copy = data.root().join("restored.sqlite");
    library_copy(&copy, 40, 2);
    let live = data.library_db(&owner);

    // Opened before the restore, not used yet: SQLite reads and locks
    // nothing before the first statement, so the restore finds it unused.
    let stale = Connection::open(&live).unwrap();
    lock(&data, &owner, "restore").unwrap();
    restore_db(&data, &owner, &copy, Duration::ZERO).unwrap();
    // Then it writes, as a connection that ignores the lock would.
    stale
        .execute("UPDATE posts SET caption = 'stale write ' || id", [])
        .unwrap();

    // While it is still open, the restored library is one intact database.
    let fresh = Connection::open(&live).unwrap();
    assert_eq!(integrity(&fresh), ["ok"]);
    let posts: i64 = fresh
        .query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(posts, 40, "the restored library");
    drop((stale, fresh));
    let after = Connection::open(&live).unwrap();
    assert_eq!(integrity(&after), ["ok"]);
    assert_eq!(post_count(&live), 40);
}

/// Copies the write-ahead log of a library at `path` with `n` posts while
/// it is open, so the copy holds frames that recovery would replay.
fn foreign_wal(path: &Path, n: usize) -> Vec<u8> {
    let db = UserDb::open(path, &UserDbConfig::default()).unwrap();
    db.write(|tx| {
        for post in synthetic_posts(n, 9, &[]) {
            posts::insert(tx, &post, NOW)?;
        }
        Ok::<_, RepoError>(())
    })
    .unwrap();
    let wal = std::fs::read(format!("{}-wal", path.display())).unwrap();
    assert!(!wal.is_empty());
    wal
}

/// The rollback journal of a library at `path` with `n` posts, caught in
/// the middle of a write as a crash leaves it: a hot journal that SQLite
/// plays back into whatever database it finds next to it.
fn foreign_hot_journal(path: &Path, n: usize) -> Vec<u8> {
    library_copy(path, n, 9);
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
    // Without syncs the journal header is complete at once.
    conn.pragma_update(None, "synchronous", "OFF").unwrap();
    conn.execute_batch("BEGIN; UPDATE posts SET caption = 'x' || caption;")
        .unwrap();
    let journal = std::fs::read(format!("{}-journal", path.display())).unwrap();
    conn.execute_batch("ROLLBACK").unwrap();
    assert!(journal.len() > 4096);
    journal
}

#[test]
fn stray_logs_and_journals_never_meet_the_restored_library() {
    // Review of P1-12, L2: the swap moved `-wal` and `-shm` aside only when
    // the live file existed, and never `-journal`. The stray log or journal
    // of a library that went missing was played into the restored one when
    // it was next opened.
    let (_dir, data, owner) = data_dir();
    let copy = data.root().join("restored.sqlite");
    library_copy(&copy, 25, 2);
    let live = data.library_db(&owner);
    let sidecar = |suffix: &str| PathBuf::from(format!("{}{suffix}", live.display()));
    let wal = foreign_wal(&data.root().join("other.sqlite"), 100);
    let journal = foreign_hot_journal(&data.root().join("third.sqlite"), 200);
    lock(&data, &owner, "restore").unwrap();

    for (suffix, bytes) in [("-wal", &wal), ("-journal", &journal)] {
        std::fs::remove_file(&live).unwrap();
        std::fs::write(sidecar(suffix), bytes).unwrap();
        let installed = restore_db(&data, &owner, &copy, Duration::ZERO).unwrap();
        assert_eq!(installed.kept, None, "there was no library");
        let conn = Connection::open(&live).unwrap();
        assert_eq!(integrity(&conn), ["ok"], "{suffix}");
        let posts: i64 = conn
            .query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(posts, 25, "the stray {suffix} was not played back");
        drop(conn);
        let kept: Vec<_> = std::fs::read_dir(live.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.to_str()
                    .is_some_and(|p| p.contains("library.pre-restore-") && p.ends_with(suffix))
            })
            .collect();
        assert_eq!(kept.len(), 1, "kept aside: {kept:?}");
        assert_eq!(&std::fs::read(&kept[0]).unwrap(), bytes);
    }
}

#[test]
fn install_snapshots_stages_every_copy_before_it_replaces_anything() {
    // Review of P1-12, L3: each copy was staged and swapped in turn, so an
    // I/O error on a library (a full disk) left the control database of the
    // snapshot installed and the library not.
    let (_dir, data, owner) = data_dir();
    seed_library(&data, &owner, 20, 0, 1);
    let out = data.root().join("snap");
    snapshot(&data, &out, &all()).unwrap();
    let new_dir = tempfile::tempdir().unwrap();
    let host = DataDir::new(new_dir.path().join("shelfy")).unwrap();
    // The library's copy cannot be staged: a directory is in its way.
    let blocked = PathBuf::from(format!("{}.restore", host.library_db(&owner).display()));
    std::fs::create_dir_all(&blocked).unwrap();

    let err = install_snapshots(&host, &out, false).unwrap_err();
    assert!(
        format!("{err:#}").contains("nothing was installed"),
        "{err:#}"
    );
    assert!(!host.control_db().exists(), "the control database waits");
    assert!(!host.library_db(&owner).exists());
    let staged = PathBuf::from(format!("{}.restore", host.control_db().display()));
    assert!(!staged.exists(), "no staged copy is left behind");

    std::fs::remove_dir(&blocked).unwrap();
    let report = install_snapshots(&host, &out, false).unwrap();
    assert!(report.verify.is_ok(), "{:#?}", report.verify);
    assert_eq!(post_count(&host.library_db(&owner)), 20);
}

#[test]
fn restore_db_reports_the_swap_even_when_the_audit_log_cannot_be_written() {
    // Review of P1-12, L3: the audit row was written before the command said
    // the library was restored, so a failed write hid a restore that had
    // happened, and an operator would run it again.
    let (_dir, data, owner) = data_dir();
    seed_library(&data, &owner, 30, 0, 1);
    let out = data.root().join("snap");
    snapshot(&data, &out, &all()).unwrap();
    add_posts(&data, &owner, 12, 5);
    stdout(&admin(&data, &["user", "lock", &owner]));
    // Something holds the control database's write lock past the busy
    // timeout.
    let control = Connection::open(data.control_db()).unwrap();
    control.execute_batch("BEGIN IMMEDIATE").unwrap();
    let copy = out.join(format!("users/{owner}.sqlite"));
    let output = admin(
        &data,
        &["user", "restore-db", &owner, copy.to_str().unwrap()],
    );
    control.execute_batch("ROLLBACK").unwrap();

    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(&format!("restored user {owner}'s library")),
        "{text}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("audit row could not be written"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(post_count(&data.library_db(&owner)), 30);
}

#[test]
fn a_missing_library_keeps_its_copy_and_no_restore_goes_without_it() {
    // Review of P1-12, M4: a snapshot removed the copy of a user whose
    // library was gone from the data directory although the account
    // existed, and `verify` and `install-snapshots` accepted a set without
    // it: a full restore silently lost the library.
    let (_dir, data, owner) = data_dir();
    seed_library(&data, &owner, 10, 0, 1);
    let out = data.root().join("backup-staging/db");
    snapshot(&data, &out, &all()).unwrap();
    let copy = out.join(format!("users/{owner}.sqlite"));

    std::fs::remove_dir_all(data.users_dir().join(&owner)).unwrap();
    let report = snapshot(&data, &out, &changed()).unwrap();
    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
    assert!(
        report.failed[0].1.contains("missing"),
        "{:?}",
        report.failed
    );
    assert_eq!(post_count(&copy), 10, "the last copy is kept");

    // A set without the library fails `verify` and installs nothing.
    std::fs::remove_file(&copy).unwrap();
    let report = verify(&data, &out, &VerifyOptions::default()).unwrap();
    assert!(!report.is_ok());
    assert!(
        report.databases[0].problems[0].contains(&format!("user {owner} has no library copy")),
        "{:?}",
        report.databases[0].problems
    );
    let new_dir = tempfile::tempdir().unwrap();
    let host = DataDir::new(new_dir.path().join("shelfy")).unwrap();
    let err = install_snapshots(&host, &out, false).unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("nothing was installed"), "{message}");
    assert!(message.contains("has no library copy"), "{message}");
    assert!(!host.control_db().exists());

    // So is the backup of a host without users (an empty host that was
    // backed up after the loss, picked as "latest").
    let empty_dir = tempfile::tempdir().unwrap();
    let empty = DataDir::new(empty_dir.path().join("shelfy")).unwrap();
    empty.create_layout().unwrap();
    drop(shelfy_core::db::ControlDb::open(empty.control_db(), &Default::default()).unwrap());
    let empty_out = empty.root().join("snap");
    snapshot(&empty, &empty_out, &all()).unwrap();
    let err = install_snapshots(&host, &empty_out, false).unwrap_err();
    assert!(format!("{err:#}").contains("no users"), "{err:#}");
}

#[test]
fn verify_leaves_the_live_library_of_a_locked_user_alone() {
    // Review of P1-12, H1: `admin verify` opened the live library of a user
    // being restored to count its rows.
    let (_dir, data, owner) = data_dir();
    seed_library(&data, &owner, 20, 2, 1);
    let out = data.root().join("snap");
    snapshot(&data, &out, &all()).unwrap();
    lock(&data, &owner, "restore").unwrap();
    // The restore holds the library.
    let restore = Connection::open(data.library_db(&owner)).unwrap();
    restore.busy_timeout(Duration::ZERO).unwrap();
    restore
        .pragma_update(None, "locking_mode", "EXCLUSIVE")
        .unwrap();
    restore
        .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
        .unwrap();
    restore.execute_batch("BEGIN EXCLUSIVE; COMMIT;").unwrap();

    let started = Instant::now();
    let users = [owner.clone()];
    let drill = VerifyOptions {
        users: &users,
        max_drift: 10,
    };
    let report = verify(&data, &out, &drill).unwrap();
    assert!(report.is_ok(), "{report:#?}");
    assert!(
        report.databases[1]
            .notes
            .iter()
            .any(|n| n.contains("locked for maintenance")),
        "{:?}",
        report.databases[1].notes
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "it did not wait"
    );
}

#[test]
fn create_owner_leaves_a_locked_library_alone() {
    // Review of P1-12, H1: running `create-owner` again (deploy scripts do)
    // opened, and could migrate, the library of an owner being restored.
    let (_dir, data, owner) = data_dir();
    lock(&data, &owner, "restore").unwrap();
    // The restore has the file in some state of its own: here, empty.
    let live = data.library_db(&owner);
    std::fs::write(&live, b"").unwrap();
    let outcome = create_owner(&data, OWNER_EMAIL).unwrap();
    assert_eq!(outcome.user_id(), owner);
    assert_eq!(
        std::fs::metadata(&live).unwrap().len(),
        0,
        "neither opened nor migrated"
    );
    unlock(&data, &owner).unwrap();
    create_owner(&data, OWNER_EMAIL).unwrap();
    assert!(std::fs::metadata(&live).unwrap().len() > 0, "repaired");
}

/// The current-thread runtime of `#[tokio::test]` has one worker, so blocking
/// it shows: this test fails if the 423 path blocks the async worker.
#[tokio::test]
async fn a_locked_users_request_does_not_block_the_async_worker() {
    // Review of P1-12, L1: the 423 path released the library on the async
    // worker: `PRAGMA optimize` and a TRUNCATE checkpoint, which wait up to
    // the busy timeout (5 s) for another connection's lock.
    let t = TestState::new();
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let data = t.data_dir();
    let owner = support::auth::owner(&t);
    let response = send(&app, with_session(get("/api/v1/posts"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(t.state.user_dbs().is_open(&owner));

    // Another connection holds the library's write lock for a second.
    let other = Connection::open(data.library_db(&owner)).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        other.execute_batch("COMMIT").unwrap();
    });
    lock(&data, &owner, "restore").unwrap();

    // A task that ticks every 10 ms records the longest gap between ticks.
    let longest = Arc::new(std::sync::Mutex::new(Duration::ZERO));
    let ticker = {
        let longest = Arc::clone(&longest);
        tokio::spawn(async move {
            let mut last = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let gap = last.elapsed();
                last = Instant::now();
                let mut longest = longest.lock().unwrap();
                *longest = (*longest).max(gap);
            }
        })
    };
    tokio::task::yield_now().await;
    let response = send(&app, with_session(get("/api/v1/posts"), &cookie)).await;
    // Let the ticker see the time that passed.
    tokio::time::sleep(Duration::from_millis(50)).await;
    ticker.abort();
    writer.join().unwrap();
    assert_eq!(response.status(), StatusCode::LOCKED);
    let longest = *longest.lock().unwrap();
    assert!(
        longest < Duration::from_millis(500),
        "the async worker was blocked for {longest:?}"
    );
    // The 423 came after the release: the library is closed.
    assert!(!t.state.user_dbs().is_open(&owner));
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
