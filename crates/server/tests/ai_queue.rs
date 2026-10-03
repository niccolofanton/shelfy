//! Entire social catalog queue on the loopback provider and synthetic libraries.
mod support;
use axum::body::Body;
use axum::http::StatusCode;
use serde_json::{Value, json};
use shelfy_ai::stub::{Endpoint, Fault, FaultRule, Stub, StubConfig};
use shelfy_core::ai::queue::{self, Mode};
use shelfy_core::repo::{
    Platform,
    posts::{self, NewPost},
};
use shelfy_core::selector::Selector;
use shelfy_server::{
    ai::{self, OperatorConfig},
    events::model::JobState,
    jobs::{Clock, Registry, ai_drain},
    outbound::OriginAllowlist,
};
use std::time::Duration;
use support::auth::{owner, sign_in, with_session};
use support::{TestState, get, json as response_json, post_json, send};

async fn fixture(config: StubConfig) -> (Stub, TestState, String) {
    fixture_concurrency(config, 1).await
}
async fn fixture_concurrency(config: StubConfig, concurrency: u8) -> (Stub, TestState, String) {
    let stub = Stub::start(config).await.unwrap();
    let t = TestState::with_config(|c| {
        c.outbound.allow_origins =
            OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
        c.operator = OperatorConfig {
            url: Some(stub.openai_base()),
            model: Some("stub-text".into()),
            vision_model: Some("stub-vision".into()),
            timeout: Duration::from_secs(60),
            concurrency,
            ..OperatorConfig::default()
        };
        c.jobs.registry = Registry::new().register(ai_drain::kind());
        c.jobs.clock = Clock::tokio(support::jobs::START);
    });
    let id = owner(&t);
    (stub, t, id)
}
async fn posts(t: &TestState, user: &str, count: usize) -> Vec<String> {
    t.write(user, |tx| {
        let mut keys = vec![];
        for n in 0..count {
            let key = format!("x_{}", 10000 + n);
            let mut p = NewPost::new(&key, Platform::Twitter, &key, "text", 1);
            p.caption = Some(format!("Synthetic brass lamp {n}"));
            posts::insert(tx, &p, 1)?;
            keys.push(key);
        }
        Ok(keys)
    })
    .await
}
async fn counts(t: &TestState, user: &str) -> queue::StateCounts {
    t.state
        .user_db(user)
        .await
        .unwrap()
        .read(queue::state_counts)
        .unwrap()
}
async fn request(
    t: &TestState,
    cookie: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> axum::http::Response<Body> {
    let mut req = post_json(path, body.to_string());
    if let Some(k) = key {
        req.headers_mut()
            .insert("idempotency-key", k.parse().unwrap());
    }
    send(&t.app(), with_session(req, cookie)).await
}

#[tokio::test]
async fn estimate_freezes_population_confirms_once_and_cancel_all_resets_items() {
    let (_stub, t, user) = fixture(StubConfig::default()).await;
    posts(&t, &user, 2).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let body = json!({"selector":{"filter":{}},"mode":"missing","deep":true});
    let response = request(&t, &cookie, "/api/v1/ai/analyze", body.clone(), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let estimate = response_json(response).await;
    assert_eq!(estimate["counts"]["analyzable"], 2);
    assert_eq!(estimate["queued"], false);
    t.write(&user, |tx| {
        let mut p = NewPost::new("x_30000", Platform::Twitter, "30000", "text", 1);
        p.caption = Some("Another synthetic item".into());
        posts::insert(tx, &p, 1)
    })
    .await;
    let token = estimate["confirmToken"].as_str().unwrap();
    let mut confirmed = body.clone();
    confirmed["confirmToken"] = token.into();
    let no_key = request(&t, &cookie, "/api/v1/ai/analyze", confirmed.clone(), None).await;
    assert_eq!(no_key.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let mut mismatch = confirmed.clone();
    mismatch["deep"] = false.into();
    let response = request(
        &t,
        &cookie,
        "/api/v1/ai/analyze",
        mismatch,
        Some("mismatch-key"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let response = request(
        &t,
        &cookie,
        "/api/v1/ai/analyze",
        confirmed.clone(),
        Some("confirm-once"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let answer = response_json(response).await;
    assert_eq!(answer["enqueued"], 2);
    let replay = request(
        &t,
        &cookie,
        "/api/v1/ai/analyze",
        confirmed,
        Some("confirm-once"),
    )
    .await;
    assert_eq!(replay.headers()["idempotent-replayed"], "true");
    assert_eq!(response_json(replay).await, answer);
    let response = request(
        &t,
        &cookie,
        "/api/v1/queues/ai.drain/cancel-all",
        json!({}),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(counts(&t, &user).await.pending, 0);
    assert_eq!(counts(&t, &user).await.unanalyzed, 3);
}

#[tokio::test(start_paused = true)]
async fn confirm_expiry_follows_the_test_clock() {
    let (_stub, t, user) = fixture(StubConfig::default()).await;
    let keys = posts(&t, &user, 2).await;
    let selector = Selector::Keys(keys);
    let result = ai::queue::analyze(
        &t.state,
        &user,
        selector.clone(),
        Mode::Missing,
        None,
        false,
    )
    .await
    .unwrap();
    tokio::time::advance(Duration::from_secs(601)).await;
    assert_eq!(
        ai::queue::analyze(
            &t.state,
            &user,
            selector,
            Mode::Missing,
            result.confirm_token,
            false
        )
        .await
        .unwrap_err()
        .code()
        .as_str(),
        "confirm_token_invalid"
    );
}

#[tokio::test]
async fn routes_deny_anonymous_and_isolate_every_queue_action() {
    let (_stub, t, user) = fixture(StubConfig::default()).await;
    let keys = posts(&t, &user, 1).await;
    for path in [
        "/api/v1/ai/analyze",
        "/api/v1/ai/queue/cancel",
        "/api/v1/ai/queue/retry",
    ] {
        assert_eq!(
            send(&t.app(), post_json(path, "{}")).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(&t.app(), get("/api/v1/ai/queue")).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let member = support::auth::add_member(&t, "member@example.test");
    let member_cookie = support::auth::sign_in_as(&t.app(), &t, "member@example.test").await;
    let owner_cookie = sign_in(&t.app(), &t).await;
    ai::queue::analyze(
        &t.state,
        &user,
        Selector::Keys(keys.clone()),
        Mode::Missing,
        None,
        false,
    )
    .await
    .unwrap();
    let response = request(
        &t,
        &member_cookie,
        "/api/v1/ai/queue/cancel",
        json!({"keys":keys}),
        None,
    )
    .await;
    assert_eq!(response_json(response).await["changed"], 0);
    let view = response_json(
        send(
            &t.app(),
            with_session(get("/api/v1/ai/queue"), &member_cookie),
        )
        .await,
    )
    .await;
    assert_eq!(view["counts"]["pending"], 0);
    assert!(view["providerState"].is_null());
    assert_eq!(
        request(
            &t,
            &member_cookie,
            "/api/v1/ai/analyze",
            json!({"selector":{"keys":["x_10000"]},"mode":"missing"}),
            None
        )
        .await
        .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        request(
            &t,
            &owner_cookie,
            "/api/v1/ai/queue/retry",
            json!({"all":true,"keys":[]}),
            None
        )
        .await
        .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(counts(&t, &member).await.pending, 0);
}

#[tokio::test(start_paused = true)]
async fn drain_runs_500_items_with_faults_offline_pause_restart_and_retry() {
    let (stub, t, user) = fixture(StubConfig::default()).await;
    let keys = posts(&t, &user, 500).await;
    let now = t.state.jobs().clock().now_ms();
    t.write(&user, |tx| {
        queue::mark_pending(tx, &Selector::Keys(keys), Mode::Missing, now)
    })
    .await;
    let job = ai_drain::enqueue(t.state.jobs(), &user)
        .await
        .unwrap()
        .job
        .id;
    stub.inject(FaultRule {
        fault: Fault::ServerError,
        times: Some(4),
        endpoint: Some(Endpoint::Chat),
    });
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let driver = tokio::spawn(async {
        loop {
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let db = t.state.user_db(&user).await.unwrap();
    t.wait_job(&user, job, |_| {
        db.read(queue::state_counts).unwrap().done >= 100
    })
    .await;
    t.state.jobs().pause(&user, ai_drain::KIND).await.unwrap();
    t.wait_job(&user, job, |j| j.state == JobState::Queued)
        .await;
    let paused_done = counts(&t, &user).await.done;
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(counts(&t, &user).await.done, paused_done);
    stub.set_offline(true).await.unwrap();
    t.state.jobs().resume(&user, ai_drain::KIND).await.unwrap();
    t.wait_job(&user, job, |_| {
        t.state.ai().operator_state() == Some(shelfy_server::events::model::ProviderState::Offline)
            && db.read(queue::state_counts).unwrap().analyzing == 0
    })
    .await;
    let attempts = || {
        db.read(|conn| {
            Ok::<_, shelfy_core::repo::RepoError>(conn.query_row(
                "SELECT sum(ai_attempts) FROM posts",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .unwrap()
    };
    let before = attempts();
    tokio::time::advance(Duration::from_secs(120)).await;
    t.wait_job(&user, job, |j| j.state == JobState::Queued)
        .await;
    assert_eq!(attempts(), before, "offline work consumes no item try");
    stub.set_offline(false).await.unwrap();
    t.state.ai().operator_probe_once(&t.state).await;
    t.wait_job(&user, job, |_| {
        db.read(queue::state_counts).unwrap().done >= 220
    })
    .await;
    scheduler.abort().await;
    // A process restart creates fresh gates/caches around the same durable rows.
    let config = t.state.config().clone();
    let t = TestState::with_config(|c| *c = config);
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    stub.inject(FaultRule::new(Fault::Refusal).on(Endpoint::Chat));
    let row = t
        .wait_job(&user, job, |j| j.state == JobState::Succeeded)
        .await;
    assert_eq!(row.state, JobState::Succeeded);
    let c = counts(&t, &user).await;
    assert_eq!(c.done + c.error, 500);
    assert_eq!(c.pending + c.analyzing, 0);
    assert!(c.error > 0, "a refusal is terminal before explicit retry");
    assert_eq!(
        ai::queue::retry(&t.state, &user, queue::Reach::All)
            .await
            .unwrap(),
        c.error
    );
    let retried = ai_drain::enqueue(t.state.jobs(), &user)
        .await
        .unwrap()
        .job
        .id;
    t.wait_job(&user, retried, |j| j.state == JobState::Succeeded)
        .await;
    assert_eq!(counts(&t, &user).await.done, 500);
    driver.abort();
    scheduler.abort().await;
    let errors = t
        .state
        .user_db(&user)
        .await
        .unwrap()
        .read(queue::errors_by_code)
        .unwrap();
    assert!(errors.is_empty(), "{errors:?}");
}

#[tokio::test(start_paused = true)]
async fn ai_stream_is_opt_in_live_only_and_has_no_replay_id() {
    let t = TestState::new();
    let user = support::library::ALICE;
    let app = t.app_as(user);
    let mut ordinary = support::sse::Stream::connect(&app, "/api/v1/events", &[]).await;
    let resume = ordinary.hello().await.id.unwrap();
    let mut live =
        support::sse::Stream::connect(&app, "/api/v1/events?topics=ai.stream", &[]).await;
    live.hello().await;
    t.state
        .events()
        .ai_stream(user, "x_10000", "synthetic partial".into());
    let event = live.next().await;
    assert_eq!(event.event.as_deref(), Some("ai.stream"));
    assert!(event.id.is_none());
    assert!(
        ordinary.next().await.is_heartbeat(),
        "default subscribers do not receive partial text"
    );
    let mut replay = support::sse::Stream::connect(
        &app,
        "/api/v1/events?topics=ai.stream",
        &[("last-event-id", &resume)],
    )
    .await;
    replay.hello().await;
    assert!(
        replay.next().await.is_heartbeat(),
        "partial text is absent from the replay ring"
    );
}

#[tokio::test(start_paused = true)]
async fn quota_and_invalid_key_pause_without_failing_the_items() {
    for fault in [Fault::QuotaExhausted, Fault::Unauthorized] {
        let (stub, t, user) = fixture(StubConfig::default()).await;
        let keys = posts(&t, &user, 2).await;
        let now = t.state.jobs().clock().now_ms();
        t.write(&user, |tx| {
            queue::mark_pending(tx, &Selector::Keys(keys), Mode::Missing, now)
        })
        .await;
        stub.inject(FaultRule::new(fault).on(Endpoint::Chat));
        let job = ai_drain::enqueue(t.state.jobs(), &user)
            .await
            .unwrap()
            .job
            .id;
        let scheduler = t
            .state
            .jobs()
            .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
        let driver = tokio::spawn(async {
            loop {
                tokio::time::advance(Duration::from_millis(100)).await;
                tokio::task::yield_now().await;
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        t.wait_job(&user, job, |j| {
            j.state == JobState::Queued && t.state.jobs().is_paused(&user, ai_drain::KIND)
        })
        .await;
        let db = t.state.user_db(&user).await.unwrap();
        let attempts: i64 = db
            .read(|conn| {
                Ok::<_, shelfy_core::repo::RepoError>(conn.query_row(
                    "SELECT sum(ai_attempts) FROM posts",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(attempts, 0);
        assert_eq!(counts(&t, &user).await.pending, 2);
        assert_eq!(counts(&t, &user).await.error, 0);
        driver.abort();
        scheduler.abort().await;
    }
}

#[tokio::test(start_paused = true)]
async fn stored_images_are_inline_jpeg_and_cover_duplicates_are_sent_once() {
    use shelfy_core::repo::{
        media::{self, NewMediaObject},
        posts::NewMedia,
    };
    use shelfy_media::store::{IngestLimits, MediaStore};
    let (stub, t, user) = fixture(StubConfig {
        webp_images: false,
        ..StubConfig::default()
    })
    .await;
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(32, 32)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let store = MediaStore::new(t.data_dir().users_dir())
        .user(&user)
        .unwrap();
    let object = store
        .ingest(
            std::io::Cursor::new(jpeg.into_inner()),
            IngestLimits::ARCHIVE_IMAGE,
        )
        .unwrap()
        .publish()
        .unwrap();
    t.write(&user, |tx| {
        let obj = media::upsert_object(
            tx,
            &NewMediaObject {
                sha256: *object.digest.as_bytes(),
                ext: object.kind.ext().into(),
                mime: object.kind.mime().into(),
                bytes: object.size as i64,
                width: Some(32),
                height: Some(32),
                duration_ms: None,
                role: "image".into(),
                variants: 0,
                origin: "server".into(),
            },
            1,
        )?;
        let mut post = NewPost::new("ig_10000", Platform::Instagram, "10000", "carousel", 1);
        post.cover_object = Some(obj);
        post.media = vec![NewMedia {
            kind: "image".into(),
            object_id: Some(obj),
            ..NewMedia::default()
        }];
        posts::insert(tx, &post, 1)
    })
    .await;
    ai::queue::analyze(
        &t.state,
        &user,
        Selector::Keys(vec!["ig_10000".into()]),
        Mode::Missing,
        None,
        false,
    )
    .await
    .unwrap();
    let job = ai_drain::enqueue(t.state.jobs(), &user)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let driver = tokio::spawn(async {
        loop {
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    t.wait_job(&user, job, |j| j.state == JobState::Succeeded)
        .await;
    assert_eq!(counts(&t, &user).await.done, 1);
    let requests = stub.requests();
    let body = requests
        .iter()
        .find(|r| r.endpoint == Endpoint::Chat)
        .unwrap()
        .body
        .as_ref()
        .unwrap();
    let parts = body["messages"][1]["content"].as_array().unwrap();
    let images = parts
        .iter()
        .filter(|p| p["type"] == "image_url")
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 1);
    assert!(
        images[0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    driver.abort();
    scheduler.abort().await;
}

#[tokio::test(start_paused = true)]
async fn two_synthetic_users_share_the_operator_gate_without_starvation() {
    let (_stub, t, alice) = fixture(StubConfig::default()).await;
    let bob = support::auth::add_member(&t, "bob@example.test");
    support::auth::control_db(&t)
        .execute("UPDATE users SET role='owner' WHERE id=?1", [&bob])
        .unwrap();
    for user in [&alice, &bob] {
        let keys = posts(&t, user, 4).await;
        let now = t.state.jobs().clock().now_ms();
        t.write(user, |tx| {
            queue::mark_pending(tx, &Selector::Keys(keys), Mode::Missing, now)
        })
        .await;
    }
    let a = ai_drain::enqueue(t.state.jobs(), &alice)
        .await
        .unwrap()
        .job
        .id;
    let b = ai_drain::enqueue(t.state.jobs(), &bob)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let driver = tokio::spawn(async {
        loop {
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    t.wait_job(&alice, a, |j| j.state == JobState::Succeeded)
        .await;
    t.wait_job(&bob, b, |j| j.state == JobState::Succeeded)
        .await;
    assert_eq!(counts(&t, &alice).await.done, 4);
    assert_eq!(counts(&t, &bob).await.done, 4);
    driver.abort();
    scheduler.abort().await;
}

#[tokio::test]
async fn drain_uses_both_operator_slots_and_never_exceeds_them() {
    let (stub, t, user) = fixture_concurrency(
        StubConfig {
            latency: Duration::from_secs(3),
            ..StubConfig::default()
        },
        2,
    )
    .await;
    let keys = posts(&t, &user, 4).await;
    let now = t.state.jobs().clock().now_ms();
    t.write(&user, |tx| {
        queue::mark_pending(tx, &Selector::Keys(keys), Mode::Missing, now)
    })
    .await;
    let job = ai_drain::enqueue(t.state.jobs(), &user)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    tokio::time::timeout(Duration::from_secs(2), async {
        while stub.requests().len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both requests must reach the provider before either completes");
    let c = counts(&t, &user).await;
    assert_eq!(c.analyzing, 2);
    assert_eq!(c.done, 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        stub.requests().len(),
        2,
        "no third request while both slots are occupied"
    );
    t.wait_job(&user, job, |j| j.state == JobState::Succeeded)
        .await;
    assert_eq!(counts(&t, &user).await.done, 4);
    scheduler.abort().await;
}

#[tokio::test]
async fn admin_status_reports_orphans_and_offline_waiting_as_aggregates_only() {
    let (_stub, t, user) = fixture(StubConfig::default()).await;
    let keys = posts(&t, &user, 3).await;
    let now = t.state.jobs().clock().now_ms();
    t.write(&user, |tx| {
        queue::mark_pending(tx, &Selector::Keys(keys), Mode::Missing, now)?;
        let claim = queue::claim_due(tx, now)?.unwrap();
        queue::fail(
            tx,
            claim.post_id,
            claim.attempt,
            &claim.token,
            "refused",
            now,
        )?;
        queue::claim_due(tx, now)?.unwrap();
        queue::set_provider_status(tx, "offline", now)
    })
    .await;
    let data = t.state.config().data_dir.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut output = Vec::new();
        shelfy_server::admin::ai_status::run(
            &data,
            &shelfy_server::admin::ai_status::AiStatusArgs { user },
            &mut output,
        )
        .unwrap();
        String::from_utf8(output).unwrap()
    })
    .await
    .unwrap();
    for expected in [
        "unanalyzed=0 pending=1 analyzing=1 done=0 errors=1",
        "due_pending=1 orphaned_analyzing=1",
        "provider_last_observed=offline waiting_for_offline=1",
        "error.refused=1",
    ] {
        assert!(output.contains(expected), "{output}");
    }
    assert!(!output.contains("x_1000"));
    assert!(!output.contains("Synthetic brass lamp"));
}
