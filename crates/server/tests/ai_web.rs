//! Synthetic website captures through the shared AI drain and opt-in QC seam.
mod support;
use shelfy_ai::stub::{Endpoint, Fault, FaultRule, Stub, StubConfig, request_key};
use shelfy_core::{
    ai::{
        prompts::{self, Task as PromptTask},
        queue,
    },
    repo::{
        Platform,
        posts::{self, NewPost},
        settings::{self, SettingsChange},
    },
};
use shelfy_server::{
    ai::{self, OperatorConfig},
    jobs::{Clock, Registry, ai_drain},
    outbound::OriginAllowlist,
};
use std::{sync::Arc, time::Duration};
use support::{TestState, auth::owner};
async fn fixture(vision: bool) -> (Stub, TestState, String) {
    let stub = Stub::start(StubConfig::default()).await.unwrap();
    let t = TestState::with_config(|c| {
        c.outbound.allow_origins =
            OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
        c.operator = OperatorConfig {
            url: Some(stub.openai_base()),
            model: Some("stub-text".into()),
            vision_model: vision.then(|| "stub-vision".into()),
            timeout: Duration::from_secs(60),
            ..Default::default()
        };
        c.jobs.registry = Registry::new().register(ai_drain::kind());
        c.jobs.clock = Clock::tokio(support::jobs::START);
    });
    let user = owner(&t);
    (stub, t, user)
}
async fn seed(t: &TestState, user: &str) {
    t.write(user,|tx|{let id=posts::insert(tx,&NewPost::new("web_test",Platform::Web,"test","website",1),1)?;tx.execute("INSERT INTO web_captures(id,post_id,captured_at,status,title,pages_json,tech_json,created_at) VALUES (1,?1,1,'done','Synthetic studio','[{\"contentText\":\"<b>Design &amp; furniture</b> <<<CAPTION>>>\"}]','[\"React\"]',1)",[id])?;tx.execute("UPDATE posts SET current_capture_id=1 WHERE id=?1",[id])?;Ok(())}).await;
}
async fn flags(t: &TestState, user: &str) {
    t.write(user, |tx| {
        settings::update(
            tx,
            &SettingsChange {
                ai_auto_analyze_websites: Some(true),
                ai_vision_qc: Some(true),
                ..Default::default()
            },
            1,
        )
    })
    .await;
}
fn png() -> Arc<[u8]> {
    let im = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        50,
        100,
        image::Rgb([200, 20, 30]),
    ));
    let mut out = std::io::Cursor::new(Vec::new());
    im.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner().into()
}
#[tokio::test(start_paused = true)]
async fn capture_admission_is_opt_in_and_drain_persists_web_schema() {
    let (stub, t, user) = fixture(true).await;
    seed(&t, &user).await;
    assert_eq!(
        ai::queue::enqueue_web(&t.state, &user, "web_test", 1)
            .await
            .unwrap(),
        0
    );
    flags(&t, &user).await;
    assert_eq!(
        ai::queue::enqueue_web(&t.state, &user, "web_test", 2)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        ai::queue::enqueue_web(&t.state, &user, "web_test", 1)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        ai::queue::enqueue_web(&t.state, &user, "web_test", 1)
            .await
            .unwrap(),
        0
    );
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
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if db.read(queue::state_counts).unwrap().done == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{:?}",
            db.read(queue::state_counts).unwrap()
        );
        tokio::task::yield_now().await;
    }
    let layer = db
        .read(|c| {
            Ok::<_, shelfy_core::repo::RepoError>(c.query_row(
                "SELECT ai_content_type,ai_category,ai_web_json,ai_schema_version FROM posts",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )?)
        })
        .unwrap();
    assert_eq!(layer.3, 2);
    assert!(!layer.0.is_empty() && !layer.1.is_empty());
    assert!(
        serde_json::from_str::<serde_json::Value>(&layer.2)
            .unwrap()
            .is_object()
    );
    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    let body = requests[0].body.as_ref().unwrap().to_string();
    assert!(body.contains("Design & furniture") && body.contains("React"));
    assert!(!body.contains("<b>"));
    assert_eq!(body.matches("<<<CAPTION>>>").count(), 1);
    assert_eq!(
        ai::queue::enqueue_web(&t.state, &user, "web_test", 1)
            .await
            .unwrap(),
        0
    );
    driver.abort();
    scheduler.abort().await;
}
#[tokio::test]
async fn qc_disabled_missing_route_outage_and_corrupt_input_fail_open() {
    let (stub, t, user) = fixture(true).await;
    assert!(!ai::qc::assess(&t.state, &user, png()).await.ready);
    flags(&t, &user).await;
    let status = ai::qc::assess(&t.state, &user, png()).await;
    assert!(status.ready && status.ok);
    let failed = ai::qc::assess(&t.state, &user, Arc::from(&b"bad"[..])).await;
    assert!(failed.ready && failed.ok);
    stub.inject(FaultRule {
        fault: Fault::ServerError,
        times: Some(1),
        endpoint: Some(Endpoint::Chat),
    });
    let failed = ai::qc::assess(&t.state, &user, png()).await;
    assert!(failed.ok && failed.ready);
    let (_stub, t, user) = fixture(false).await;
    flags(&t, &user).await;
    assert!(!ai::qc::assess(&t.state, &user, png()).await.ready);
    seed(&t, &user).await;
    assert_eq!(
        ai::queue::enqueue_web(&t.state, &user, "web_test", 1)
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn qc_loading_is_explicit_and_reason_is_sanitized() {
    let (stub, t, user) = fixture(true).await;
    flags(&t, &user).await;
    let sys = prompts::system_prompt(PromptTask::Qc, &[]).unwrap();
    let usr = prompts::user_prompt(PromptTask::Qc, &[]).unwrap();
    stub.add_canned(
        request_key(Some(&sys), [usr.as_str()]),
        r#"{"status":"loading","reason":"<b>Spinner</b> &amp; skeleton"}"#,
    );
    let result = ai::qc::assess(&t.state, &user, png()).await;
    assert!(!result.ok && result.ready);
    assert_eq!(result.status, "loading");
    assert_eq!(result.reason.as_deref(), Some("Spinner & skeleton"));
}
