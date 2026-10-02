//! The admin commands, in-process and through the binary: `create-owner`,
//! `invite` and `snapshot` round trips on temporary data directories.

use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc;

use rusqlite::{Connection, OpenFlags, params};
use shelfy_core::db::{UserDb, UserDbConfig};
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::schema::{self, Kind};
use shelfy_server::admin::invite::create_invite;
use shelfy_server::admin::owner::{CreateOwnerOutcome, create_owner};
use shelfy_server::admin::snapshot::snapshot;
use shelfy_server::config::{DataDir, PublicUrl};
use shelfy_server::tokens::hash_token;
use tempfile::TempDir;

const NOW: i64 = 1_790_899_200_000;

fn data_dir() -> (TempDir, DataDir) {
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path()).unwrap();
    (dir, data)
}

fn read_only(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn is_ulid(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[test]
fn create_owner_creates_one_owner_with_a_library() {
    let (_dir, data) = data_dir();
    let outcome = create_owner(&data, "  Owner@Example.TEST ").unwrap();
    let CreateOwnerOutcome::Created(id) = outcome.clone() else {
        panic!("expected a new owner, got {outcome:?}");
    };
    assert!(is_ulid(&id), "{id}");

    let control = read_only(&data.control_db());
    let (email, role, status, quota): (String, String, String, i64) = control
        .query_row(
            "SELECT email, role, status, quota_bytes FROM users WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(email, "owner@example.test");
    assert_eq!(role, "owner");
    assert_eq!(status, "active");
    assert_eq!(quota, 0, "the owner's quota is unlimited");
    let (action, target, meta): (String, String, String) = control
        .query_row(
            "SELECT action, target, meta_json FROM audit_log WHERE actor_user_id IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(action, "owner.create");
    assert_eq!(target, id);
    assert!(!meta.contains("example.test"), "no email in the audit log");

    // The library exists and is a migrated Shelfy library.
    let library = read_only(&data.library_db(&id));
    let app_id: i32 = library
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .unwrap();
    assert_eq!(app_id, Kind::Library.application_id());
    assert_eq!(
        schema::version(&library).unwrap(),
        Kind::Library.latest_version()
    );

    // Idempotent for the same email, whatever its case.
    assert_eq!(
        create_owner(&data, "OWNER@example.test").unwrap(),
        CreateOwnerOutcome::AlreadyExists(id.clone())
    );
    // One owner per instance.
    let err = create_owner(&data, "someone@example.test").unwrap_err();
    assert!(err.to_string().contains("single owner"), "{err:#}");
    // Malformed emails are refused before anything is written.
    let err = create_owner(&data, "not an email").unwrap_err();
    assert!(err.to_string().contains("invalid email"), "{err:#}");
    let users: i64 = control
        .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
        .unwrap();
    assert_eq!(users, 1);
}

#[test]
fn invite_needs_an_owner_and_keeps_only_the_hash() {
    let (_dir, data) = data_dir();
    let public = PublicUrl::parse("https://shelfy.example.test").unwrap();

    let err = create_invite(&data, &public, None, 7).unwrap_err();
    assert!(err.to_string().contains("no control database"), "{err:#}");
    assert!(
        !data.control_db().exists(),
        "a mistyped data directory is not initialized"
    );

    let owner = create_owner(&data, "owner@example.test").unwrap();
    let link = create_invite(&data, &public, Some("Friend@Example.test"), 3).unwrap();
    let url = link.url.expose();
    let token = url
        .strip_prefix("https://shelfy.example.test/invite/")
        .expect("an invite URL on the public origin");
    assert_eq!(token.len(), 43);
    assert!(!format!("{link:?}").contains(token), "Debug hides the link");

    let control = read_only(&data.control_db());
    let (email, role, created_by, created_at, expires_at): (String, String, String, i64, i64) =
        control
            .query_row(
                "SELECT email, role, created_by, created_at, expires_at FROM invites \
                 WHERE token_hash = ?1",
                [hash_token(token).as_slice()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
    assert_eq!(email, "friend@example.test");
    assert_eq!(role, "member");
    assert_eq!(created_by, owner.user_id());
    assert_eq!(expires_at - created_at, 3 * 86_400_000);
    assert_eq!(expires_at, link.expires_at);
    // The token itself is stored nowhere.
    let dump: String = control
        .query_row(
            "SELECT group_concat(COALESCE(meta_json, '') || action) FROM audit_log",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(dump.contains("invite.create"));
    assert!(!dump.contains(token));
    let plain: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM invites WHERE CAST(token_hash AS TEXT) = ?1",
            [token],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(plain, 0);
}

#[test]
fn snapshot_copies_consistent_self_contained_databases() {
    let (_dir, data) = data_dir();
    let owner = create_owner(&data, "owner@example.test").unwrap();
    let user = owner.user_id().to_owned();

    // The server holds the library open: committed rows sit in its WAL.
    let library = UserDb::open(data.library_db(&user), &UserDbConfig::default()).unwrap();
    library
        .write(|tx| {
            collections::create(
                tx,
                &NewCollection {
                    name: "Committed".into(),
                    ..NewCollection::default()
                },
                NOW,
            )
        })
        .unwrap();

    // A writer in the middle of a transaction while the snapshot runs.
    let path = data.library_db(&user);
    let (holding_tx, holding_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let writer = std::thread::spawn(move || {
        let mut conn = Connection::open(path).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO collections (name, position, created_at) VALUES ('Uncommitted', 99, ?1)",
            params![NOW],
        )
        .unwrap();
        holding_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        tx.rollback().unwrap();
    });
    holding_rx.recv().unwrap();

    let out = data.root().join("snap");
    let report = snapshot(&data, &out, &[]).unwrap();
    release_tx.send(()).unwrap();
    writer.join().unwrap();

    let names: Vec<&str> = report.files.iter().map(|f| f.name.as_str()).collect();
    let user_file = format!("users/{user}.sqlite");
    assert_eq!(names, ["control.sqlite", user_file.as_str()]);
    for file in &report.files {
        let path = out.join(&file.name);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), file.bytes);
        assert!(!path.with_extension("sqlite-wal").exists());
        assert!(!out.join(format!("{}.partial", file.name)).exists());
        let copy = read_only(&path);
        let mode: String = copy
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete", "{} is self-contained", file.name);
        let check: String = copy
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(check, "ok");
    }

    let control = read_only(&out.join("control.sqlite"));
    let owners: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM users WHERE role = 'owner'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owners, 1);
    assert_eq!(
        schema::version(&control).unwrap(),
        Kind::Control.latest_version()
    );

    let copy = read_only(&out.join(&user_file));
    let names: Vec<String> = collections::list(&copy)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["Committed"], "committed rows only, WAL included");
    drop(library);

    // Again into the same directory, for one user only: files are replaced.
    let report = snapshot(&data, &out, std::slice::from_ref(&user)).unwrap();
    assert_eq!(report.files.len(), 2);
    let err = snapshot(&data, &out, &["../etc".to_owned()]).unwrap_err();
    assert!(err.to_string().contains("invalid user id"), "{err:#}");
    let err = snapshot(&data, &out, &["01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()]).unwrap_err();
    assert!(err.to_string().contains("has no library"), "{err:#}");
}

#[test]
fn snapshot_refuses_a_data_directory_without_a_control_database() {
    let (_dir, data) = data_dir();
    let err = snapshot(&data, &data.root().join("snap"), &[]).unwrap_err();
    assert!(err.to_string().contains("no control database"), "{err:#}");
}

fn admin(data: &DataDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shelfy-server"))
        .arg("admin")
        .args(args)
        .env("SHELFY_DATA_DIR", data.root())
        .env("SHELFY_PUBLIC_URL", "https://shelfy.example.test")
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
fn the_binary_prints_results_on_stdout_only() {
    let (_dir, data) = data_dir();

    let created = admin(&data, &["create-owner", "--email", "owner@example.test"]);
    let line = stdout(&created);
    let id = line
        .trim_end()
        .strip_prefix("created owner ")
        .unwrap_or_else(|| panic!("unexpected output {line:?}"));
    assert!(is_ulid(id), "{id}");
    assert!(
        !line.contains("example.test"),
        "the email is not printed back"
    );
    assert!(created.stderr.is_empty(), "no logs on a clean run");

    let again = stdout(&admin(
        &data,
        &["create-owner", "--email", "owner@example.test"],
    ));
    assert_eq!(
        again.trim_end(),
        format!("owner {id} already exists; nothing changed")
    );

    let invite = stdout(&admin(&data, &["invite", "--ttl-days", "2"]));
    let mut lines = invite.lines();
    assert!(lines.next().unwrap().contains("valid 2 days"));
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("https://shelfy.example.test/invite/")
    );

    let out = data.root().join("snap");
    let snap = stdout(&admin(&data, &["snapshot", "--out", out.to_str().unwrap()]));
    assert!(snap.starts_with("control.sqlite\t"), "{snap}");
    assert!(snap.contains(&format!("users/{id}.sqlite\t")), "{snap}");
    assert!(snap.contains("snapshot of 2 databases"), "{snap}");

    // Failures exit non-zero with the reason on stderr, nothing on stdout.
    let failed = admin(&data, &["create-owner", "--email", "other@example.test"]);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("single owner"));
}
