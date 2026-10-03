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
            "{:?} errors={:?} requests={:?}",
            db.read(queue::state_counts).unwrap(),
            db.read(queue::errors_by_code).unwrap(),
            stub.requests()
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
    assert_eq!(body.matches("<<<CAPTION>>>").count(), 0);
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

#[tokio::test(start_paused = true)]
async fn recorded_p4_capture_keeps_measured_facets_and_sends_four_768px_jpegs() {
    use base64::Engine as _;
    use rusqlite::params;
    use serde_json::{Value, json};
    use shelfy_media::{MediaKind, digest::Digest, store::MediaStore};
    let (stub, t, user) = fixture(true).await;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../capture/fixtures/recorded/basic");
    let m: Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let store = MediaStore::new(t.state.config().data_dir.users_dir())
        .user(&user)
        .unwrap();
    let mut files = vec![];
    for name in [
        "p0-hero.webp",
        "p0-band0.webp",
        "p0-band1.webp",
        "p0-band2.webp",
        "p0-footer.webp",
    ] {
        let bytes = std::fs::read(root.join(name)).unwrap();
        let digest = Digest::of(&bytes);
        let path = store.object_path(&digest, MediaKind::Webp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &bytes).unwrap();
        files.push((digest.as_bytes().to_vec(), bytes.len()));
    }
    let mut pages = m["pages"].as_array().unwrap().clone();
    for p in &mut pages {
        p.as_object_mut().unwrap().remove("assets");
    }
    let capture=t.write(&user,move|tx|{
        let id=posts::insert(tx,&NewPost::new("web_test",Platform::Web,"test","website",1),1)?;
        for (i,(hash,bytes)) in files.iter().enumerate(){tx.execute("INSERT INTO media_objects(id,sha256,ext,mime,bytes,role,origin,created_at) VALUES (?1,?2,'webp','image/webp',?3,'band','capture',1)",params![(i+1) as i64,hash,*bytes as i64])?;}
        // Same NewCapture shape and metadata wrapper written by P4-14 ingest.
        let mut c=shelfy_core::web::captures::NewCapture::new(1);c.title=m["title"].as_str().map(str::to_owned);c.requested_url=m["url"].as_str().map(str::to_owned);c.final_url=m["finalUrl"].as_str().map(str::to_owned);c.palette=Some(m["palette"].clone());c.fonts=Some(m["typography"]["fonts"].clone());c.tech=Some(m["tech"].clone());c.traits=Some(m["traits"].clone());c.awards=Some(m["awards"].clone());c.meta=Some(json!({"description":m["description"],"metadata":m["webMeta"],"capture":{"jobId":99}}));c.pages=pages;c.hero_object=Some(1);
        use shelfy_core::web::{AssetRole,captures::{self,NewAsset}};
        let assets=vec![NewAsset::new(0,AssetRole::Hero,0,1),NewAsset::new(0,AssetRole::Band,0,2),NewAsset::new(0,AssetRole::Band,1,3),NewAsset::new(0,AssetRole::Band,2,4),NewAsset::new(0,AssetRole::Footer,0,5)];
        captures::insert(tx,id,&c,&assets,1)
    }).await;
    flags(&t, &user).await;
    assert_eq!(
        ai::queue::enqueue_web(&t.state, &user, "web_test", capture)
            .await
            .unwrap(),
        1
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
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if db.read(queue::state_counts).unwrap().done == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{:?} errors={:?} requests={:?}",
            db.read(queue::state_counts).unwrap(),
            db.read(queue::errors_by_code).unwrap(),
            stub.requests()
        );
        tokio::task::yield_now().await;
    }
    let body = stub.requests()[0].body.clone().unwrap();
    let content = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["role"] == "user")
        .unwrap()["content"]
        .as_array()
        .unwrap();
    let images = content
        .iter()
        .filter(|v| v["type"] == "image_url")
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 4);
    for image in images {
        let data = image["image_url"]["url"]
            .as_str()
            .unwrap()
            .strip_prefix("data:image/jpeg;base64,")
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width().max(decoded.height()), 768);
    }
    assert_eq!(
        body["response_format"]["json_schema"]["name"],
        "web_catalog_v2"
    );
    let text = body.to_string();
    assert!(
        text.contains("indigo")
            && text.contains("dark")
            && text.contains("GROUND TRUTH")
            && text.contains("Section 1")
    );
    let catalog: Value = db
        .read(|c| {
            Ok::<_, shelfy_core::repo::RepoError>(
                serde_json::from_str::<Value>(&c.query_row(
                    "SELECT ai_web_json FROM posts",
                    [],
                    |r| r.get::<_, String>(0),
                )?)
                .unwrap(),
            )
        })
        .unwrap();
    assert_eq!(catalog["schema"], 2);
    assert!(catalog["observations"].is_string());
    assert!(catalog["notableDetails"].is_array());
    assert!(catalog["referenceFor"].is_array());
    assert_eq!(catalog["facets"]["scheme"], json!(["dark"]));
    assert!(
        catalog["facets"]["color"]
            .as_array()
            .unwrap()
            .contains(&json!("indigo"))
    );
    assert_eq!(catalog["facets"].as_object().unwrap().len(), 19);
    driver.abort();
    scheduler.abort().await;
}

#[tokio::test]
async fn mixed_preview_and_confirmation_use_the_rich_web_budget_without_dry_run_admission() {
    use serde_json::json;
    use support::{
        auth::{sign_in, with_session},
        json as response_json, post_json, send,
    };
    let (_stub, t, user) = fixture(true).await;
    seed(&t, &user).await;
    t.write(&user, |tx| {
        let mut post = NewPost::new("x_quote", Platform::Twitter, "quote", "text", 1);
        post.caption = Some("Synthetic lamp".into());
        posts::insert(tx, &post, 1)
    })
    .await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let body =
        json!({"selector":{"keys":["web_test","x_quote"]},"mode":"selected","estimateOnly":true});
    let response = send(
        &app,
        with_session(post_json("/api/v1/ai/analyze", body.to_string()), &cookie),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let preview = response_json(response).await;
    assert_eq!(preview["estimate"]["outputTokens"], 768 + 2048);
    assert_eq!(
        preview["estimate"]["inputTokens"],
        750 + 2 * 900 + 4000 + 4 * 900
    );
    assert_eq!(preview["queued"], false);
    assert!(preview["estimate"]["costUsd"].is_null());
    assert_eq!(
        t.state
            .user_db(&user)
            .await
            .unwrap()
            .read(queue::state_counts)
            .unwrap()
            .pending,
        0
    );
    let mut confirmed = body;
    confirmed["estimateOnly"] = false.into();
    confirmed["confirmToken"] = preview["confirmToken"].clone();
    let mut request = post_json("/api/v1/ai/analyze", confirmed.to_string());
    request
        .headers_mut()
        .insert("idempotency-key", "web-mixed-confirm".parse().unwrap());
    let response = send(&app, with_session(request, &cookie)).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let result = response_json(response).await;
    assert_eq!(result["enqueued"], 2);
    assert_eq!(result["estimate"]["outputTokens"], 768 + 2048);
}
