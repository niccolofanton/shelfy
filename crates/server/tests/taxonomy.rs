//! Review routes through authentication/CSRF, library isolation and ETags.
mod support;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use shelfy_core::repo::posts;
use shelfy_core::tags::{aliases, clusters};
use support::auth::{owner, sign_in, spa, with_session};
use support::library::{ALICE, BOB};
use support::{TestState, from_app, get, json as body_json, send};
fn pair(a: &str, c: &str) -> aliases::AliasPair {
    aliases::AliasPair {
        alias_norm: a.into(),
        alias_form: a.into(),
        canonical_norm: c.into(),
        canonical_form: c.into(),
    }
}
fn group(label: &str, tags: &[&str]) -> clusters::RefinedGroup {
    clusters::RefinedGroup {
        label: label.into(),
        tags: tags.iter().map(|s| (*s).into()).collect(),
    }
}
fn request(method: &str, url: &str, body: Value) -> Request<Body> {
    from_app(
        Request::builder()
            .method(method)
            .uri(url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
}
async fn seed(t: &TestState, user: &str) {
    t.write(user,|tx| {
        tx.execute("INSERT INTO posts (id,key,platform,native_id,media_type,imported_at,sort_ts,updated_at) VALUES (1,'ig_1','instagram','1','image',1,1,1)",[])?;
        posts::update_ai(tx,1,&posts::AiPatch {tags:Some(Some(vec!["lamps".into(),"b".into(),"lamp".into()])),..Default::default()},1)?;
        posts::update_user_content(tx,1,&posts::UserContentPatch {tags:Some(vec!["lamps".into()]),..Default::default()},1)?;
        clusters::save_run(tx,&[group("Lighting",&["lamps","b"])],1,1)?;
        aliases::save_proposals(tx,&[pair("lamps","lamp")],1)?;
        Ok(())
    }).await;
}
#[tokio::test]
async fn review_routes_deny_anonymous_and_are_scoped_to_the_authenticated_library() {
    let t = TestState::new();
    let user = owner(&t);
    seed(&t, &user).await;
    seed(&t, BOB).await;
    let routes = [
        ("GET", "/api/v1/tag-clusters"),
        ("GET", "/api/v1/tag-aliases"),
        ("PATCH", "/api/v1/tag-clusters/1"),
        ("DELETE", "/api/v1/tag-clusters/1"),
        ("DELETE", "/api/v1/tag-clusters/1/tags/lamps"),
        ("POST", "/api/v1/tag-aliases/lamps/accept"),
        ("POST", "/api/v1/tag-aliases/lamps/dismiss"),
        ("POST", "/api/v1/tag-aliases/accept-all"),
    ];
    for (method, url) in routes {
        assert_eq!(
            send(&t.app(), request(method, url, json!({"status":"accepted"})))
                .await
                .status(),
            StatusCode::UNAUTHORIZED,
            "{method} {url}"
        );
    }
    let cookie = sign_in(&t.app(), &t).await;
    let token = format!(
        "shx_{}",
        shelfy_server::tokens::SecretToken::generate().expose()
    );
    support::auth::control_db(&t).execute(
        "INSERT INTO api_tokens (id,user_id,kind,token_hash,scopes,created_at) VALUES (?1,?2,'extension',?3,'ingest lookup',1)",
        rusqlite::params![shelfy_server::ids::new_ulid(),user,shelfy_server::tokens::hash_token(&token).as_slice()],
    ).unwrap();
    for (method, url) in routes {
        let mut req = with_session(request(method, url, json!({"status":"accepted"})), &cookie);
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        assert_eq!(
            send(&t.app(), req).await.status(),
            StatusCode::UNAUTHORIZED,
            "token: {method} {url}"
        );
    }
    assert_eq!(
        send(
            &t.app(),
            with_session(
                Request::patch("/api/v1/tag-clusters/1")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"status\":\"accepted\"}"))
                    .unwrap(),
                &cookie
            )
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let response = send(&t.app(), with_session(get("/api/v1/tag-clusters"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["items"][0]["label"], "Lighting");
    let response = send(
        &t.app(),
        spa(
            &t,
            request(
                "PATCH",
                "/api/v1/tag-clusters/1",
                json!({"label":"Owner reviewed","status":"accepted"}),
            ),
            &cookie,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bob = t.state.user_db(BOB).await.unwrap();
    assert_eq!(
        bob.read(|c| clusters::list(c, 24)).unwrap()[0].label,
        "Lighting"
    );
    assert_eq!(
        send(&t.app_as(ALICE), get("/api/v1/tag-clusters"))
            .await
            .status(),
        StatusCode::OK
    );
    assert!(
        body_json(send(&t.app_as(ALICE), get("/api/v1/tag-clusters")).await).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        send(
            &t.app_as(ALICE),
            request(
                "PATCH",
                "/api/v1/tag-clusters/1",
                json!({"status":"accepted"})
            )
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &t.app_as(ALICE),
            request("POST", "/api/v1/tag-aliases/lamps/accept", json!({}))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn alias_accept_invalidates_gets_preserves_json_and_later_writes_use_the_canonical() {
    let t = TestState::new();
    seed(&t, ALICE).await;
    let app = t.app_as(ALICE);
    let first = send(&app, get("/api/v1/tag-aliases?status=proposed")).await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers()[header::ETAG].clone();
    assert_eq!(body_json(first).await["items"][0]["count"], 1);
    let conditional = Request::get("/api/v1/tag-aliases?status=proposed")
        .header(header::IF_NONE_MATCH, etag.clone())
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, conditional).await.status(),
        StatusCode::NOT_MODIFIED
    );
    let accepted = send(
        &app,
        request("POST", "/api/v1/tag-aliases/lamps/accept", json!({})),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(body_json(accepted).await["accepted"], 1);
    let changed = send(
        &app,
        Request::get("/api/v1/tag-aliases?status=proposed")
            .header(header::IF_NONE_MATCH, etag.clone())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_ne!(changed.headers()[header::ETAG], etag);
    assert!(
        body_json(changed).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let db = t.state.user_db(ALICE).await.unwrap();
    assert_eq!(
        db.read::<_, shelfy_core::repo::RepoError>(|c| Ok(c.query_row(
            "SELECT ai_tags_json FROM posts WHERE id=1",
            [],
            |r| r.get::<_, String>(0)
        )?))
        .unwrap(),
        "[\"lamps\",\"b\",\"lamp\"]"
    );
    t.write(ALICE, |tx| {
        posts::update_user_content(
            tx,
            1,
            &posts::UserContentPatch {
                tags: Some(vec!["lamps".into()]),
                ..Default::default()
            },
            2,
        )?;
        Ok(())
    })
    .await;
    assert_eq!(
        db.read::<_, shelfy_core::repo::RepoError>(|c| Ok(c.query_row(
            "SELECT tag_norm FROM post_tags WHERE post_id=1 AND source='manual'",
            [],
            |r| r.get::<_, String>(0)
        )?))
        .unwrap(),
        "lamp"
    );
    assert_eq!(
        send(
            &app,
            request("POST", "/api/v1/tag-aliases/lamps/dismiss", json!({}))
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        body_json(
            send(
                &app,
                request("POST", "/api/v1/tag-aliases/accept-all", json!({}))
            )
            .await
        )
        .await["accepted"],
        0
    );
    assert_eq!(
        send(&app, get("/api/v1/tag-aliases?status=unknown"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn cluster_review_etags_limits_and_stale_decisions() {
    let t = TestState::new();
    seed(&t, ALICE).await;
    let app = t.app_as(ALICE);
    let before = send(&app, get("/api/v1/tag-clusters")).await;
    let etag = before.headers()[header::ETAG].clone();
    assert_eq!(body_json(before).await["items"][0]["postCount"], 1);
    assert_eq!(
        send(
            &app,
            Request::get("/api/v1/tag-clusters")
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap()
        )
        .await
        .status(),
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(
        send(
            &app,
            request("PATCH", "/api/v1/tag-clusters/1", json!({"label":"  "}))
        )
        .await
        .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(&app, request("PATCH", "/api/v1/tag-clusters/1", json!({})))
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(
            &app,
            request("DELETE", "/api/v1/tag-clusters/1/tags/b", json!({}))
        )
        .await
        .status(),
        StatusCode::OK
    );
    let changed = send(&app, get("/api/v1/tag-clusters")).await;
    assert_ne!(changed.headers()[header::ETAG], etag);
    assert_eq!(
        body_json(changed).await["items"][0]["tags"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    t.write(ALICE, |tx| {
        clusters::save_run(tx, &[group("New", &["a", "c"])], 2, 2)
    })
    .await;
    assert_eq!(
        send(
            &app,
            request(
                "PATCH",
                "/api/v1/tag-clusters/1",
                json!({"status":"accepted"})
            )
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&app, request("DELETE", "/api/v1/tag-clusters/1", json!({})))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &app,
            request("POST", "/api/v1/tag-aliases/lamps/dismiss", json!({}))
        )
        .await
        .status(),
        StatusCode::OK
    );
    t.write(ALICE, |tx| {
        let groups = (0..30)
            .map(|i| group(&format!("Theme {i}"), &[&format!("a{i}"), &format!("b{i}")]))
            .collect::<Vec<_>>();
        clusters::save_run(tx, &groups, 3, 3)
    })
    .await;
    assert_eq!(
        body_json(send(&app, get("/api/v1/tag-clusters")).await).await["items"]
            .as_array()
            .unwrap()
            .len(),
        24
    );
}
