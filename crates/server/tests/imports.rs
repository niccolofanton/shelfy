//! Synthetic imports through route auth, atomic admission, worker and report.
mod support;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use shelfy_server::tokens::{SecretToken, hash_token};
use shelfy_server::{
    control::uploads::{self, NewUpload, UploadMeta, UploadPurpose},
    events::model::JobState,
    ids::{new_ulid, now_ms},
    jobs::idempotency::{IDEMPOTENCY_KEY, REPLAYED},
};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use support::auth::{add_member, owner, sign_in, sign_in_as, spa, with_session};
use support::library::NOW;
use support::{TestState, get, json as response_json, post_json, send};
use tokio_util::sync::CancellationToken;

fn upload(t: &TestState, user: &str, bytes: &[u8], purpose: UploadPurpose, ext: &str) -> String {
    let id = new_ulid();
    let now = now_ms();
    let meta = UploadMeta {
        sha256: shelfy_media::Digest::of(bytes).to_string(),
        ext: Some(ext.into()),
        ..UploadMeta::default()
    };
    t.state
        .control()
        .write(|c| {
            uploads::insert(
                c,
                &NewUpload {
                    id: &id,
                    user_id: user,
                    purpose,
                    length: bytes.len() as i64,
                    meta: &meta,
                    expires_at: now + 86_400_000,
                },
                now,
            )?;
            uploads::advance(c, &id, 0, bytes.len() as i64)?;
            uploads::complete(c, &id, &meta, now)
        })
        .unwrap();
    std::fs::create_dir_all(t.data_dir().uploads_dir()).unwrap();
    std::fs::write(
        uploads::file_path(&t.data_dir().uploads_dir(), &id, true),
        bytes,
    )
    .unwrap();
    id
}
fn request(id: &str, key: &str) -> Request<Body> {
    let mut r = post_json("/api/v1/imports", json!({"uploadId":id}).to_string());
    r.headers_mut()
        .insert(IDEMPOTENCY_KEY, key.parse().unwrap());
    r
}
async fn done(t: &TestState, user: &str, id: i64) -> shelfy_server::control::jobs::JobRow {
    tokio::time::timeout(
        Duration::from_secs(15),
        t.wait_job(user, id, |r| r.state.is_final()),
    )
    .await
    .unwrap()
}
fn fixture() -> &'static [u8] {
    include_bytes!("../../core/tests/fixtures/import/desktop.json")
}

#[tokio::test]
async fn route_auth_idempotency_claim_and_user_isolation() {
    let t = TestState::new();
    let app = t.app();
    let user = owner(&t);
    let cookie = sign_in(&app, &t).await;
    let id = upload(&t, &user, fixture(), UploadPurpose::IMPORT, "json");
    assert_eq!(
        send(&app, request(&id, "anonymous")).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, get("/api/v1/imports/1")).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let mut csrf = request(&id, "csrf");
    csrf.headers_mut().remove("x-shelfy-client");
    assert_eq!(
        send(&app, with_session(csrf, &cookie)).await.status(),
        StatusCode::FORBIDDEN
    );
    let mut missing = post_json("/api/v1/imports", json!({"uploadId":id}).to_string());
    missing = spa(&t, missing, &cookie);
    assert_eq!(send(&app, missing).await.status(), StatusCode::BAD_REQUEST);
    let accepted = send(&app, spa(&t, request(&id, "one"), &cookie)).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let location = accepted.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let first: Value = response_json(accepted).await;
    let replay = send(&app, spa(&t, request(&id, "one"), &cookie)).await;
    assert_eq!(replay.headers()[REPLAYED], "true");
    assert_eq!(response_json(replay).await, first);
    let again = send(&app, spa(&t, request(&id, "two"), &cookie)).await;
    assert_eq!(again.status(), StatusCode::CONFLICT);
    assert_eq!(response_json(again).await["code"], "upload_consumed");
    let other = add_member(&t, "other@example.test");
    let other_cookie = sign_in_as(&app, &t, "other@example.test").await;
    assert_eq!(
        send(&app, with_session(get(&location), &other_cookie))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let other_upload = upload(&t, &other, fixture(), UploadPurpose::IMPORT, "json");
    assert_eq!(
        send(&app, spa(&t, request(&other_upload, "foreign"), &cookie))
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let token = SecretToken::generate();
    let raw = format!("shx_{}", token.expose());
    let hash = hash_token(&raw);
    t.control().execute("INSERT INTO api_tokens(id,user_id,kind,token_hash,scopes,created_at) VALUES(?1,?2,'extension',?3,'uploads',?4)",rusqlite::params![new_ulid(),user,hash,now_ms()]).unwrap();
    let mut bearer = request(&id, "token");
    bearer.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {raw}").parse().unwrap(),
    );
    assert_eq!(send(&app, bearer).await.status(), StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn claims_roll_back_on_wrong_purpose_missing_bytes_or_failed_job_insert() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app_as(&user);
    let wrong = upload(
        &t,
        &user,
        fixture(),
        UploadPurpose::BOOKMARK_ORIGINAL,
        "jpg",
    );
    assert_eq!(
        send(&app, request(&wrong, "wrong")).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let missing = upload(&t, &user, fixture(), UploadPurpose::IMPORT, "json");
    std::fs::remove_file(uploads::file_path(
        &t.data_dir().uploads_dir(),
        &missing,
        true,
    ))
    .unwrap();
    assert_eq!(
        send(&app, request(&missing, "missing")).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(
        !t.state
            .control()
            .read(|c| uploads::get(c, &user, &missing))
            .unwrap()
            .unwrap()
            .is_consumed()
    );
    let failed = upload(&t, &user, fixture(), UploadPurpose::IMPORT, "json");
    t.control().execute_batch("CREATE TRIGGER no_import_job BEFORE INSERT ON jobs WHEN NEW.kind='import' BEGIN SELECT RAISE(ABORT,'synthetic insertion failure'); END").unwrap();
    assert_eq!(
        send(&app, request(&failed, "failed")).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(
        !t.state
            .control()
            .read(|c| uploads::get(c, &user, &failed))
            .unwrap()
            .unwrap()
            .is_consumed()
    );
}
#[tokio::test]
async fn imports_complete_report_once_and_do_not_schedule_remote_media() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app_as(&user);
    let mut events = t.state.events().subscribe(&user, None);
    let id = upload(&t, &user, fixture(), UploadPurpose::IMPORT, "json");
    let accepted: Value = response_json(send(&app, request(&id, "fixture")).await).await;
    let job = accepted["job"]["id"].as_i64().unwrap();
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let row = done(&t, &user, job).await;
    assert_eq!(row.state, JobState::Succeeded, "{:?}", row.error_code);
    let report = response_json(send(&app, get(&format!("/api/v1/imports/{job}"))).await).await;
    support::sse::assert_schema(&report, "Import");
    assert_eq!(report["report"]["imported"], 5);
    assert_eq!(report["report"]["collections"], 2);
    assert_eq!(report["report"]["links"], 2);
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut posts = false;
        let mut stats = false;
        let mut notice = false;
        while !(posts && stats && notice) {
            if let shelfy_server::events::Delivery::Event(event) = events.next().await {
                let data: Value = serde_json::from_str(&event.data).unwrap();
                match event.topic {
                    shelfy_server::events::model::EventTopic::PostsChanged => {
                        assert_eq!(data["reason"], "import");
                        posts = true;
                    }
                    shelfy_server::events::model::EventTopic::StatsChanged => stats = true,
                    shelfy_server::events::model::EventTopic::Notification => {
                        assert_eq!(data["code"], "import.done");
                        notice = true;
                    }
                    _ => {}
                }
            }
        }
    })
    .await
    .unwrap();
    let db = t.state.user_db(&user).await.unwrap();
    db.read(|c| -> Result<(), shelfy_core::repo::RepoError> {
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM notifications WHERE code='import.done'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            1
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM web_captures", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM media_objects", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();
    let id = upload(&t, &user, fixture(), UploadPurpose::IMPORT, "json");
    let again: Value = response_json(send(&app, request(&id, "reimport")).await).await;
    let job = again["job"]["id"].as_i64().unwrap();
    assert_eq!(done(&t, &user, job).await.state, JobState::Succeeded);
    let report = response_json(send(&app, get(&format!("/api/v1/imports/{job}"))).await).await;
    assert_eq!(report["report"]["imported"], 0);
    assert_eq!(report["report"]["updated"], 0);
    assert_eq!(report["report"]["skipped"], 5);
    assert_eq!(report["report"]["links"], 0);
    assert!(
        scheduler
            .stop(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
    );
}
#[tokio::test]
async fn unsupported_format_fails_permanently_without_library_mutation() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app_as(&user);
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    for (n, bytes, ext) in [
        ("object", b"{\"unexpected\":[]}".as_slice(), "json"),
        ("zip", b"PK\x03\x04synthetic".as_slice(), "zip"),
        ("invalid", b"[{\"foo\":1}]".as_slice(), "json"),
        (
            "trailing",
            b"[{\"platform\":\"twitter\",\"id\":\"1\"}]extra".as_slice(),
            "json",
        ),
    ] {
        let id = upload(&t, &user, bytes, UploadPurpose::IMPORT, ext);
        let response: Value = response_json(send(&app, request(&id, n)).await).await;
        let job = response["job"]["id"].as_i64().unwrap();
        let row = done(&t, &user, job).await;
        assert_eq!(row.state, JobState::Failed);
        assert_eq!(row.error_code.as_deref(), Some("import_format_unknown"));
        assert_eq!(row.attempts, 1);
    }
    let db = t.state.user_db(&user).await.unwrap();
    assert_eq!(
        db.read(|c| c
            .query_row("SELECT count(*) FROM posts", [], |r| r.get::<_, i64>(0))
            .map_err(shelfy_core::repo::RepoError::from))
            .unwrap(),
        0
    );
    assert!(
        scheduler
            .stop(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
    );
}
#[tokio::test]
async fn quota_failure_rolls_back_the_whole_batch_and_keeps_partial_report() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app_as(&user);
    let db = t.state.user_db(&user).await.unwrap();
    let baseline=db.read(|c|c.query_row("SELECT (SELECT page_count FROM pragma_page_count)*(SELECT page_size FROM pragma_page_size)",[],|r|r.get::<_,i64>(0)).map_err(shelfy_core::repo::RepoError::from)).unwrap();
    t.control()
        .execute(
            "UPDATE users SET quota_bytes=?1 WHERE id=?2",
            rusqlite::params![baseline + 4096, user],
        )
        .unwrap();
    let bytes =
        serde_json::to_vec(&json!([{"id":"1","platform":"twitter","text":"x".repeat(20_000)}]))
            .unwrap();
    let id = upload(&t, &user, &bytes, UploadPurpose::IMPORT, "json");
    let accepted: Value = response_json(send(&app, request(&id, "quota")).await).await;
    let job = accepted["job"]["id"].as_i64().unwrap();
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let row = done(&t, &user, job).await;
    assert_eq!(row.state, JobState::Failed);
    assert_eq!(row.error_code.as_deref(), Some("quota_exceeded"));
    assert_eq!(
        db.read(|c| c
            .query_row("SELECT count(*) FROM posts", [], |r| r.get::<_, i64>(0))
            .map_err(shelfy_core::repo::RepoError::from))
            .unwrap(),
        0
    );
    assert!(
        scheduler
            .stop(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
    );
}
#[tokio::test]
async fn cancellation_between_batches_preserves_committed_report_and_retry_resumes() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app_as(&user);
    let db = t.state.user_db(&user).await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    db.write({let entered=Arc::clone(&entered);let gate=Arc::clone(&gate);move|c|->Result<(),shelfy_core::repo::RepoError>{
        c.create_scalar_function("import_test_gate",0,rusqlite::functions::FunctionFlags::SQLITE_UTF8,move|_|{
            entered.notify_one();let (lock,cv)=&*gate;let mut released=lock.lock().unwrap();while !*released {released=cv.wait(released).unwrap();}Ok(1)
        })?;
        c.execute_batch("CREATE TRIGGER import_gate BEFORE INSERT ON posts WHEN (SELECT count(*) FROM posts)=500 BEGIN SELECT import_test_gate(); END")?;Ok(())
    }}).unwrap();
    let bytes = serde_json::to_vec(
        &(1..=2000)
            .map(|i| json!({"id":i.to_string(),"platform":"twitter","text":"Synthetic"}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let upload = upload(&t, &user, &bytes, UploadPurpose::IMPORT, "json");
    let accepted: Value = response_json(send(&app, request(&upload, "cancel")).await).await;
    let id = accepted["job"]["id"].as_i64().unwrap();
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    t.state.jobs().cancel(&user, id).await.unwrap();
    let incarnation = t
        .state
        .jobs()
        .get(&user, id)
        .await
        .unwrap()
        .unwrap()
        .incarnation;
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    let barrier = Arc::clone(&db);
    tokio::task::spawn_blocking(move || {
        barrier.write(|c| {
            c.execute_batch("DROP TRIGGER import_gate")
                .map_err(shelfy_core::repo::RepoError::from)
        })
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        db.read(|c| shelfy_server::imports::checkpoint(c, id))
            .unwrap()
            .incarnation
            .as_deref(),
        Some(incarnation.as_str())
    );
    let report = response_json(send(&app, get(&format!("/api/v1/imports/{id}"))).await).await;
    support::sse::assert_schema(&report, "Import");
    assert_eq!(report["job"]["state"], "cancelled");
    assert_eq!(report["report"]["imported"], 1000);
    assert_eq!(
        db.read(|c| c
            .query_row("SELECT count(*) FROM posts", [], |r| r.get::<_, i64>(0))
            .map_err(shelfy_core::repo::RepoError::from))
            .unwrap(),
        1000
    );
    t.state.jobs().retry(&user, id).await.unwrap();
    assert_eq!(done(&t, &user, id).await.state, JobState::Succeeded);
    let report = response_json(send(&app, get(&format!("/api/v1/imports/{id}"))).await).await;
    assert_eq!(report["report"]["imported"], 2000);
    assert_eq!(report["report"]["skipped"], 0);
    assert_eq!(
        db.read(|c| c
            .query_row(
                "SELECT count(*) FROM notifications WHERE code='import.done'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .map_err(shelfy_core::repo::RepoError::from))
            .unwrap(),
        1
    );
    assert!(
        scheduler
            .stop(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
    );
}

#[tokio::test]
async fn competing_requests_claim_once_and_changed_fingerprint_keeps_new_upload() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app_as(&user);
    let id = upload(&t, &user, fixture(), UploadPurpose::IMPORT, "json");
    let (a, b) = tokio::join!(
        send(&app, request(&id, "race-a")),
        send(&app, request(&id, "race-b"))
    );
    let statuses = [a.status(), b.status()];
    assert!(statuses.contains(&StatusCode::ACCEPTED));
    assert!(statuses.contains(&StatusCode::CONFLICT));
    let count: i64 = t
        .control()
        .query_row("SELECT count(*) FROM jobs WHERE kind='import'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let new = upload(&t, &user, fixture(), UploadPurpose::IMPORT, "json");
    assert_eq!(
        send(&app, request(&new, "race-a")).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(
        !t.state
            .control()
            .read(|c| uploads::get(c, &user, &new))
            .unwrap()
            .unwrap()
            .is_consumed()
    );
    assert_eq!(
        send(&app, request("../escape", "bad-input")).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn legacy_unbound_partial_import_fails_without_writing_and_explains_reupload() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app_as(&user);
    let bytes=br#"[{"id":"1","platform":"twitter","text":"Synthetic one"},{"id":"2","platform":"twitter","text":"Synthetic two"}]"#;
    let source = upload(&t, &user, bytes, UploadPurpose::IMPORT, "json");
    let accepted: Value = response_json(send(&app, request(&source, "legacy-partial")).await).await;
    let id = accepted["job"]["id"].as_i64().unwrap();
    t.control()
        .execute(
            "UPDATE jobs SET incarnation='legacy:00000000000000000000000000000000' WHERE id=?1",
            [id],
        )
        .unwrap();
    let db = t.state.user_db(&user).await.unwrap();
    db.write(|tx| {
        shelfy_server::imports::save(
            tx,
            id,
            &shelfy_server::imports::Checkpoint {
                next: 1,
                definitions_done: true,
                ..Default::default()
            },
        )?;
        shelfy_core::repo::posts::insert(
            tx,
            &shelfy_core::repo::posts::NewPost::new(
                "x_1",
                shelfy_core::repo::Platform::Twitter,
                "1",
                "text",
                NOW,
            ),
            NOW,
        )?;
        Ok::<_, shelfy_core::repo::RepoError>(())
    })
    .unwrap();
    let before = db.generation();
    let marker: String = db
        .read(|c| {
            Ok::<_, shelfy_core::repo::RepoError>(c.query_row(
                "SELECT value FROM meta WHERE key=?1",
                [shelfy_server::imports::report_key(id)],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let row = done(&t, &user, id).await;
    assert_eq!(row.state, JobState::Failed);
    assert_eq!(row.error_code.as_deref(), Some("import_checkpoint_unbound"));
    assert!(
        row.error_detail
            .as_deref()
            .unwrap()
            .contains("upload the file again")
    );
    assert_eq!(before, db.generation());
    let after: String = db
        .read(|c| {
            Ok::<_, shelfy_core::repo::RepoError>(c.query_row(
                "SELECT value FROM meta WHERE key=?1",
                [shelfy_server::imports::report_key(id)],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(marker, after);
    assert_eq!(
        db.read(|c| Ok::<_, shelfy_core::repo::RepoError>(c.query_row(
            "SELECT count(*) FROM posts",
            [],
            |r| r.get::<_, i64>(0)
        )?))
        .unwrap(),
        1
    );
    let response = send(&app, get(&format!("/api/v1/imports/{id}"))).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(response).await["code"],
        "import_checkpoint_unbound"
    );
    // Follow the recovery action with a fresh claim/job: merge the prior partial
    // metadata by stable post key rather than create a second copy.
    let retry_source = upload(&t, &user, bytes, UploadPurpose::IMPORT, "json");
    let accepted: Value =
        response_json(send(&app, request(&retry_source, "legacy-reupload")).await).await;
    let retry_id = accepted["job"]["id"].as_i64().unwrap();
    assert!(retry_id > id);
    assert_eq!(done(&t, &user, retry_id).await.state, JobState::Succeeded);
    assert_eq!(
        db.read(|c| Ok::<_, shelfy_core::repo::RepoError>(c.query_row(
            "SELECT count(*) FROM posts",
            [],
            |r| r.get::<_, i64>(0)
        )?))
        .unwrap(),
        2
    );
    assert_eq!(
        db.read(|c| Ok::<_, shelfy_core::repo::RepoError>(c.query_row(
            "SELECT count(*) FROM notifications WHERE code='import.done'",
            [],
            |r| r.get::<_, i64>(0)
        )?))
        .unwrap(),
        1
    );
    assert!(
        scheduler
            .stop(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
    );
}

#[tokio::test]
async fn a_new_import_ignores_an_old_finished_marker_instead_of_skipping_its_file() {
    let t = TestState::new();
    let user = owner(&t);
    let app = t.app_as(&user);
    let bytes = br#"[{"id":"91","platform":"twitter","text":"New synthetic input"}]"#;
    let source = upload(&t, &user, bytes, UploadPurpose::IMPORT, "json");
    let accepted: Value =
        response_json(send(&app, request(&source, "fresh-after-orphan")).await).await;
    let id = accepted["job"]["id"].as_i64().unwrap();
    let db = t.state.user_db(&user).await.unwrap();
    db.write(|tx| {
        shelfy_server::imports::save(
            tx,
            id,
            &shelfy_server::imports::Checkpoint {
                complete: true,
                next: 999,
                ..Default::default()
            },
        )
    })
    .unwrap();
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    let row = done(&t, &user, id).await;
    assert_eq!(row.state, JobState::Succeeded);
    let marker = db
        .read(|c| shelfy_server::imports::checkpoint(c, id))
        .unwrap();
    assert!(marker.complete);
    assert_eq!(marker.report.imported, 1);
    assert_eq!(marker.next, 1);
    assert_eq!(
        marker.incarnation.as_deref(),
        Some(row.incarnation.as_str())
    );
    assert_eq!(marker.upload_id.as_deref(), Some(source.as_str()));
    assert!(
        scheduler
            .stop(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
    );
}
