//! Capture service fixtures; no Chromium, external network, or AI provider.
mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde_json::{Value, json};
use shelfy_core::repo::RepoError;
use shelfy_server::outbound::Origin;
use shelfy_server::{
    capture::{self, Options},
    events::model::JobState,
    jobs::{Clock, Registry},
};
use support::auth::{owner, sign_in, spa};
use support::{TestState, json as response_json, post_json, send};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
enum Mode {
    Recorded,
    Symlink,
    Hardlink,
    Traversal,
    Oversize,
    Polyglot,
    BadManifest,
    UnknownEvent,
    NestedParams,
    TooManyEvents,
    TooManyLines,
    OversizeLine,
    Blocked,
    NoOg,
    Busy,
    Hang,
    DisconnectDone,
}
#[derive(Clone)]
struct Fake {
    root: Arc<Mutex<PathBuf>>,
    mode: Arc<Mutex<Mode>>,
    calls: Arc<AtomicUsize>,
    ids: Arc<Mutex<Vec<String>>>,
}
struct Harness {
    t: TestState,
    fake: Fake,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Harness {
    async fn new(mode: Mode) -> Self {
        let fake = Fake {
            root: Arc::new(Mutex::new(PathBuf::new())),
            mode: Arc::new(Mutex::new(mode)),
            calls: Arc::default(),
            ids: Arc::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/health", get(health))
            .route("/v1/captures", post(capture))
            .with_state(fake.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let t = TestState::with_config(|c| {
            c.outbound.capture = Some(Origin::parse(&format!("http://{addr}")).unwrap());
            c.capture.internal_token = Some(secrecy::SecretString::from("fixture-internal"));
            let registry = Registry::new().register(shelfy_server::jobs::capture::kind(1));
            c.jobs.registry = registry;
            c.jobs.clock = Clock::System;
        });
        *fake.root.lock().unwrap() = capture::work_root(&t.state);
        Self { t, fake, server }
    }
    async fn queue(&self) -> (String, i64, String) {
        let app = self.t.app();
        let cookie = sign_in(&app, &self.t).await;
        let response=send(&app,spa(&self.t,post_json("/api/v1/sites",json!({"url":"https://fixtures.shelfy.test/native-tall.html","singlePage":true}).to_string()),&cookie)).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let v = response_json(response).await;
        (
            v["key"].as_str().unwrap().into(),
            v["job"]["id"].as_i64().unwrap(),
            cookie,
        )
    }
    async fn terminal(&self, user: &str, id: i64) -> JobState {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let row = self.t.job(user, id).await;
                if matches!(
                    row.state,
                    JobState::Succeeded | JobState::Failed | JobState::Cancelled
                ) {
                    return row.state;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "capture did not terminate: {:?}; service calls {}",
                self.t
                    .state
                    .control()
                    .read(|c| shelfy_server::control::jobs::get(c, user, id))
                    .unwrap(),
                self.fake.calls.load(Ordering::SeqCst)
            )
        })
    }
    fn mode(&self, m: Mode) {
        *self.fake.mode.lock().unwrap() = m;
    }
}
async fn health(State(f): State<Fake>) -> Json<Value> {
    Json(json!({"ok":true,"slotsFree":if matches!(*f.mode.lock().unwrap(),Mode::Busy){0}else{1}}))
}
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../capture/fixtures/recorded/basic")
}
async fn capture(
    State(f): State<Fake>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> (HeaderMap, Body) {
    assert_eq!(headers["x-shelfy-internal-token"], "fixture-internal");
    let id = req["captureId"].as_str().unwrap();
    assert_eq!(req["workDir"], format!("/work/{id}"));
    f.calls.fetch_add(1, Ordering::SeqCst);
    f.ids.lock().unwrap().push(id.into());
    let mode = *f.mode.lock().unwrap();
    let root = f.root.lock().unwrap().clone();
    let dir = root.join(id);
    assert!(dir.is_dir());
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(fixture().join("manifest.json")).unwrap()).unwrap();
    manifest["url"] = req["url"].clone();
    let mut file = String::new();
    for page in manifest["pages"].as_array().unwrap() {
        for asset in page["assets"].as_array().unwrap() {
            let name = asset["file"].as_str().unwrap();
            std::fs::copy(fixture().join(name), dir.join(name)).unwrap();
            if file.is_empty() {
                file = name.into();
            }
        }
    }
    for field in ["og", "favicon"] {
        if let Some(name) = manifest[field]["file"].as_str() {
            std::fs::copy(fixture().join(name), dir.join(name)).unwrap();
        }
    }
    let first = &mut manifest["pages"][0]["assets"][0];
    match mode {
        Mode::Symlink => {
            std::fs::remove_file(dir.join(&file)).unwrap();
            std::os::unix::fs::symlink(fixture().join(&file), dir.join(&file)).unwrap();
        }
        Mode::Hardlink => {
            std::fs::hard_link(dir.join(&file), dir.join("another.webp")).unwrap();
        }
        Mode::Traversal => first["file"] = json!("../outside.webp"),
        Mode::Oversize => {
            std::fs::OpenOptions::new()
                .write(true)
                .open(dir.join(&file))
                .unwrap()
                .set_len(capture::protocol::IMAGE_BYTES + 1)
                .unwrap();
        }
        Mode::Polyglot => {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(dir.join(&file))
                .unwrap()
                .write_all(b"PK\x03\x04private-payload")
                .unwrap();
        }
        Mode::BadManifest => manifest["pages"][0]["qc"] = Value::Null,
        Mode::Blocked => {
            std::fs::copy(dir.join(&file), dir.join("blocked-og.webp")).unwrap();
            manifest["pages"] = json!([]);
            manifest["og"] = json!({"file":"blocked-og.webp","w":1440,"h":900});
            manifest["cover"] = json!({"role":"og"});
            manifest["partial"] = json!(true);
        }
        _ => {}
    }
    if !matches!(mode, Mode::NoOg | Mode::Hang) {
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }
    let line = if matches!(mode, Mode::Blocked | Mode::NoOg) {
        json!({"type":"failed","code":"capture_blocked"})
    } else {
        json!({"type":"done","manifest":"manifest.json","durationMs":12,"peakRssBytes":1234,"bytes":83378})
    };
    let bytes = match mode {
        Mode::UnknownEvent=>format!("{}\n",json!({"type":"event","kind":"info","code":"untrusted_code"})),
        Mode::NestedParams=>format!("{}\n",json!({"type":"event","kind":"read","code":"site.opening","params":{"url":{"nested":true}}})),
        Mode::TooManyEvents=>format!("{}\n",json!({"type":"event","kind":"read","code":"site.opening","params":{"url":"https://example.test/"}})).repeat(251),
        Mode::TooManyLines=>format!("{}\n",json!({"type":"page","index":0,"url":"https://example.test/","pageType":"home","assets":[]})).repeat(401),
        Mode::OversizeLine=>" ".repeat(capture::protocol::LINE_BYTES+1)+"\n",
        _=>format!("{line}\n"),
    };
    let body = match mode {
        Mode::Hang => Body::from_stream(futures_util::stream::pending::<
            Result<String, std::io::Error>,
        >()),
        Mode::DisconnectDone => Body::from_stream(futures_util::stream::unfold(0, move |n| {
            let bytes = bytes.clone();
            async move {
                match n {
                    0 => Some((Ok(bytes), 1)),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Some((Err(std::io::Error::other("fixture disconnect")), 2))
                    }
                    _ => None,
                }
            }
        })),
        _ => Body::from(bytes),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/x-ndjson".parse().unwrap(),
    );
    (headers, body)
}
async fn counts(t: &TestState, user: &str) -> (i64, i64, i64) {
    let db = t.state.user_db(user).await.unwrap();
    db.read(|c| {
        Ok::<_, RepoError>((
            c.query_row("SELECT count(*) FROM web_captures", [], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM media_objects", [], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))?,
        ))
    })
    .unwrap()
}
async fn stop(s: shelfy_server::jobs::Scheduler) {
    assert!(
        s.stop(tokio::time::Instant::now() + Duration::from_secs(3))
            .await
    );
}

#[tokio::test]
async fn recorded_ingest_dedupe_recapture_and_receipt() {
    let h = Harness::new(Mode::Recorded).await;
    let (key, id, cookie) = h.queue().await;
    let user = owner(&h.t);
    let duplicate = capture::enqueue_site(
        &h.t.state,
        &user,
        "http://fixtures.shelfy.test/native-tall.html",
        Options::default(),
    )
    .await
    .unwrap();
    assert_eq!(duplicate.1.job.id, id);
    let db = h.t.state.user_db(&user).await.unwrap();
    db.write(|c| {
        c.execute(
            "UPDATE posts SET user_note='manual',user_tags_json='[\"keep\"]' WHERE key=?1",
            [&key],
        )
        .map(|_| ())
        .map_err(RepoError::from)
    })
    .unwrap();
    let scheduler =
        h.t.state
            .jobs()
            .start(h.t.state.clone(), CancellationToken::new());
    assert_eq!(h.terminal(&user, id).await, JobState::Succeeded);
    assert_eq!(counts(&h.t, &user).await.0, 1);
    let assets: i64 = db
        .read(|c| {
            c.query_row("SELECT count(*) FROM web_capture_assets", [], |r| r.get(0))
                .map_err(RepoError::from)
        })
        .unwrap();
    assert_eq!(assets, 10);
    let post:(String,String,bool,bool)=db.read(|c|c.query_row("SELECT user_note,user_tags_json,cover_object IS NOT NULL,thumbhash IS NOT NULL FROM posts WHERE key=?1",[&key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(RepoError::from)).unwrap();
    assert_eq!(post, ("manual".into(), "[\"keep\"]".into(), true, true));
    let response = send(
        &h.t.app(),
        spa(
            &h.t,
            post_json(&format!("/api/v1/sites/{key}/recapture"), "{}"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let v = response_json(response).await;
    let next = v["job"]["id"].as_i64().unwrap();
    assert_eq!(h.terminal(&user, next).await, JobState::Succeeded);
    assert_eq!(counts(&h.t, &user).await.0, 2);
    let row = h.t.job(&user, next).await;
    assert!(
        serde_json::from_str::<Value>(&row.payload_json).unwrap()["options"]["singlePage"]
            .as_bool()
            .unwrap()
    );
    let daily = h
        .t
        .state
        .control()
        .read(|c| {
            shelfy_server::control::usage_daily::of_day(c, &user, h.t.state.jobs().clock().now_ms())
        })
        .unwrap();
    assert_eq!(daily.captures, 2);
    assert_eq!(h.t.state.quota().reserved_total(), 0);
    assert_eq!(
        std::fs::read_dir(capture::work_root(&h.t.state))
            .unwrap()
            .count(),
        0
    );
    stop(scheduler).await;
}

#[tokio::test]
async fn hostile_artifacts_never_publish_or_charge() {
    for mode in [
        Mode::Symlink,
        Mode::Hardlink,
        Mode::Traversal,
        Mode::Oversize,
        Mode::Polyglot,
        Mode::BadManifest,
        Mode::UnknownEvent,
        Mode::NestedParams,
        Mode::TooManyEvents,
        Mode::TooManyLines,
        Mode::OversizeLine,
    ] {
        let h = Harness::new(mode).await;
        let (_, id, _) = h.queue().await;
        let user = owner(&h.t);
        let scheduler =
            h.t.state
                .jobs()
                .start(h.t.state.clone(), CancellationToken::new());
        assert_eq!(h.terminal(&user, id).await, JobState::Failed, "{mode:?}");
        assert_eq!(counts(&h.t, &user).await, (0, 0, 1), "{mode:?}");
        assert_eq!(h.t.state.quota().reserved_total(), 0);
        assert_eq!(
            std::fs::read_dir(capture::work_root(&h.t.state))
                .unwrap()
                .count(),
            0
        );
        stop(scheduler).await;
    }
}

#[tokio::test]
async fn blocked_fallback_and_no_og() {
    for mode in [Mode::Blocked, Mode::NoOg] {
        let h = Harness::new(mode).await;
        let (_, id, _) = h.queue().await;
        let user = owner(&h.t);
        let scheduler =
            h.t.state
                .jobs()
                .start(h.t.state.clone(), CancellationToken::new());
        let result = h.terminal(&user, id).await;
        if matches!(mode, Mode::Blocked) {
            assert_eq!(result, JobState::Succeeded);
            let db = h.t.state.user_db(&user).await.unwrap();
            let status: String = db
                .read(|c| {
                    c.query_row("SELECT status FROM web_captures", [], |r| r.get(0))
                        .map_err(RepoError::from)
                })
                .unwrap();
            assert_eq!(status, "blocked");
        } else {
            assert_eq!(result, JobState::Failed);
            assert_eq!(counts(&h.t, &user).await, (0, 0, 1));
        }
        stop(scheduler).await;
    }
}

#[tokio::test]
async fn broken_stream_after_done_recovers_disk_manifest() {
    let h = Harness::new(Mode::DisconnectDone).await;
    let (_, id, _) = h.queue().await;
    let user = owner(&h.t);
    let scheduler =
        h.t.state
            .jobs()
            .start(h.t.state.clone(), CancellationToken::new());
    assert_eq!(h.terminal(&user, id).await, JobState::Succeeded);
    assert_eq!(h.fake.calls.load(Ordering::SeqCst), 1);
    stop(scheduler).await;
}

#[tokio::test]
async fn cancel_first_preserves_edits_and_retry_repairs_removed_placeholder() {
    let h = Harness::new(Mode::Recorded).await;
    let (key, id, cookie) = h.queue().await;
    let user = owner(&h.t);
    let response = send(
        &h.t.app(),
        spa(
            &h.t,
            post_json(&format!("/api/v1/jobs/{id}/cancel"), "{}"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(counts(&h.t, &user).await, (0, 0, 0));
    h.t.state.jobs().retry(&user, id).await.unwrap();
    let scheduler =
        h.t.state
            .jobs()
            .start(h.t.state.clone(), CancellationToken::new());
    assert_eq!(h.terminal(&user, id).await, JobState::Succeeded);
    assert_eq!(counts(&h.t, &user).await.2, 1);
    stop(scheduler).await;
    let queued = capture::enqueue_site(
        &h.t.state,
        &user,
        "https://other.shelfy.test/",
        Options::default(),
    )
    .await
    .unwrap();
    let db = h.t.state.user_db(&user).await.unwrap();
    db.write(|c| {
        c.execute("UPDATE posts SET user_note='keep' WHERE key=?1", [queued.0])
            .map(|_| ())
            .map_err(RepoError::from)
    })
    .unwrap();
    let response = send(
        &h.t.app(),
        spa(
            &h.t,
            post_json(&format!("/api/v1/jobs/{}/cancel", queued.1.job.id), "{}"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(counts(&h.t, &user).await.2, 2);
    assert!(key.starts_with("web_"));
}

#[tokio::test]
async fn limits_off_url_authz_and_busy_zero_tries() {
    let h = Harness::new(Mode::Busy).await;
    let user = owner(&h.t);
    for url in [
        "http://127.0.0.1/",
        "http://[::1]/",
        "http://localhost/",
        "ftp://example.test/",
        "https://user:pass@example.test/",
    ] {
        assert!(
            capture::enqueue_site(&h.t.state, &user, url, Options::default())
                .await
                .is_err()
        );
    }
    h.t.state
        .control()
        .write(|c| shelfy_server::control::users::set_limits(c, &user, None, Some(1)))
        .unwrap();
    let (_, id, cookie) = h.queue().await;
    let error = capture::enqueue_site(
        &h.t.state,
        &user,
        "https://other.shelfy.test/",
        Options::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.code(),
        shelfy_server::error::ErrorCode::CaptureDailyLimit
    );
    assert_eq!(counts(&h.t, &user).await.2, 1);
    let missing = send(
        &h.t.app(),
        spa(
            &h.t,
            post_json("/api/v1/sites/web:other/recapture", "{}"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let scheduler =
        h.t.state
            .jobs()
            .start(h.t.state.clone(), CancellationToken::new());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = h.t.job(&user, id).await;
            if row.stage.as_deref() == Some("waiting_capture") {
                assert_eq!(row.attempts, 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(h.fake.calls.load(Ordering::SeqCst), 0);
    stop(scheduler).await;
    let off = TestState::new();
    let offuser = owner(&off);
    assert_eq!(
        capture::enqueue_site(
            &off.state,
            &offuser,
            "https://example.test/",
            Options::default()
        )
        .await
        .unwrap_err()
        .code(),
        shelfy_server::error::ErrorCode::CaptureUnavailable
    );
}

#[tokio::test]
async fn shutdown_cleans_work_and_restart_uses_fresh_id() {
    let h = Harness::new(Mode::Hang).await;
    let (_, id, _) = h.queue().await;
    let user = owner(&h.t);
    let scheduler =
        h.t.state
            .jobs()
            .start(h.t.state.clone(), CancellationToken::new());
    tokio::time::timeout(Duration::from_secs(5), async {
        while h.fake.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop(scheduler).await;
    assert_eq!(
        std::fs::read_dir(capture::work_root(&h.t.state))
            .unwrap()
            .count(),
        0
    );
    h.mode(Mode::Recorded);
    let state = shelfy_server::state::AppState::open(h.t.state.config().clone()).unwrap();
    let scheduler = state.jobs().start(state.clone(), CancellationToken::new());
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let row = state.jobs().get(&user, id).await.unwrap().unwrap();
            if row.state == JobState::Succeeded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let ids = h.fake.ids.lock().unwrap().clone();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    stop(scheduler).await;
}

#[tokio::test]
async fn owner_isolation_quota_and_health_status_only() {
    let h = Harness::new(Mode::Recorded).await;
    let (key, id, _) = h.queue().await;
    let user = owner(&h.t);
    let other = "01JOTHERUSER000000000000000";
    h.t.add_user(other);
    let response = send(
        &h.t.app_as(other),
        support::from_app(post_json(&format!("/api/v1/sites/{key}/recapture"), "{}")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(counts(&h.t, other).await, (0, 0, 0));
    h.t.state
        .control()
        .write(|c| shelfy_server::control::users::set_limits(c, &user, Some(1), None))
        .unwrap();
    let scheduler =
        h.t.state
            .jobs()
            .start(h.t.state.clone(), CancellationToken::new());
    assert_eq!(h.terminal(&user, id).await, JobState::Failed);
    assert_eq!(h.fake.calls.load(Ordering::SeqCst), 0);
    assert_eq!(counts(&h.t, &user).await, (0, 0, 1));
    stop(scheduler).await;
    let response = send(&h.t.app(), support::get("/health/capture")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(support::body(response).await.is_empty());
    let off = TestState::new();
    let response = send(&off.app(), support::get("/health/capture")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(support::body(response).await.is_empty());
}

#[tokio::test]
async fn recovery_receipt_does_not_recapture_or_double_count() {
    let h = Harness::new(Mode::Recorded).await;
    let (_, id, _) = h.queue().await;
    let user = owner(&h.t);
    let scheduler =
        h.t.state
            .jobs()
            .start(h.t.state.clone(), CancellationToken::new());
    assert_eq!(h.terminal(&user, id).await, JobState::Succeeded);
    stop(scheduler).await;
    // Simulate the crash boundary after the library committed but before control
    // recorded the successful daily charge. Replaying the durable job repairs it.
    h.t.state.control().write(|c|{c.execute("DELETE FROM usage_daily WHERE user_id=?1",[&user])?;c.execute("UPDATE jobs SET state='failed',payload_json=json_remove(payload_json,'$.captureCounted') WHERE id=?1",[id])?;Ok::<_,RepoError>(())}).unwrap();
    let state = shelfy_server::state::AppState::open(h.t.state.config().clone()).unwrap();
    state.jobs().retry(&user, id).await.unwrap();
    let scheduler = state.jobs().start(state.clone(), CancellationToken::new());
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if state.jobs().get(&user, id).await.unwrap().unwrap().state == JobState::Succeeded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(h.fake.calls.load(Ordering::SeqCst), 1);
    assert_eq!(counts(&h.t, &user).await.0, 1);
    assert_eq!(
        state
            .control()
            .read(|c| shelfy_server::control::usage_daily::of_day(
                c,
                &user,
                state.jobs().clock().now_ms()
            ))
            .unwrap()
            .captures,
        1
    );
    stop(scheduler).await;
}

#[tokio::test]
async fn sweep_removes_old_queued_orphans_but_keeps_running_and_links() {
    let h = Harness::new(Mode::Recorded).await;
    let (_, job, _) = h.queue().await;
    let root = capture::work_root(&h.t.state);
    std::fs::create_dir_all(&root).unwrap();
    let running = shelfy_server::ids::new_ulid();
    let queued = shelfy_server::ids::new_ulid();
    let recent = shelfy_server::ids::new_ulid();
    let link = shelfy_server::ids::new_ulid();
    for name in [&running, &queued, &recent] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    let old = std::time::SystemTime::now() - Duration::from_secs(25 * 3600);
    for name in [&running, &queued] {
        std::fs::File::open(root.join(name))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep"), b"fixture").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join(&link)).unwrap();
    h.t.state.control().write(|c|c.execute("UPDATE jobs SET state='running',payload_json=json_set(payload_json,'$.workCaptureId',?2) WHERE id=?1",rusqlite::params![job,running]).map(|_|()).map_err(RepoError::from)).unwrap();
    shelfy_server::jobs::capture::sweep(&h.t.state, std::time::SystemTime::now()).await;
    assert!(root.join(running).is_dir());
    assert!(!root.join(queued).exists());
    assert!(root.join(recent).is_dir());
    assert!(
        std::fs::symlink_metadata(root.join(link))
            .unwrap()
            .is_symlink()
    );
    assert!(outside.path().join("keep").is_file());
}
