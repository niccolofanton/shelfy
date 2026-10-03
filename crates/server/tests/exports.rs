//! Export v2: synthetic full-schema roundtrip, session isolation and recovery.
#[path = "../../core/tests/support/mod.rs"]
mod core_fixture;
mod support;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rusqlite::{Connection, params};
use shelfy_media::store::MediaStore;
use shelfy_media::{Digest, MediaKind};
use shelfy_server::control::exports as rows;
use shelfy_server::events::model::JobState;
use shelfy_server::exports::{self, bundle::Manifest};
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::jobs::export::Payload;
use shelfy_server::migrations::validate::{BundleLimits, validate_export};
use std::fs;
use std::io::{Cursor, Read};
use std::time::Duration;
use support::auth::{OWNER_EMAIL, add_member, owner, sign_in, sign_in_as, spa, with_session};
use support::{TestState, body, get, json, problem, send};
use tokio_util::sync::CancellationToken;

async fn populated(t: &TestState, user: &str) -> Vec<(String, Vec<u8>)> {
    let db = t.state.user_db(user).await.unwrap();
    db.write(|tx| {
        core_fixture::fixture_library(tx);
        Ok::<_, shelfy_core::repo::RepoError>(())
    })
    .unwrap();
    let media = MediaStore::new(t.data_dir().users_dir())
        .user(user)
        .unwrap();
    let objects: Vec<(i64, String)> = db
        .read(|c| {
            c.prepare("SELECT id,ext FROM media_objects ORDER BY id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()
                .map_err(shelfy_core::repo::RepoError::from)
        })
        .unwrap();
    let mut out = Vec::new();
    for (id, ext) in objects {
        let bytes = format!("synthetic-object-{id}").into_bytes();
        let digest = Digest::of(&bytes);
        let path = media.object_path(&digest, MediaKind::from_ext(&ext).unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        // A rendition with unrelated bytes must never appear in the bundle.
        fs::write(path.with_extension("g480.webp"), b"rendition-only-marker").unwrap();
        db.write(|tx| {
            tx.execute(
                "UPDATE media_objects SET sha256=?2,bytes=?3 WHERE id=?1",
                params![id, digest.as_bytes().as_slice(), bytes.len() as i64],
            )
            .map_err(shelfy_core::repo::RepoError::from)
        })
        .unwrap();
        out.push((
            format!("media/{}/{}.{}", digest.shard(), digest, ext),
            bytes,
        ));
    }
    out
}
fn create(t: &TestState, cookie: &str, key: &str) -> Request<Body> {
    spa(
        t,
        Request::post("/api/v1/exports")
            .header("idempotency-key", key)
            .body(Body::empty())
            .unwrap(),
        cookie,
    )
}
async fn wait(t: &TestState, user: &str, id: i64, state: JobState) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let row = t.state.jobs().get(user, id).await.unwrap().unwrap();
            if row.state == state {
                break;
            }
            assert!(
                !matches!(row.state, JobState::Failed | JobState::Cancelled) || state == row.state,
                "job failed: {row:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn path(t: &TestState, user: &str, id: &str) -> std::path::PathBuf {
    exports::file(&exports::directory(&t.state, user), id)
}

#[tokio::test]
async fn full_schema_bundle_is_private_lossless_and_resumable() {
    let t = TestState::new();
    let uid = owner(&t);
    let app = t.app();
    let objects = populated(&t, &uid).await;
    let cookie = sign_in(&app, &t).await;
    // Secrets live in the control DB only, and never enter an export.
    let mut secrets = vec![
        OWNER_EMAIL.as_bytes().to_vec(),
        cookie.as_bytes().to_vec(),
        b"provider-secret-export-test".to_vec(),
        b"token-hash-export-test".to_vec(),
    ];
    Connection::open(t.data_dir().control_db()).unwrap().execute("INSERT INTO provider_keys(user_id,provider_id,key_version,nonce,ciphertext,last4,created_at) VALUES (?1,'export-test',1,X'01',?2,'test',1)",params![uid,secrets[2]]).unwrap();
    Connection::open(t.data_dir().control_db()).unwrap().execute("INSERT INTO api_tokens(id,user_id,kind,token_hash,scopes,created_at) VALUES ('export-secret-token',?1,'migrate',?2,'migrate',1)",params![uid,secrets[3]]).unwrap();
    let session_hash: Vec<u8> = Connection::open(t.data_dir().control_db())
        .unwrap()
        .query_row("SELECT id_hash FROM sessions LIMIT 1", [], |r| r.get(0))
        .unwrap();
    secrets.push(session_hash);
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let response = send(&app, create(&t, &cookie, "export-a")).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let created = json(response).await;
    let id = created["id"].as_str().unwrap();
    let job = created["jobId"].as_i64().unwrap();
    wait(&t, &uid, job, JobState::Succeeded).await;
    let replay = send(&app, create(&t, &cookie, "export-a")).await;
    assert_eq!(replay.headers()["idempotent-replayed"], "true");
    assert_eq!(json(replay).await["id"], id);
    let dedupe = json(send(&app, create(&t, &cookie, "export-b")).await).await;
    assert_eq!(dedupe["id"], id);
    let listed = json(send(&app, with_session(get("/api/v1/exports"), &cookie)).await).await;
    assert!(listed[0]["bytes"].as_u64().unwrap() > 0);
    let response = send(
        &app,
        with_session(get(&format!("/api/v1/exports/{id}/download")), &cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment; filename=\"shelfy-export-")
    );
    let zip_bytes = body(response).await;
    assert!(
        zip_bytes.windows(4).any(|w| w == b"PK\x06\x06"),
        "ZIP64 end record"
    );
    let mut zip = zip::ZipArchive::new(Cursor::new(zip_bytes.clone())).unwrap();
    let manifest: Manifest =
        serde_json::from_reader(zip.by_name("manifest.json").unwrap()).unwrap();
    assert_eq!(manifest.format, "shelfy-export");
    assert_eq!(manifest.version, 2);
    assert_eq!(manifest.entries.len(), objects.len() + 1);
    let db = Connection::open(t.data_dir().library_db(&uid)).unwrap();
    for (table, count) in &manifest.counts {
        let current: i64 = db
            .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(*count, current as u64, "{table}");
    }
    let extracted = t.dir.path().join("reimport.sqlite");
    for entry in &manifest.entries {
        let mut member = zip.by_name(&entry.path).unwrap();
        assert_eq!(
            member.compression(),
            if entry.path == "library.sqlite" {
                zip::CompressionMethod::Deflated
            } else {
                zip::CompressionMethod::Stored
            }
        );
        let mut content = Vec::new();
        member.read_to_end(&mut content).unwrap();
        assert_eq!(content.len() as u64, entry.bytes);
        assert_eq!(Digest::of(&content).to_string(), entry.sha256);
        for secret in &secrets {
            assert!(
                !content.windows(secret.len()).any(|w| w == secret),
                "secret in {}",
                entry.path
            );
        }
        assert!(
            !content
                .windows(b"rendition-only-marker".len())
                .any(|w| w == b"rendition-only-marker")
        );
        if entry.path == "library.sqlite" {
            fs::write(&extracted, &content).unwrap();
        }
    }
    // The existing migration installer accepts the exact DB and masters.
    let restored = Connection::open(&extracted).unwrap();
    assert_eq!(
        core_fixture::dump(&db, shelfy_core::schema::Kind::Library, "roundtrip"),
        core_fixture::dump(&restored, shelfy_core::schema::Kind::Library, "roundtrip")
    );
    for n in 0..zip.len() {
        let mut payload = Vec::new();
        zip.by_index(n).unwrap().read_to_end(&mut payload).unwrap();
        for secret in &secrets {
            assert!(!payload.windows(secret.len()).any(|w| w == secret));
        }
    }
    let facts = validate_export(&extracted, &BundleLimits::default()).unwrap();
    assert_eq!(facts.objects.len(), objects.len());
    for (name, expected) in objects {
        let mut bytes = Vec::new();
        zip.by_name(&name).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }
    let range = with_session(
        Request::get(format!("/api/v1/exports/{id}/download"))
            .header(header::RANGE, "bytes=5-20")
            .body(Body::empty())
            .unwrap(),
        &cookie,
    );
    let response = send(&app, range).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body(response).await.as_ref(), &zip_bytes[5..=20]);
    let badrange = with_session(
        Request::get(format!("/api/v1/exports/{id}/download"))
            .header(header::RANGE, "bytes=999999999-")
            .body(Body::empty())
            .unwrap(),
        &cookie,
    );
    assert_eq!(
        send(&app, badrange).await.status(),
        StatusCode::RANGE_NOT_SATISFIABLE
    );
    let email = "export-member@example.test";
    add_member(&t, email);
    let member = sign_in_as(&app, &t, email).await;
    for request in [
        get(&format!("/api/v1/exports/{id}/download")),
        Request::delete(format!("/api/v1/exports/{id}"))
            .body(Body::empty())
            .unwrap(),
    ] {
        problem(
            send(&app, spa(&t, request, &member)).await,
            StatusCode::NOT_FOUND,
        )
        .await;
    }
    assert!(
        json(send(&app, with_session(get("/api/v1/exports"), &member)).await)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    scheduler
        .stop(tokio::time::Instant::now() + Duration::from_secs(5))
        .await;
    // Ready archives age out, with their files and rows.
    assert_eq!(exports::sweep(&t.state, now_ms() + 8 * 86_400_000).await, 1);
    assert!(!path(&t, &uid, id).exists());
}

#[tokio::test]
async fn every_route_requires_session_and_creation_requires_idempotency() {
    let t = TestState::new();
    let uid = owner(&t);
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let id = new_ulid();
    for request in [
        get("/api/v1/exports"),
        get(&format!("/api/v1/exports/{id}/download")),
        Request::post("/api/v1/exports")
            .header("idempotency-key", "a")
            .body(Body::empty())
            .unwrap(),
        Request::delete(format!("/api/v1/exports/{id}"))
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(
            send(&app, support::from_app(request)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let token = shelfy_server::admin::migrate_token::create_migrate_token(
        &t.data_dir(),
        OWNER_EMAIL,
        Duration::from_secs(3600),
    )
    .unwrap()
    .token
    .expose()
    .clone();
    for uri in [
        "/api/v1/exports".to_owned(),
        format!("/api/v1/exports/{id}/download"),
    ] {
        assert_eq!(
            send(
                &app,
                Request::get(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(
            &app,
            spa(
                &t,
                Request::post("/api/v1/exports")
                    .body(Body::empty())
                    .unwrap(),
                &cookie
            )
        )
        .await
        .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(
        t.state
            .control()
            .read(|c| rows::list(c, &uid, now_ms()))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn boot_recovers_commit_before_scheduler_admission_and_overwrites_partial() {
    for stored_state in ["queued", "running"] {
        let t = TestState::new();
        let user = owner(&t);
        populated(&t, &user).await;
        let id = new_ulid();
        let now = now_ms();
        // Deliberately commit without Jobs::admit_committed, as if the process died there.
        let payload = serde_json::to_string(&Payload {
            export_id: id.clone(),
        })
        .unwrap();
        let job=t.state.control().write(|tx| {
        tx.execute("INSERT INTO jobs(user_id,kind,dedupe_key,state,payload_json,max_attempts,run_at,created_at,updated_at) VALUES (?1,'export','export',?4,?2,2,?3,?3,?3)",params![user,payload,now,stored_state])?;
        let job=tx.last_insert_rowid();
        tx.execute("INSERT INTO exports(id,user_id,job_id,created_at,expires_at,estimated_bytes) VALUES (?1,?2,?3,?4,?5,1000000)",params![id,user,job,now,now+7*86_400_000])?;
        Ok::<_,shelfy_core::repo::RepoError>(job)
    }).unwrap();
        let dir = exports::directory(&t.state, &user);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{id}.zip.part")), b"interrupted-export").unwrap();
        let scheduler = t
            .state
            .jobs()
            .start(t.state.clone(), CancellationToken::new());
        wait(&t, &user, job, JobState::Succeeded).await;
        assert!(zip::ZipArchive::new(fs::File::open(path(&t, &user, &id)).unwrap()).is_ok());
        assert!(!dir.join(format!("{id}.zip.part")).exists());
        assert_eq!(
            t.state
                .jobs()
                .get(&user, job)
                .await
                .unwrap()
                .unwrap()
                .attempts,
            0
        );
        scheduler
            .stop(tokio::time::Instant::now() + Duration::from_secs(5))
            .await;
    }
}

#[tokio::test]
async fn deleting_queued_export_cancels_and_removes_it() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let export = json(send(&app, create(&t, &cookie, "delete-me")).await).await;
    let id = export["id"].as_str().unwrap();
    let job = export["jobId"].as_i64().unwrap();
    let response = send(
        &app,
        spa(
            &t,
            Request::delete(format!("/api/v1/exports/{id}"))
                .body(Body::empty())
                .unwrap(),
            &cookie,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        t.state.jobs().get(&user, job).await.unwrap().unwrap().state,
        JobState::Cancelled
    );
    assert!(
        t.state
            .control()
            .read(|c| rows::list(c, &user, now_ms()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        send(
            &app,
            with_session(get(&format!("/api/v1/exports/{id}/download")), &cookie)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!path(&t, &user, id).exists());
    scheduler
        .stop(tokio::time::Instant::now() + Duration::from_secs(5))
        .await;
}

#[tokio::test]
async fn disk_estimate_refuses_export_but_user_quota_does_not() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    Connection::open(t.data_dir().control_db())
        .unwrap()
        .execute("UPDATE users SET quota_bytes=1 WHERE id=?1", [&user])
        .unwrap();
    assert_eq!(
        send(&app, create(&t, &cookie, "quota-independent"))
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    let old = json(send(&app, with_session(get("/api/v1/exports"), &cookie)).await).await;
    send(
        &app,
        spa(
            &t,
            Request::delete(format!(
                "/api/v1/exports/{}",
                old[0]["id"].as_str().unwrap()
            ))
            .body(Body::empty())
            .unwrap(),
            &cookie,
        ),
    )
    .await;
    let db = t.state.user_db(&user).await.unwrap();
    db.write(|tx|tx.execute("INSERT INTO media_objects(sha256,ext,mime,bytes,role,origin,created_at) VALUES (zeroblob(32),'jpg','image/jpeg',1000000000000000,'image','upload',1)",[]).map_err(shelfy_core::repo::RepoError::from)).unwrap();
    let p = problem(
        send(&app, create(&t, &cookie, "disk-full")).await,
        StatusCode::INSUFFICIENT_STORAGE,
    )
    .await;
    assert_eq!(p.code.as_str(), "storage_full");
    assert!(
        t.state
            .control()
            .read(|c| rows::list(c, &user, now_ms()))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn corrupt_master_fails_without_publishing_and_can_be_retried() {
    let t = TestState::new();
    let user = owner(&t);
    let objects = populated(&t, &user).await;
    let object = t.data_dir().users_dir().join(&user).join(&objects[0].0);
    fs::write(&object, b"wrong-hash-same-size!").unwrap();
    let export = exports::enqueue(&t.state, &user).await.unwrap();
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    wait(&t, &user, export.job_id, JobState::Failed).await;
    assert!(!path(&t, &user, &export.id).exists());
    assert_eq!(
        t.state
            .jobs()
            .get(&user, export.job_id)
            .await
            .unwrap()
            .unwrap()
            .error_code
            .as_deref(),
        Some("validation_failed")
    );
    fs::write(&object, &objects[0].1).unwrap();
    t.state.jobs().retry(&user, export.job_id).await.unwrap();
    wait(&t, &user, export.job_id, JobState::Succeeded).await;
    scheduler
        .stop(tokio::time::Instant::now() + Duration::from_secs(5))
        .await;
}

#[tokio::test]
async fn expiry_cancels_a_never_started_export_in_a_paused_queue() {
    let t = TestState::new();
    let user = owner(&t);
    t.state.jobs().pause(&user, "export").await.unwrap();
    let export = exports::enqueue(&t.state, &user).await.unwrap();
    assert_eq!(exports::sweep(&t.state, export.expires_at).await, 1);
    assert_eq!(
        t.state
            .jobs()
            .get(&user, export.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobState::Cancelled
    );
    assert!(
        t.state
            .control()
            .read(|c| rows::list(c, &user, export.created_at))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn streaming_copy_observes_cancellation_between_chunks() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let t = TestState::new();
    let user = owner(&t);
    populated(&t, &user).await;
    let db = t.state.user_db(&user).await.unwrap();
    let snapshot = t.dir.path().join("snapshot.sqlite");
    let token = CancellationToken::new();
    db.read(|c| shelfy_server::exports::bundle::snapshot(c, &snapshot, &token, &|| {}))
        .unwrap();
    let part = t.dir.path().join("cancelled.zip.part");
    let entries = t.dir.path().join("entries.json.part");
    let checks = AtomicUsize::new(0);
    let result = shelfy_server::exports::bundle::write(
        shelfy_server::exports::bundle::Build {
            snapshot: &snapshot,
            users: &t.data_dir().users_dir(),
            user: &user,
            part: &part,
            entries_path: &entries,
            created_at: now_ms(),
        },
        &token,
        || {
            if checks.fetch_add(1, Ordering::SeqCst) == 2 {
                token.cancel();
            }
        },
    );
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert!(checks.load(Ordering::SeqCst) <= 4, "stops within a chunk");
    assert!(!t.dir.path().join("cancelled.zip").exists());
}
