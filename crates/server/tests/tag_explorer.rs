//! Every taxonomy data route: authentication, CSRF, isolation, ETags and atomic edits.
mod support;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use shelfy_core::repo::{Platform, posts};
use support::auth::{owner, sign_in, spa, with_session};
use support::library::{ALICE, BOB};
use support::{TestState, from_app, get, json as body_json, send};
const GETS: [&str; 7] = [
    "/api/v1/tags/overview",
    "/api/v1/tags",
    "/api/v1/entities",
    "/api/v1/tags/lamp/related",
    "/api/v1/tags/health",
    "/api/v1/tags/merge-suggestions",
    "/api/v1/facets",
];
fn request(method: &str, url: &str, value: Value) -> Request<Body> {
    from_app(
        Request::builder()
            .method(method)
            .uri(url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
    )
}
async fn seed(t: &TestState, user: &str) {
    t.write(user, |tx| {
        for id in 1..=2 {
            let mut p = posts::NewPost::new(
                format!("ig_{id}"),
                Platform::Instagram,
                id.to_string(),
                "image",
                1,
            );
            p.ai = Some(posts::AiLayer {
                status: Some("done".into()),
                tags: vec!["lamp".into(), "lamps".into()],
                specific_tags: Some(vec!["lamps".into()]),
                entities: vec!["Studio".into()],
                language: Some("it".into()),
                ..Default::default()
            });
            p.user_tags = vec!["lamp".into()];
            posts::insert(tx, &p, 1)?;
        }
        Ok(())
    })
    .await;
}
#[tokio::test]
async fn every_route_requires_session_and_writes_require_csrf() {
    let t = TestState::new();
    let user = owner(&t);
    seed(&t, &user).await;
    let writes = [
        ("/api/v1/tags/rename", json!({"from":"lamps","to":"lamp"})),
        (
            "/api/v1/tags/merge",
            json!({"sources":["lamps"],"target":"lamp"}),
        ),
        ("/api/v1/tags/post-keys", json!({"tags":["lamp"]})),
    ];
    for url in GETS {
        assert_eq!(
            send(&t.app(), get(url)).await.status(),
            StatusCode::UNAUTHORIZED,
            "{url}"
        );
    }
    for (url, body) in &writes {
        assert_eq!(
            send(&t.app(), request("POST", url, body.clone()))
                .await
                .status(),
            StatusCode::UNAUTHORIZED,
            "{url}"
        );
    }
    let cookie = sign_in(&t.app(), &t).await;
    let token = format!(
        "shx_{}",
        shelfy_server::tokens::SecretToken::generate().expose()
    );
    support::auth::control_db(&t).execute("INSERT INTO api_tokens(id,user_id,kind,token_hash,scopes,created_at) VALUES(?1,?2,'extension',?3,'ingest lookup',1)",rusqlite::params![shelfy_server::ids::new_ulid(),user,shelfy_server::tokens::hash_token(&token).as_slice()]).unwrap();
    for url in GETS {
        let mut r = with_session(get(url), &cookie);
        r.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        assert_eq!(
            send(&t.app(), r).await.status(),
            StatusCode::UNAUTHORIZED,
            "{url}"
        );
    }
    for (url, body) in writes {
        assert_eq!(
            send(
                &t.app(),
                with_session(
                    Request::post(url)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                    &cookie
                )
            )
            .await
            .status(),
            StatusCode::FORBIDDEN,
            "{url}"
        );
        let mut r = spa(&t, request("POST", url, body.clone()), &cookie);
        r.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        assert_eq!(
            send(&t.app(), r).await.status(),
            StatusCode::UNAUTHORIZED,
            "{url}"
        );
        assert_eq!(
            send(&t.app(), spa(&t, request("POST", url, body), &cookie))
                .await
                .status(),
            StatusCode::OK,
            "{url}"
        );
    }
}
#[tokio::test]
async fn all_reads_have_etags_isolation_and_generation_cache_invalidates() {
    let t = TestState::new();
    seed(&t, ALICE).await;
    seed(&t, BOB).await;
    let app = t.app_as(ALICE);
    let mut before = vec![];
    for url in GETS {
        let r = send(&app, get(url)).await;
        assert_eq!(r.status(), StatusCode::OK, "{url}");
        let etag = r.headers()[header::ETAG].clone();
        let r = send(
            &app,
            Request::get(url)
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_MODIFIED, "{url}");
        assert_eq!(r.headers()[header::ETAG], etag);
        let bob = send(&t.app_as(BOB), get(url)).await;
        assert_ne!(bob.headers()[header::ETAG], etag);
        before.push((url, etag));
    }
    let r = send(
        &app,
        request(
            "POST",
            "/api/v1/tags/rename",
            json!({"from":"lamps","to":"lightfixture"}),
        ),
    )
    .await;
    assert_eq!(body_json(r).await["updated"], 2);
    for (url, etag) in &before {
        let r = send(
            &app,
            Request::get(*url)
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK, "{url}");
        assert_ne!(r.headers()[header::ETAG], *etag);
    }
    let merges = body_json(send(&app, get("/api/v1/tags/merge-suggestions")).await).await;
    assert!(
        merges["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["canonical"] != "lamps")
    );
    let bob = body_json(send(&t.app_as(BOB), get("/api/v1/tags/merge-suggestions")).await).await;
    assert_eq!(bob["items"][0]["canonical"], "lamp");
    assert_eq!(bob["items"][0]["variants"], json!(["lamps"]));
    let keys = body_json(
        send(
            &app,
            request(
                "POST",
                "/api/v1/tags/post-keys",
                json!({"tags":["lamp","lightfixture"],"mode":"and"}),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(keys, json!({"keys":["ig_1","ig_2"],"truncated":false}));
    let etag = send(&app, get("/api/v1/tags")).await.headers()[header::ETAG].clone();
    let r = send(
        &app,
        request(
            "POST",
            "/api/v1/tags/merge",
            json!({"sources":["absent"],"target":"empty"}),
        ),
    )
    .await;
    assert_eq!(body_json(r).await["updated"], 0);
    assert_eq!(
        send(&app, get("/api/v1/tags")).await.headers()[header::ETAG],
        etag
    );
}
#[tokio::test]
async fn failed_merge_and_invalid_input_preserve_generation_and_json() {
    let t = TestState::new();
    seed(&t, ALICE).await;
    let app = t.app_as(ALICE);
    t.write(ALICE,|tx|{tx.execute_batch("CREATE TRIGGER fail_merge BEFORE UPDATE OF ai_tags_json ON posts WHEN NEW.id=2 BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;")?;Ok(())}).await;
    let etag = send(&app, get("/api/v1/tags")).await.headers()[header::ETAG].clone();
    let r = send(
        &app,
        request(
            "POST",
            "/api/v1/tags/merge",
            json!({"sources":["lamps"],"target":"replacement"}),
        ),
    )
    .await;
    assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        send(&app, get("/api/v1/tags")).await.headers()[header::ETAG],
        etag
    );
    let db = t.state.user_db(ALICE).await.unwrap();
    assert_eq!(
        db.read::<_, shelfy_core::repo::RepoError>(|c| Ok(c.query_row(
            "SELECT ai_tags_json FROM posts WHERE id=1",
            [],
            |r| r.get::<_, String>(0)
        )?))
        .unwrap(),
        "[\"lamp\",\"lamps\"]"
    );
    for (url, body) in [
        ("/api/v1/tags/rename", json!({"from":"lamp","to":" "})),
        ("/api/v1/tags/merge", json!({"sources":[],"target":"x"})),
        ("/api/v1/tags/post-keys", json!({"tags":vec!["x";501]})),
    ] {
        assert_eq!(
            send(&app, request("POST", url, body)).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{url}"
        );
    }
    for url in [
        "/api/v1/tags?limit=501",
        "/api/v1/entities?limit=0",
        "/api/v1/tags/merge-suggestions?limit=101",
    ] {
        assert_eq!(
            send(&app, get(url)).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{url}"
        );
    }
    assert_eq!(
        send(&app, get("/api/v1/tags?tier=unknown")).await.status(),
        StatusCode::BAD_REQUEST
    );
}

/// Full admin-synth library; excludes media delivery, times the real HTTP GETs.
#[cfg(not(debug_assertions))]
#[tokio::test]
#[ignore = "release acceptance benchmark: 20k synthetic posts, ai-share 0.7"]
async fn release_tag_gets_p95_on_20k_synthetic_topic_library() {
    let t = TestState::new();
    let data = t.data_dir();
    let report = tokio::task::spawn_blocking(move || {
        shelfy_server::admin::synth::synth(
            &data,
            ALICE,
            &shelfy_server::admin::synth::SynthOptions {
                posts: 20000,
                profile: shelfy_server::admin::synth::Profile::Reference,
                seed: 7,
                ai_share: 0.7,
            },
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!((13500..14500).contains(&report.with_ai));
    let app = t.app_as(ALICE);
    println!(
        "synthetic posts={} analyzed={}",
        report.posts, report.with_ai
    );
    for url in GETS {
        let mut samples = vec![];
        let mut cold = 0.0;
        for i in 0..32 {
            let start = std::time::Instant::now();
            let r = send(&app, get(url)).await;
            assert_eq!(r.status(), StatusCode::OK, "{url}");
            let _ = body_json(r).await;
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            if i == 0 {
                cold = ms;
            }
            if i >= 2 {
                samples.push(ms);
            }
        }
        samples.sort_by(f64::total_cmp);
        let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
        println!("{url} cold={cold:.3}ms p95={p95:.3}ms");
        assert!(p95 <= 60.0, "{url} p95={p95:.3}ms >60ms");
    }
}

#[tokio::test]
async fn cache_parameters_are_normalized_and_oversized_health_is_not_retained() {
    let t = TestState::new();
    seed(&t, ALICE).await;
    let app = t.app_as(ALICE);
    let etag = send(&app, get("/api/v1/tags")).await.headers()[header::ETAG].clone();
    assert_eq!(
        send(&app, get("/api/v1/tags?tier=all&limit=200"))
            .await
            .headers()[header::ETAG],
        etag
    );
    assert_eq!(
        body_json(send(&app, get("/api/v1/tags?limit=1")).await).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let manual = body_json(send(&app, get("/api/v1/tags?tier=manual")).await).await;
    assert_eq!(manual["items"].as_array().unwrap().len(), 1);
    assert_eq!(manual["items"][0]["tag"], "lamp");
    assert_eq!(
        body_json(send(&app, get("/api/v1/tags/absent/related")).await).await["items"],
        json!([])
    );
    t.write(ALICE, |tx| {
        for i in 0..1100 {
            let tag = format!("rare{i:04}{}", "a".repeat(990));
            tx.execute(
                "INSERT INTO post_tags(post_id,tag_norm,tag_form,source) VALUES(1,?1,?1,'ai')",
                [tag],
            )?;
        }
        Ok(())
    })
    .await;
    let response = send(&app, get("/api/v1/tags/health")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = support::body(response).await;
    assert!(bytes.len() > 1024 * 1024);
    let db = t.state.user_db(ALICE).await.unwrap();
    assert!(
        t.state
            .library_caches()
            .tag_views
            .get(
                ALICE,
                db.generation(),
                &shelfy_server::library::view_digest("tags.health", &())
            )
            .is_none()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["orphanTags"]
            .as_array()
            .unwrap()
            .len(),
        1100
    );
}
