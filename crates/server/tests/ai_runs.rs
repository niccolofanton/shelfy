//! Synthetic provider-backed taxonomy jobs, including durable interruption.
mod support;
use axum::http::StatusCode;
use serde_json::json;
use shelfy_ai::stub::{Endpoint, Fault, FaultRule, Stub, StubConfig, request_key};
use shelfy_core::ai::taxonomy_prompt;
use shelfy_core::repo::{
    Platform, notifications,
    posts::{self, AiPatch, NewPost},
};
use shelfy_core::tags::{aliases, clusters, graph};
use shelfy_server::{
    ai::{
        OperatorConfig,
        runs::{self, RunKind},
    },
    events::model::JobState,
    jobs::{Clock, Registry, ai_run},
    outbound::OriginAllowlist,
};
use std::time::Duration;
use support::auth::{owner, sign_in, with_session};
use support::{TestState, json as body_json, post_json, send};

async fn fixture(embed: bool) -> (Stub, TestState, String) {
    let stub = Stub::start(StubConfig::default()).await.unwrap();
    let t = TestState::with_config(|c| {
        c.outbound.allow_origins =
            OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
        c.operator = OperatorConfig {
            url: Some(stub.openai_base()),
            model: Some("stub-text".into()),
            embed_model: embed.then(|| "stub-embed".into()),
            timeout: Duration::from_secs(60),
            ..Default::default()
        };
        c.jobs.registry = Registry::new().register(ai_run::kind());
        c.jobs.clock = Clock::tokio(support::jobs::START);
    });
    let user = owner(&t);
    (stub, t, user)
}
async fn seed(t: &TestState, user: &str, groups: usize, tags_per_group: usize) {
    t.write(user, move |tx| {
        for group in 0..groups {
            for copy in 0..3 {
                let native = format!("{}", 10000 + group * 10 + copy);
                let key = format!("x_{native}");
                let id = posts::insert(
                    tx,
                    &NewPost::new(&key, Platform::Twitter, &native, "text", 1),
                    1,
                )?;
                posts::update_ai(
                    tx,
                    id,
                    &AiPatch {
                        tags: Some(Some(
                            (0..tags_per_group)
                                .map(|n| format!("g{group} tag{n}"))
                                .collect(),
                        )),
                        ..Default::default()
                    },
                    1,
                )?;
            }
        }
        Ok(())
    })
    .await;
}
async fn canned_clusters(stub: &Stub, t: &TestState, user: &str) -> usize {
    let groups = t
        .state
        .user_db(user)
        .await
        .unwrap()
        .read(|c| graph::candidate_groups(c, None, graph::Options::default()))
        .unwrap();
    for (i, group) in groups.iter().enumerate() {
        let prompt = taxonomy_prompt::refine(group).unwrap();
        stub.add_canned(
            request_key(Some(&prompt.system), [prompt.user.as_str()]),
            json!({"groups":[{"name":format!("Theme {i}"),"tags":group.tags}],"outliers":[]})
                .to_string(),
        );
    }
    groups.len()
}
async fn canned_aliases(stub: &Stub, t: &TestState, user: &str) -> usize {
    let plan = t
        .state
        .user_db(user)
        .await
        .unwrap()
        .read(|c| runs::snapshot(c, RunKind::Aliases))
        .unwrap();
    for batch in plan.tags.chunks(40) {
        let prompt = taxonomy_prompt::aliases(batch, &plan.vocabulary).unwrap();
        let pairs = batch
            .iter()
            .filter(|tag| tag.norm != plan.vocabulary[0].norm)
            .map(|tag| json!({"alias":tag.norm,"canonical":plan.vocabulary[0].norm}))
            .collect::<Vec<_>>();
        stub.add_canned(
            request_key(Some(&prompt.system), [prompt.user.as_str()]),
            json!({"aliases":pairs}).to_string(),
        );
    }
    plan.tags.len().div_ceil(40)
}
fn driver() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        loop {
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        }
    })
}
async fn proposed(t: &TestState, user: &str) -> usize {
    t.state
        .user_db(user)
        .await
        .unwrap()
        .read(|c| {
            Ok::<_, shelfy_core::repo::RepoError>(
                clusters::list(c, 400)?
                    .iter()
                    .filter(|c| c.status == shelfy_core::tags::Status::Proposed)
                    .count(),
            )
        })
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn full_cluster_run_records_usage_and_reuses_embedding_cache() {
    let (stub, t, user) = fixture(true).await;
    seed(&t, &user, 2, 20).await;
    // Embeddings can alter groups: return the raw input group via refusal,
    // verifying the specified fallback and usage for the failed calls.
    stub.inject(FaultRule {
        fault: Fault::Refusal,
        times: None,
        endpoint: Some(Endpoint::Chat),
    });
    let id = runs::enqueue(&t.state, &user, RunKind::Clusters)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let driver = driver();
    let job = t
        .wait_job(&user, id, |j| {
            matches!(j.state, JobState::Succeeded | JobState::Failed)
        })
        .await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert!(proposed(&t, &user).await > 0);
    let log = stub.requests();
    let embeds = log
        .iter()
        .filter(|r| r.endpoint == Endpoint::Embeddings)
        .collect::<Vec<_>>();
    assert_eq!(embeds.len(), 2);
    assert_eq!(
        embeds[0].body.as_ref().unwrap()["input"]
            .as_array()
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        embeds[1].body.as_ref().unwrap()["input"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let count = t
        .state
        .user_db(&user)
        .await
        .unwrap()
        .read(|c| {
            Ok::<_, shelfy_core::repo::RepoError>(c.query_row(
                "SELECT COUNT(*) FROM tag_embeddings",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 40);
    let control = t.state.control();
    let calls = control
        .read(|c| {
            shelfy_server::control::usage_daily::of_day(c, &user, shelfy_server::ids::now_ms())
        })
        .unwrap();
    assert!(calls.ai_calls >= 2);
    let id2 = runs::enqueue(&t.state, &user, RunKind::Clusters)
        .await
        .unwrap()
        .job
        .id;
    assert_eq!(
        t.wait_job(&user, id2, |j| matches!(
            j.state,
            JobState::Succeeded | JobState::Failed
        ))
        .await
        .state,
        JobState::Succeeded
    );
    assert_eq!(
        stub.requests()
            .iter()
            .filter(|r| r.endpoint == Endpoint::Embeddings)
            .count(),
        2
    );
    let notes = t
        .state
        .user_db(&user)
        .await
        .unwrap()
        .read(|c| notifications::list(c, None, 20))
        .unwrap();
    assert!(
        notes
            .items
            .iter()
            .any(|n| n.code == "ai.run_finished" && n.params["kind"] == "clusters")
    );
    driver.abort();
    scheduler.abort().await;
}
#[tokio::test(start_paused = true)]
async fn full_alias_run_validates_batches_and_does_not_rewrite_posts() {
    let (stub, t, user) = fixture(false).await;
    seed(&t, &user, 9, 5).await;
    t.write(&user, |tx| {
        aliases::save_proposals(
            tx,
            &[aliases::AliasPair {
                alias_norm: "anchor".into(),
                alias_form: "anchor".into(),
                canonical_norm: "g0 tag0".into(),
                canonical_form: "g0 tag0".into(),
            }],
            1,
        )?;
        Ok(())
    })
    .await;
    assert_eq!(canned_aliases(&stub, &t, &user).await, 2);
    let id = runs::enqueue(&t.state, &user, RunKind::Aliases)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let driver = driver();
    let job = t
        .wait_job(&user, id, |j| {
            matches!(j.state, JobState::Succeeded | JobState::Failed)
        })
        .await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let db = t.state.user_db(&user).await.unwrap();
    let pairs = db.read(|c| aliases::list(c, None)).unwrap();
    assert_eq!(
        pairs.iter().filter(|p| p.alias_norm != "anchor").count(),
        44
    );
    assert!(
        pairs
            .iter()
            .all(|p| p.status == shelfy_core::tags::Status::Proposed)
    );
    assert_eq!(
        db.read(|c| Ok::<_, shelfy_core::repo::RepoError>(c.query_row(
            "SELECT COUNT(DISTINCT tag_norm) FROM post_tags",
            [],
            |r| r.get::<_, i64>(0)
        )?))
        .unwrap(),
        45
    );
    assert_eq!(
        stub.requests()
            .iter()
            .filter(|r| r.endpoint == Endpoint::Chat)
            .count(),
        2
    );
    driver.abort();
    scheduler.abort().await;
}
#[tokio::test(start_paused = true)]
async fn embedding_failure_falls_back_to_cooccurrence_and_refines_each_group() {
    let (stub, t, user) = fixture(true).await;
    seed(&t, &user, 3, 3).await;
    let groups = canned_clusters(&stub, &t, &user).await;
    assert_eq!(groups, 3);
    stub.inject(FaultRule {
        fault: Fault::BadRequest,
        times: Some(1),
        endpoint: Some(Endpoint::Embeddings),
    });
    let id = runs::enqueue(&t.state, &user, RunKind::Clusters)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let driver = driver();
    let job = t
        .wait_job(&user, id, |j| {
            matches!(j.state, JobState::Succeeded | JobState::Failed)
        })
        .await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(proposed(&t, &user).await, groups);
    assert_eq!(
        stub.requests()
            .iter()
            .filter(|r| r.endpoint == Endpoint::Chat)
            .count(),
        groups
    );
    driver.abort();
    scheduler.abort().await;
}
#[tokio::test(start_paused = true)]
async fn offline_resume_and_cancel_keep_committed_chunks_across_fresh_state() {
    let (stub, t, user) = fixture(false).await;
    seed(&t, &user, 4, 3).await;
    canned_clusters(&stub, &t, &user).await;
    stub.set_latency(Duration::from_secs(2));
    let id = runs::enqueue(&t.state, &user, RunKind::Clusters)
        .await
        .unwrap()
        .job
        .id;
    stub.set_offline(true).await.unwrap();
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let driver = driver();
    let queued = t
        .wait_job(&user, id, |j| {
            j.state == JobState::Queued && j.run_at > support::jobs::START
        })
        .await;
    assert_eq!(queued.attempts, 0);
    assert_eq!(proposed(&t, &user).await, 0);
    stub.set_offline(false).await.unwrap();
    t.state.ai().operator_probe_once(&t.state).await;
    t.wait_job(&user, id, |_| {
        t.state.user_dbs().get_if_present(&user).is_some_and(|db| {
            db.read(|c| Ok::<_, shelfy_core::repo::RepoError>(!clusters::list(c, 100)?.is_empty()))
                .unwrap()
        })
    })
    .await;
    let cancelled = t.state.jobs().cancel(&user, id).await.unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    let before = proposed(&t, &user).await;
    assert!((1..4).contains(&before));
    scheduler.abort().await;
    assert_eq!(proposed(&t, &user).await, before);
    // Retry the durable plan through a fresh AppState around the same databases.
    let config = t.state.config().clone();
    let t = TestState::with_config(|c| *c = config);
    t.state.jobs().retry(&user, id).await.unwrap();
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let done = t
        .wait_job(&user, id, |j| {
            matches!(j.state, JobState::Succeeded | JobState::Failed)
        })
        .await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert_eq!(proposed(&t, &user).await, 4);
    driver.abort();
    scheduler.abort().await;
}
#[tokio::test]
async fn routes_are_authenticated_idempotent_and_deduplicate_both_kinds() {
    let (_stub, t, _user) = fixture(false).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    for path in [
        "/api/v1/tag-clusters/regenerate",
        "/api/v1/tag-aliases/propose",
    ] {
        assert_eq!(
            send(&app, post_json(path, "{}")).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let mut req = with_session(post_json(path, "{}"), &cookie);
        req.headers_mut().insert(
            "idempotency-key",
            format!("taxonomy-run-{path}").parse().unwrap(),
        );
        let response = send(&app, req).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let first = body_json(response).await;
        let mut req = with_session(post_json(path, "{}"), &cookie);
        req.headers_mut().insert(
            "idempotency-key",
            format!("taxonomy-run-{path}").parse().unwrap(),
        );
        let repeated = body_json(send(&app, req).await).await;
        assert_eq!(repeated["id"], first["id"]);
        let duplicate =
            body_json(send(&app, with_session(post_json(path, "{}"), &cookie)).await).await;
        assert_eq!(duplicate["id"], first["id"]);
    }
    let spec = ai_run::kind();
    assert_eq!(
        (
            spec.spec().global,
            spec.spec().per_user,
            spec.spec().max_attempts
        ),
        (4, 1, 1)
    );
    assert_eq!(spec.spec().lease, Duration::from_secs(1800));
}

#[tokio::test(start_paused = true)]
async fn alias_cancel_preserves_only_committed_batches_and_no_finished_notification() {
    let (stub, t, user) = fixture(false).await;
    seed(&t, &user, 21, 4).await;
    t.write(&user, |tx| {
        aliases::save_proposals(
            tx,
            &[aliases::AliasPair {
                alias_norm: "anchor".into(),
                alias_form: "anchor".into(),
                canonical_norm: "g0 tag0".into(),
                canonical_form: "g0 tag0".into(),
            }],
            1,
        )?;
        Ok(())
    })
    .await;
    assert_eq!(canned_aliases(&stub, &t, &user).await, 3);
    stub.set_latency(Duration::from_secs(2));
    let id = runs::enqueue(&t.state, &user, RunKind::Aliases)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), tokio_util::sync::CancellationToken::new());
    let driver = driver();
    let db = t.state.user_db(&user).await.unwrap();
    t.wait_job(&user, id, |_| {
        db.read(|c| aliases::list(c, None)).unwrap().len() > 1
    })
    .await;
    t.state.jobs().pause(&user, ai_run::KIND).await.unwrap();
    t.wait_job(&user, id, |j| j.state == JobState::Queued).await;
    let before = db.read(|c| aliases::list(c, None)).unwrap().len();
    assert!(before > 1 && before < 84);
    t.state.jobs().cancel(&user, id).await.unwrap();
    scheduler.abort().await;
    assert_eq!(db.read(|c| aliases::list(c, None)).unwrap().len(), before);
    assert!(
        !db.read(|c| notifications::list(c, None, 20))
            .unwrap()
            .items
            .iter()
            .any(|n| n.code == "ai.run_finished")
    );
    driver.abort();
}

#[tokio::test]
async fn alias_plan_bounds_candidates_and_vocabulary_without_control_payload_growth() {
    let (_stub, t, user) = fixture(false).await;
    seed(&t, &user, 101, 4).await;
    let db = t.state.user_db(&user).await.unwrap();
    let plan = db.read(|c| runs::snapshot(c, RunKind::Aliases)).unwrap();
    assert_eq!(plan.tags.len(), 400);
    assert_eq!(plan.vocabulary.len(), 300);
    let row = runs::enqueue(&t.state, &user, RunKind::Aliases)
        .await
        .unwrap()
        .job;
    assert!(
        row.payload_json.len() < 100,
        "the vocabulary must stay in the library"
    );
}
