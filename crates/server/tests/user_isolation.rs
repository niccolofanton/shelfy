//! `admin create-user` (X2, E6) and the isolation it is for: a mock account
//! on the live host is a regular member, not the owner, and every per-user
//! route must keep two members apart exactly as it keeps a member apart
//! from the owner today.
//!
//! - [`create_user_is_for_members_only_and_is_idempotent`][]: the command
//!   itself, in process (refuses the owner's email, idempotent, stores the
//!   display name, writes an audit row with no email in it, creates an
//!   empty library, never touches the single-owner invariant).
//! - [`the_rest_of_the_admin_cli_takes_a_member_too`][]: `login-link`,
//!   `migrate-token`, `synth` and `bench` already resolve any active
//!   account by id or email (no owner-only gate), so they work unchanged
//!   for a user `create-user` made.
//! - [`two_members_never_see_each_others_posts_media_collections_jobs_or_events`][]:
//!   the end-to-end check the card asks for. Two members are created with
//!   `create-user`, signed in for real (a login link redeemed like the SPA
//!   does, not the `app_as` test stand-in), and one can reach none of the
//!   other's posts, media, collections, jobs or events; the owner alone
//!   keeps the `admin` capability.

mod support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use shelfy_core::repo::Platform;
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::posts::{self, NewPost};
use shelfy_media::store::{IngestLimits, MediaStore};
use shelfy_server::admin::bench;
use shelfy_server::admin::create_user::{CreateUserOutcome, create_user};
use shelfy_server::admin::login_link::create_login_link;
use shelfy_server::admin::migrate_token::create_migrate_token;
use shelfy_server::admin::owner::create_owner;
use shelfy_server::admin::synth::{self, Profile, SynthOptions};
use shelfy_server::auth::cookie::SESSION_COOKIE;
use shelfy_server::error::ErrorCode;
use shelfy_server::events::model::ChangeReason;
use shelfy_server::ids::now_ms;
use shelfy_server::jobs::Registry;
use support::auth::{control_db, sign_in_as, with_session};
use support::jobs::{Probe, kind};
use support::sse::Stream;
use support::{TestState, from_app, get, json, problem, send};

const OWNER_EMAIL: &str = "owner@example.test";
const EMAIL_A: &str = "mock-a@example.test";
const EMAIL_B: &str = "mock-b@example.test";

fn state() -> (TestState, Probe) {
    let probe = Probe::new();
    let t = TestState::with_jobs(Registry::new().register(kind("test.iso", 1, 1, &probe)));
    (t, probe)
}

/// An empty `POST`, as the web app sends it (CSRF headers, no body).
fn post(uri: &str) -> Request<Body> {
    from_app(Request::post(uri).body(Body::empty()).unwrap())
}

/// A `DELETE`, as the web app sends it.
fn delete(uri: &str) -> Request<Body> {
    from_app(Request::delete(uri).body(Body::empty()).unwrap())
}

#[tokio::test(start_paused = true)]
async fn create_user_is_for_members_only_and_is_idempotent() {
    let (t, _probe) = state();
    let data = t.data_dir();
    let owner = create_owner(&data, OWNER_EMAIL).unwrap();

    // Refuses the owner's email, whatever its case or spacing.
    let err = create_user(&data, " Owner@Example.TEST ", None).unwrap_err();
    assert!(err.to_string().contains("owner's email"), "{err:#}");
    let control = control_db(&t);
    let users: i64 = control
        .query_row("SELECT count(*) FROM users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(users, 1, "the refused call created nothing");

    // Creates a member, with its display name, its empty library, and an
    // audit row that names no email.
    let created = create_user(&data, " A@Example.TEST ", Some("Mock A")).unwrap();
    let CreateUserOutcome::Created(id_a) = created.clone() else {
        panic!("expected a new member, got {created:?}");
    };
    assert_ne!(id_a, owner.user_id());
    let (email, display_name, role, quota): (String, Option<String>, String, i64) = control
        .query_row(
            "SELECT email, display_name, role, quota_bytes FROM users WHERE id = ?1",
            [&id_a],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(email, "a@example.test");
    assert_eq!(display_name.as_deref(), Some("Mock A"));
    assert_eq!(role, "member");
    assert_eq!(
        quota,
        5 * 1024 * 1024 * 1024,
        "the §4.2 default member quota"
    );
    let (action, target, meta): (String, String, String) = control
        .query_row(
            "SELECT action, target, meta_json FROM audit_log WHERE action = 'user.create'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(action, "user.create");
    assert_eq!(target, id_a);
    assert_eq!(meta, r#"{"via":"admin"}"#);
    assert!(!meta.contains("example.test"), "no email in the audit log");
    assert!(data.library_db(&id_a).is_file(), "the library exists");

    // Idempotent for the same email: a rerun, even with a different display
    // name, changes nothing.
    let again = create_user(&data, "a@example.test", Some("Ignored")).unwrap();
    assert_eq!(again, CreateUserOutcome::AlreadyExists(id_a.clone()));
    let unchanged: Option<String> = control
        .query_row(
            "SELECT display_name FROM users WHERE id = ?1",
            [&id_a],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        unchanged.as_deref(),
        Some("Mock A"),
        "a rerun changes nothing"
    );

    // A second, distinct member: still not the owner, no single-owner
    // conflict.
    let other = create_user(&data, EMAIL_B, None).unwrap();
    let CreateUserOutcome::Created(id_b) = other else {
        panic!("expected a new member");
    };
    assert_ne!(id_b, id_a);
    let owners: i64 = control
        .query_row("SELECT count(*) FROM users WHERE role = 'owner'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(owners, 1, "create-user never adds an owner");
}

#[tokio::test]
async fn the_rest_of_the_admin_cli_takes_a_member_too() {
    let t = TestState::new();
    let data = t.data_dir();
    create_owner(&data, OWNER_EMAIL).unwrap();
    let CreateUserOutcome::Created(member) = create_user(&data, EMAIL_A, None).unwrap() else {
        panic!("expected a new member");
    };

    // `admin login-link --email` mints a link for the member, not only the
    // owner.
    let link = create_login_link(
        &data,
        &t.state.config().public_url,
        EMAIL_A,
        Duration::from_secs(900),
    )
    .unwrap();
    assert!(link.url.expose().contains("/login/magic#"));

    // `admin migrate-token --email` mints a `migrate`-scoped token for the
    // member.
    let token = create_migrate_token(&data, EMAIL_A, Duration::from_secs(7 * 86_400)).unwrap();
    assert!(!token.token.expose().is_empty());

    // `admin synth` fills the member's (empty) library, exactly as it fills
    // the owner's.
    let report = synth::synth(
        &data,
        &member,
        &SynthOptions {
            posts: 50,
            profile: Profile::Reference,
            seed: 7,
            ai_share: 0.0,
        },
    )
    .unwrap();
    assert_eq!(report.posts, 50);

    // `admin bench` times the member's library the same way (the core
    // function `bench --user` calls; no owner-only gate anywhere on this
    // path).
    let bench_report = bench::bench(&t.state, &member, 10, 1).await.unwrap();
    assert_eq!(bench_report.posts, 50);
}

#[tokio::test(start_paused = true)]
async fn two_members_never_see_each_others_posts_media_collections_jobs_or_events() {
    let (t, _probe) = state();
    let data = t.data_dir();
    create_owner(&data, OWNER_EMAIL).unwrap();
    let CreateUserOutcome::Created(id_a) = create_user(&data, EMAIL_A, Some("Mock A")).unwrap()
    else {
        panic!("expected a new member");
    };
    let CreateUserOutcome::Created(id_b) = create_user(&data, EMAIL_B, Some("Mock B")).unwrap()
    else {
        panic!("expected a new member");
    };

    // Seed A's library: a post, a collection holding it, and a stored media
    // file. None of this is Bob's.
    let post_key = "ig_a_only";
    let collection_a = t
        .write(&id_a, |tx| {
            let mut post = NewPost::new(post_key, Platform::Instagram, "a_only", "image", now_ms());
            post.caption = Some("only Mock A can read this".into());
            posts::insert(tx, &post, now_ms())?;
            let folder = collections::create(
                tx,
                &NewCollection {
                    name: "Mock A's folder".into(),
                    ..NewCollection::default()
                },
                now_ms(),
            )?;
            collections::add_posts(
                tx,
                &[posts::id_for_key(tx, post_key)?.unwrap()],
                &[folder.id],
                now_ms(),
            )?;
            Ok::<_, shelfy_core::repo::RepoError>(folder.id)
        })
        .await;
    // A minimal JPEG: the ingest path sniffs the content, so a few magic
    // bytes are needed, not just any content.
    let jpeg_bytes = [
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xD9,
    ];
    let media_a = MediaStore::new(data.users_dir())
        .user(&id_a)
        .unwrap()
        .ingest(&jpeg_bytes[..], IngestLimits::UPLOAD)
        .unwrap()
        .publish()
        .unwrap();
    let media_url = format!("/media/{}", media_a.name());
    let job_a = t.enqueue(&id_a, "test.iso", json!({ "mode": "ok" })).await;

    // Sign in both for real: a login link, redeemed like the SPA does.
    let app = t.app();
    let cookie_a = sign_in_as(&app, &t, EMAIL_A).await;
    let cookie_b = sign_in_as(&app, &t, EMAIL_B).await;
    assert_ne!(cookie_a, cookie_b);

    // Posts: Bob's own library has no such key.
    let response = send(
        &app,
        with_session(get(&format!("/api/v1/posts/{post_key}")), &cookie_b),
    )
    .await;
    assert_eq!(
        problem(response, StatusCode::NOT_FOUND).await.code,
        ErrorCode::NotFound,
        "a post of Mock A's"
    );
    // Mock A reads it fine.
    let own = send(
        &app,
        with_session(get(&format!("/api/v1/posts/{post_key}")), &cookie_a),
    )
    .await;
    assert_eq!(own.status(), StatusCode::OK);

    // Media: no user id appears in the URL, and the file is read from the
    // authenticated user's own store only (§7.1).
    let response = send(&app, with_session(get(&media_url), &cookie_b)).await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Bob reading Mock A's media"
    );
    let response = send(&app, with_session(get(&media_url), &cookie_a)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Mock A reading her own media"
    );

    // Collections: absent from Bob's listing, and a direct delete is a 404,
    // like a missing one.
    let listing = json(send(&app, with_session(get("/api/v1/collections"), &cookie_b)).await).await;
    let names: Vec<&str> = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"Mock A's folder"),
        "Bob's listing must not carry Mock A's collection: {names:?}"
    );
    let response = send(
        &app,
        with_session(
            delete(&format!("/api/v1/collections/{collection_a}")),
            &cookie_b,
        ),
    )
    .await;
    assert_eq!(
        problem(response, StatusCode::NOT_FOUND).await.code,
        ErrorCode::NotFound,
        "deleting Mock A's collection as Bob"
    );

    // Jobs: another user's job id is as unreachable as a missing one, and
    // never appears in a listing.
    let response = send(
        &app,
        with_session(post(&format!("/api/v1/jobs/{job_a}/cancel")), &cookie_b),
    )
    .await;
    assert_eq!(
        problem(response, StatusCode::NOT_FOUND).await.code,
        ErrorCode::NotFound,
        "cancelling Mock A's job as Bob"
    );
    let jobs_b = json(send(&app, with_session(get("/api/v1/jobs"), &cookie_b)).await).await;
    assert_eq!(jobs_b["items"], json!([]), "Bob's own queue is empty");

    // Events: Bob's stream never carries Mock A's events, even published
    // while he is listening.
    let mut bob_stream = Stream::connect(
        &app,
        "/api/v1/events",
        &[("cookie", &format!("{SESSION_COOKIE}={cookie_b}"))],
    )
    .await;
    bob_stream.hello().await;
    t.state
        .events()
        .posts_changed(&id_a, ChangeReason::Edit, Some(vec![post_key.to_owned()]));
    let next = bob_stream.next().await;
    assert!(
        next.is_heartbeat(),
        "Bob got none of Mock A's event: {next:?}"
    );

    // `GET /me`: both are plain members, neither gets the owner's `admin`
    // capability; the owner alone keeps it.
    for cookie in [&cookie_a, &cookie_b] {
        let me = json(send(&app, with_session(get("/api/v1/me"), cookie)).await).await;
        assert_eq!(me["role"], "member");
        assert_eq!(me["capabilities"]["admin"], false);
    }
    let owner_id: String = {
        let control = control_db(&t);
        control
            .query_row("SELECT id FROM users WHERE role = 'owner'", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let owner_cookie = sign_in_as(&app, &t, OWNER_EMAIL).await;
    let me_owner = json(send(&app, with_session(get("/api/v1/me"), &owner_cookie)).await).await;
    assert_eq!(me_owner["id"], owner_id);
    assert_eq!(me_owner["role"], "owner");
    assert_eq!(me_owner["capabilities"]["admin"], true);

    // Bob's own library never saw Mock A's post or collection either (not
    // merely hidden behind an authz check: the row itself only ever existed
    // in A's library file).
    let bob_has_none = t
        .write(&id_b, |tx| {
            Ok::<_, shelfy_core::repo::RepoError>((
                posts::get(tx, post_key)?.is_none(),
                collections::list(tx)?.is_empty(),
            ))
        })
        .await;
    assert_eq!(bob_has_none, (true, true));
}
