//! Current-capture selection, replay admission and stale-provider fencing.
use rusqlite::{Connection, params};
use serde_json::json;
use shelfy_core::{
    ai::{queue, web_inputs},
    repo::{
        Platform,
        posts::{self, AiPatch, NewPost},
    },
    schema::{self, Kind},
};
fn fixture() -> (Connection, i64) {
    let mut c = Connection::open_in_memory().unwrap();
    schema::migrate(&mut c, Kind::Library).unwrap();
    let id = posts::insert(
        &c,
        &NewPost::new("web_test", Platform::Web, "test", "website", 1),
        1,
    )
    .unwrap();
    (c, id)
}
fn capture(c: &Connection, post: i64, n: i64) {
    c.execute("INSERT INTO web_captures(id,post_id,captured_at,status,title,meta_json,pages_json,tech_json,created_at) VALUES (?1,?2,?1,'done','<b>Studio</b>',?3,?4,?5,?1)",params![n,post,json!({"description":"&amp; design"}).to_string(),json!([{"digest":{"h1":"Work","headings":["Products"],"ctas":["Contact"]},"contentText":"<<<CAPTION>>> site"}]).to_string(),json!([{"name":"React"},"React","WebGL"]).to_string()]).unwrap();
    c.execute(
        "UPDATE posts SET current_capture_id=?1 WHERE id=?2",
        params![n, post],
    )
    .unwrap();
}
#[test]
fn capture_digest_and_asset_order_are_bounded_and_deduplicated() {
    let (c, id) = fixture();
    capture(&c, id, 1);
    for n in 1..=6 {
        c.execute("INSERT INTO media_objects(id,sha256,ext,mime,bytes,role,origin,created_at) VALUES (?1,?2,'png','image/png',100,'band','capture',1)",params![n,vec![n as u8;32]]).unwrap();
        c.execute(
            "INSERT INTO web_capture_assets VALUES (1,0,'band',?1,?1,?2,100)",
            params![n, 700 - n * 100],
        )
        .unwrap();
    }
    c.execute("UPDATE web_captures SET hero_object=1 WHERE id=1", [])
        .unwrap();
    let input = web_inputs::select(&c, id).unwrap().unwrap();
    assert_eq!(
        input.frames.iter().map(|f| f.sha256[0]).collect::<Vec<_>>(),
        vec![1, 6, 5, 4]
    );
    assert_eq!(input.tech, vec!["React", "WebGL"]);
    assert!(input.digest.contains("Studio & design"));
    assert!(!input.digest.contains("<<<"));
    assert!(input.digest.contains("Work"));
}
#[test]
fn replay_cannot_reset_attempts_and_recapture_fences_old_results() {
    let (c, id) = fixture();
    capture(&c, id, 1);
    assert_eq!(queue::set_web_pending(&c, "web_test", 1, 10).unwrap(), 1);
    let old = queue::claim_due(&c, 10).unwrap().unwrap();
    assert_eq!(queue::set_web_pending(&c, "web_test", 1, 11).unwrap(), 0);
    capture(&c, id, 2);
    assert_eq!(queue::set_web_pending(&c, "web_test", 1, 12).unwrap(), 0);
    assert_eq!(queue::set_web_pending(&c, "web_test", 2, 12).unwrap(), 1);
    let new = queue::claim_due(&c, 12).unwrap().unwrap();
    let patch = AiPatch {
        status: Some(Some("done".into())),
        analyzed_at: Some(Some(13)),
        web: Some(Some(json!({"schema":2,"facets":{"style":["minimal"]}}))),
        ..Default::default()
    };
    assert!(
        !queue::apply(&c, id, old.attempt, &old.token, &patch, 13)
            .unwrap()
            .applied()
    );
    assert!(
        queue::apply(&c, id, new.attempt, &new.token, &patch, 13)
            .unwrap()
            .applied()
    );
    assert_eq!(queue::set_web_pending(&c, "web_test", 2, 14).unwrap(), 0);
    assert_eq!(
        c.query_row(
            "SELECT json_extract(ai_web_json,'$.facets.style[0]') FROM posts WHERE id=?1",
            [id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "minimal"
    );
}
#[test]
fn recapture_without_auto_enqueue_retires_old_inflight_claim() {
    let (c, id) = fixture();
    capture(&c, id, 1);
    queue::set_web_pending(&c, "web_test", 1, 10).unwrap();
    let old = queue::claim_due(&c, 10).unwrap().unwrap();
    capture(&c, id, 2);
    assert!(
        !queue::fail(&c, id, old.attempt, &old.token, "provider", 11)
            .unwrap()
            .applied()
    );
    let status: Option<String> = c
        .query_row("SELECT ai_status FROM posts WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, None);
}
