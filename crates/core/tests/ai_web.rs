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

#[test]
fn digest_budget_and_p4_metadata_wrapper_are_preserved() {
    let (c, id) = fixture();
    capture(&c, id, 1);
    c.execute("UPDATE web_captures SET pages_json=?1,meta_json=?2,fonts_json=?3,traits_json=?4 WHERE id=1",params![json!([{"contentText":"word ".repeat(5000)}]).to_string(),json!({"description":"Studio","metadata":{"scheme":"dark","lang":"it","siteName":"Measured studio"}}).to_string(),json!([{"family":"Inter","classification":"sans"}]).to_string(),json!({"webgl":true}).to_string()]).unwrap();
    let input = web_inputs::select(&c, id).unwrap().unwrap();
    assert!(input.digest.encode_utf16().count() <= 8000);
    assert!(input.digest.ends_with('…'));
    assert_eq!(input.post["webMeta"]["scheme"], "dark");
    assert_eq!(input.post["webMeta"]["lang"], "it");
    assert_eq!(input.post["webMeta"]["traits"]["webgl"], true);
    assert_eq!(input.post["webFonts"][0]["family"], "Inter");
    assert!(!shelfy_core::ai::web_design::valid(
        &json!({"site_type":"agency"})
    ));
    assert_eq!(
        queue::scope_counts(
            &c,
            &shelfy_core::selector::Selector::Keys(vec!["web_test".into()]),
            queue::Mode::Missing,
            1,
        )
        .unwrap()
        .analyzable,
        1
    );
}

#[test]
fn mixed_preview_budgets_web_design_and_keeps_social_quotes_unchanged() {
    use shelfy_core::ai::estimate::Estimate;
    assert_eq!(Estimate::of_catalogs(3, 0, None), Estimate::of(3, None));
    let quote = Estimate::of_catalogs(2, 3, Some(1000));
    assert_eq!(quote.posts, 5);
    assert_eq!(quote.output_tokens, 2 * 768 + 3 * 2048);
    assert_eq!(quote.eta_ms, Some(5000));
    assert!(quote.input_tokens > Estimate::of(5, None).input_tokens);
    assert_eq!(Estimate::of_catalogs(0, 0, None).eta_ms, None);
    let (c, id) = fixture();
    capture(&c, id, 1);
    let social = posts::insert(
        &c,
        &NewPost::new("x_quote", Platform::Twitter, "quote", "text", 1),
        1,
    )
    .unwrap();
    assert_eq!(web_inputs::count_ids(&c, &[id, social]).unwrap(), 1);
}
