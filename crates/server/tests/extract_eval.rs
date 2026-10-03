//! P3-24: real DB/CAS/catalog worker/provider/normalization, loopback only.
//! Canned answers test plumbing; their score is not live model quality.
mod support;
use base64::Engine as _;
use serde_json::{Value, json};
use shelfy_ai::stub::{Endpoint, Stub, StubConfig, request_key};
use shelfy_core::ai::{catalog, inputs, queue};
use shelfy_core::repo::{
    Platform,
    media::{self, NewMediaObject},
    posts::{self, NewMedia, NewPost},
};
use shelfy_core::selector::Selector;
use shelfy_media::{
    Digest, MediaKind, Rendition,
    render::{self, RenderSpec},
    store::{IngestLimits, MediaStore},
};
use shelfy_server::{
    ai::{OperatorConfig, queue as server_queue},
    events::model::JobState,
    jobs::{Clock, Registry, ai_drain},
    outbound::OriginAllowlist,
};
use std::{io::Cursor, time::Duration};
use support::{TestState, auth::owner};

const CANNED: &str = include_str!("../../../scripts/extract-eval/web/canned.json");
const EXPECTED: &str = include_str!("../../../scripts/extract-eval/web/expected-raw.json");

async fn fixture() -> (Stub, TestState, String) {
    let stub = Stub::start(StubConfig {
        webp_images: false,
        ..StubConfig::default()
    })
    .await
    .unwrap();
    let state = TestState::with_config(|config| {
        config.outbound.allow_origins =
            OriginAllowlist::parse(&format!("http://{}", stub.addr())).unwrap();
        config.operator = OperatorConfig {
            url: Some(stub.openai_base()),
            model: Some("stub-text".into()),
            vision_model: Some("stub-vision".into()),
            concurrency: 1,
            timeout: Duration::from_secs(60),
            ..OperatorConfig::default()
        };
        config.jobs.registry = Registry::new().register(ai_drain::kind());
        config.jobs.clock = Clock::tokio(support::jobs::START);
    });
    let user = owner(&state);
    (stub, state, user)
}

async fn insert_case(
    state: &TestState,
    user: &str,
    case: &Value,
    index: usize,
) -> (String, i64, Vec<u8>) {
    let count = case["sourceImages"].as_u64().unwrap() as usize;
    let store = MediaStore::new(state.data_dir().users_dir())
        .user(user)
        .unwrap();
    let mut objects = Vec::new();
    let mut colors = Vec::new();
    for n in 0..count {
        let red = ((index * 13 + n * 23) % 200 + 20) as u8;
        colors.push(red);
        let source = image::RgbImage::from_pixel(1280, 960, image::Rgb([red, 40, 180]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .unwrap();
        let stored = store
            .ingest(Cursor::new(bytes.into_inner()), IngestLimits::ARCHIVE_IMAGE)
            .unwrap()
            .publish()
            .unwrap();
        // A distinct green channel proves shallow chose the stored g480 rendition,
        // rather than merely resizing the original to the same dimensions.
        let grid = image::RgbImage::from_pixel(480, 360, image::Rgb([red, 80, 180]));
        let mut grid_bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(grid)
            .write_to(&mut grid_bytes, image::ImageFormat::Png)
            .unwrap();
        let rendered = render::render_bytes(grid_bytes.get_ref(), RenderSpec::G480).unwrap();
        store
            .store_rendition(&stored.digest, Rendition::G480, &rendered.webp)
            .unwrap();
        objects.push(NewMediaObject {
            sha256: *stored.digest.as_bytes(),
            ext: stored.kind.ext().into(),
            mime: stored.kind.mime().into(),
            bytes: stored.size as i64,
            width: Some(1280),
            height: Some(960),
            duration_ms: None,
            role: "image".into(),
            variants: Rendition::G480.bit(),
            origin: "server".into(),
        });
    }
    let key = format!("ig_{}", 90000 + index);
    let post_id = state
        .write(user, |tx| {
            let ids = objects
                .iter()
                .map(|object| media::upsert_object(tx, object, 1))
                .collect::<Result<Vec<_>, _>>()?;
            let media_type = case["mediaType"].as_str().unwrap();
            let mut post = NewPost::new(
                &key,
                Platform::Instagram,
                format!("{}", 90000 + index),
                media_type,
                1,
            );
            post.caption = case["caption"].as_str().map(str::to_owned);
            post.cover_object = ids.first().copied();
            // Cover is also slide one: engine must deduplicate it while retaining order.
            post.media = ids
                .iter()
                .map(|id| NewMedia {
                    kind: if media_type == "video" {
                        "video"
                    } else {
                        "image"
                    }
                    .into(),
                    object_id: Some(*id),
                    ..NewMedia::default()
                })
                .collect();
            posts::insert(tx, &post, 1)
        })
        .await;
    colors.truncate(case["images"].as_u64().unwrap() as usize);
    (key, post_id, colors)
}

async fn profile(deep: bool) {
    let cases: Vec<Value> = serde_json::from_str(CANNED).unwrap();
    let (stub, state, user) = fixture().await;
    let scheduler = state.state.jobs().start(
        state.state.clone(),
        tokio_util::sync::CancellationToken::new(),
    );
    let driver = tokio::spawn(async {
        loop {
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let mut raw = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let (key, post_id, colors) = insert_case(&state, &user, case, index).await;
        let db = state.state.user_db(&user).await.unwrap();
        let (input, hints) = db
            .read(|conn| {
                Ok::<_, shelfy_core::repo::RepoError>((
                    inputs::select(conn, post_id)?.unwrap(),
                    inputs::vocabulary(conn, inputs::VOCABULARY_SIZE)?,
                ))
            })
            .unwrap();
        assert_eq!(input.caption.as_deref(), case["caption"].as_str());
        let expected = catalog::request(
            input.kind,
            input.caption.as_deref(),
            &hints,
            !colors.is_empty(),
        )
        .unwrap();
        let key_hash = request_key(Some(&expected.system), [expected.user.as_str()]);
        stub.add_canned(key_hash.clone(), case["answer"].to_string());
        server_queue::analyze(
            &state.state,
            &user,
            Selector::Keys(vec![key.clone()]),
            queue::Mode::Missing,
            None,
            deep,
        )
        .await
        .unwrap();
        let job = ai_drain::enqueue(state.state.jobs(), &user)
            .await
            .unwrap()
            .job
            .id;
        let completed = state.wait_job(&user, job, |j| j.state.is_final()).await;
        assert_eq!(completed.state, JobState::Succeeded, "{completed:?}");
        let saved = db.read(|conn| posts::get(conn, &key)).unwrap().unwrap();
        assert_eq!(saved.summary.ai_status.as_deref(), Some("done"));
        raw.push(
            json!({"id":case["id"],"mediaType":case["mediaType"],"caption":case["caption"],
            "tags":saved.summary.ai_tags,"keywords":saved.ai_keywords,"entities":saved.ai_entities,
            "description":saved.summary.ai_description.unwrap_or_default(),"error":null}),
        );
        let requests = stub.requests();
        let chats: Vec<_> = requests
            .iter()
            .filter(|r| r.endpoint == Endpoint::Chat)
            .collect();
        assert_eq!(
            chats.len(),
            index + 1,
            "one provider call per case, no retries"
        );
        let request = chats.last().unwrap();
        assert_eq!(request.key.as_deref(), Some(key_hash.as_str()));
        let body = request.body.as_ref().unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            body["response_format"]["json_schema"]["schema"],
            expected.schema.value
        );
        assert_eq!(body["temperature"], expected.temperature);
        assert_eq!(body["max_tokens"], expected.max_tokens);
        let parts = body["messages"][1]["content"].as_array();
        let images: Vec<_> = parts
            .into_iter()
            .flatten()
            .filter(|part| part["type"] == "image_url")
            .collect();
        assert_eq!(
            images.len(),
            colors.len(),
            "cover deduplication and six-image cap"
        );
        for (part, red) in images.into_iter().zip(&colors) {
            let url = part["image_url"]["url"].as_str().unwrap();
            let encoded = url
                .strip_prefix("data:image/jpeg;base64,")
                .expect("engine sends JPEG, never WebP");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(decoded.width(), if deep { 1024 } else { 480 });
            assert_eq!(decoded.height(), if deep { 768 } else { 360 });
            let pixel = decoded.get_pixel(1, 1).0;
            assert!(pixel[0].abs_diff(*red) <= 5, "frame order");
            assert!(
                pixel[1].abs_diff(if deep { 40 } else { 80 }) <= 5,
                "CAS original/g480 profile"
            );
        }
    }
    let output = state.data_dir().root().join("extract-eval-raw.json");
    std::fs::write(&output, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(output).unwrap()).unwrap(),
        serde_json::from_str::<Value>(EXPECTED).unwrap()
    );
    let counts = state
        .state
        .user_db(&user)
        .await
        .unwrap()
        .read(queue::state_counts)
        .unwrap();
    assert_eq!(counts.done, 20);
    driver.abort();
    scheduler.abort().await;
}

#[tokio::test(start_paused = true)]
async fn shallow_g480_pipeline_equals_desktop_raw_contract_for_twenty_cases() {
    profile(false).await;
}
#[tokio::test(start_paused = true)]
async fn approved_1024_poster_pipeline_equals_desktop_raw_contract_for_twenty_cases() {
    profile(true).await;
}

#[tokio::test(start_paused = true)]
async fn missing_video_poster_is_gated_before_any_catalog_provider_request() {
    let (stub, state, user) = fixture().await;
    let case = json!({"mediaType":"video", "caption":"Synthetic video subject #art #design #type #tool #study",
        "sourceImages":0,"images":0});
    let (key, _, _) = insert_case(&state, &user, &case, 30).await;
    let result = server_queue::analyze(
        &state.state,
        &user,
        Selector::Keys(vec![key.clone()]),
        queue::Mode::Missing,
        None,
        true,
    )
    .await
    .unwrap();
    assert_eq!(result.counts.waiting_for_media, 1);
    assert_eq!(result.enqueued, 0);
    assert!(!result.queued);
    let saved = state
        .state
        .user_db(&user)
        .await
        .unwrap()
        .read(|conn| posts::get(conn, &key))
        .unwrap()
        .unwrap();
    assert_eq!(saved.summary.ai_status, None);
    assert!(stub.requests().iter().all(|r| r.endpoint != Endpoint::Chat));
}

#[tokio::test(start_paused = true)]
async fn unreadable_declared_video_poster_fails_without_a_catalog_provider_request() {
    let (stub, state, user) = fixture().await;
    let case = json!({"mediaType":"video", "caption":"Synthetic video subject #art #design #type #tool #study",
        "sourceImages":1,"images":1});
    let (key, post_id, _) = insert_case(&state, &user, &case, 30).await;
    let input = state
        .state
        .user_db(&user)
        .await
        .unwrap()
        .read(|conn| inputs::select(conn, post_id))
        .unwrap()
        .unwrap();
    let inputs::Frame::Image(object) = &input.frames[0] else {
        panic!("synthetic cover must be selected first");
    };
    let digest = Digest::from_bytes(object.sha256.clone().try_into().unwrap());
    let store = MediaStore::new(state.data_dir().users_dir())
        .user(&user)
        .unwrap();
    std::fs::remove_file(store.object_path(&digest, MediaKind::from_ext(&object.ext).unwrap()))
        .unwrap();
    let result = server_queue::analyze(
        &state.state,
        &user,
        Selector::Keys(vec![key.clone()]),
        queue::Mode::Missing,
        None,
        true,
    )
    .await
    .unwrap();
    assert_eq!(result.enqueued, 1);
    let job = ai_drain::enqueue(state.state.jobs(), &user)
        .await
        .unwrap()
        .job
        .id;
    let scheduler = state.state.jobs().start(
        state.state.clone(),
        tokio_util::sync::CancellationToken::new(),
    );
    let driver = tokio::spawn(async {
        loop {
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let completed = state.wait_job(&user, job, |j| j.state.is_final()).await;
    assert_eq!(completed.state, JobState::Succeeded, "{completed:?}");
    let saved = state
        .state
        .user_db(&user)
        .await
        .unwrap()
        .read(|conn| posts::get(conn, &key))
        .unwrap()
        .unwrap();
    assert_eq!(saved.summary.ai_status.as_deref(), Some("error"));
    assert_eq!(saved.ai_error.as_deref(), Some("media_unreadable"));
    assert!(
        stub.requests().iter().all(|r| r.endpoint != Endpoint::Chat),
        "video without readable stills must not silently become a text-only catalog"
    );
    driver.abort();
    scheduler.abort().await;
}
