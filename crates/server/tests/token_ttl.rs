//! F9 account-created token expiry through actual auth, scopes and storage.
mod support;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use shelfy_server::control::api_tokens;
use shelfy_server::error::ErrorCode;
use shelfy_server::ids::now_ms;
use shelfy_server::tokens::hash_token;
use support::auth::{control_db, owner, sign_in, spa, with_session};
use support::library::fixture;
use support::{TestState, get, json, problem, send};
const DAY_MS: i64 = 24 * 3600 * 1_000;
async fn mint(t: &TestState, cookie: &str, data: Value) -> Value {
    let request = Request::post("/api/v1/me/tokens")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(data.to_string()))
        .unwrap();
    let response = send(&t.app(), spa(t, request, cookie)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    json(response).await
}
fn bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}
#[tokio::test]
async fn account_token_default_explicit_bounds_and_legacy_storage() {
    let t = TestState::new();
    let cookie = sign_in(&t.app(), &t).await;
    for kind in ["shortcut", "extension", "library"] {
        for days in [None, Some(1), Some(7), Some(30), Some(90), Some(365)] {
            let mut data = json!({"kind":kind});
            if let Some(days) = days {
                data["ttlDays"] = json!(days);
            }
            let created = mint(&t, &cookie, data).await;
            let row = &created["apiToken"];
            assert_eq!(
                row["expiresAt"].as_i64().unwrap() - row["createdAt"].as_i64().unwrap(),
                i64::from(days.unwrap_or(90)) * DAY_MS
            );
            let list =
                json(send(&t.app(), with_session(get("/api/v1/me/tokens"), &cookie)).await).await;
            assert!(
                !list
                    .to_string()
                    .contains(created["token"].as_str().unwrap())
            );
        }
    }
    for days in [
        json!(0),
        json!(366),
        json!(-1),
        json!(1.5),
        Value::Null,
        json!("90"),
        json!(65536),
    ] {
        let request = Request::post("/api/v1/me/tokens")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"kind":"shortcut","ttlDays":days}).to_string(),
            ))
            .unwrap();
        assert_eq!(
            problem(
                send(&t.app(), spa(&t, request, &cookie)).await,
                StatusCode::UNPROCESSABLE_ENTITY
            )
            .await
            .code,
            ErrorCode::ValidationFailed
        );
    }
    let old = mint(&t, &cookie, json!({"kind":"library"})).await;
    let id = old["apiToken"]["id"].as_str().unwrap();
    control_db(&t)
        .execute("UPDATE api_tokens SET expires_at=NULL WHERE id=?1", [id])
        .unwrap();
    mint(&t, &cookie, json!({"kind":"shortcut","ttlDays":7})).await;
    let list = json(send(&t.app(), with_session(get("/api/v1/me/tokens"), &cookie)).await).await;
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == id && row["expiresAt"].is_null())
    );
}
#[tokio::test]
async fn expiry_boundary_preserves_scopes_and_is_rechecked_after_a_successful_request() {
    let t = TestState::new();
    let cookie = sign_in(&t.app(), &t).await;
    let user = owner(&t);
    t.write(&user, |conn| fixture(conn).map(|_| ())).await;
    let created = mint(&t, &cookie, json!({"kind":"library","ttlDays":7})).await;
    assert_eq!(created["apiToken"]["scopes"], json!(["library:read"]));
    let token = created["token"].as_str().unwrap();
    let hash = hash_token(token);
    let expiry = created["apiToken"]["expiresAt"].as_i64().unwrap();
    let conn = control_db(&t);
    assert!(
        api_tokens::find_active(&conn, &hash, expiry - 1)
            .unwrap()
            .is_some()
    );
    assert!(
        api_tokens::find_active(&conn, &hash, expiry)
            .unwrap()
            .is_none()
    );
    assert!(
        api_tokens::find_active(&conn, &hash, expiry + 1)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        send(&t.app(), bearer(get("/api/v1/posts"), token))
            .await
            .status(),
        StatusCode::OK
    );
    let write = Request::post("/api/v1/collections")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"name":"No write scope"}).to_string()))
        .unwrap();
    assert_eq!(
        send(&t.app(), bearer(write, token)).await.status(),
        StatusCode::FORBIDDEN
    );
    conn.execute(
        "UPDATE api_tokens SET expires_at=?1 WHERE id=?2",
        rusqlite::params![now_ms(), created["apiToken"]["id"].as_str().unwrap()],
    )
    .unwrap();
    assert_eq!(
        send(&t.app(), bearer(get("/api/v1/posts"), token))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &t.app(),
            bearer(with_session(get("/api/v1/posts"), &cookie), token)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = send(&t.app(), with_session(get("/api/v1/me/tokens"), &cookie)).await;
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(json(response).await["items"].as_array().unwrap().is_empty());
    let writer = mint(
        &t,
        &cookie,
        json!({"kind":"library","ttlDays":1,"scopes":["library:write"]}),
    )
    .await;
    assert_eq!(writer["apiToken"]["scopes"], json!(["library:write"]));
    let writer_token = writer["token"].as_str().unwrap();
    assert_eq!(
        send(&t.app(), bearer(get("/api/v1/posts"), writer_token))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let request = Request::post("/api/v1/collections")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"name":"Explicit write scope"}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        send(&t.app(), bearer(request, writer_token)).await.status(),
        StatusCode::CREATED
    );
}
