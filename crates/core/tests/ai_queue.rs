//! Synthetic durable-item lifecycle and concurrency fences.
use rusqlite::Connection;
use shelfy_core::ai::{inputs, queue};
use shelfy_core::repo::{
    Platform,
    posts::{self, AiPatch, NewPost},
};
use shelfy_core::{
    schema::{self, Kind},
    selector::Selector,
};
fn library() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    schema::migrate(&mut db, Kind::Library).unwrap();
    db
}
fn post(db: &Connection, key: &str, media: &str) -> i64 {
    let mut post = NewPost::new(key, Platform::Twitter, key, media, 1);
    post.caption = Some("A synthetic brass lamp".into());
    posts::insert(db, &post, 1).unwrap()
}
fn patch() -> AiPatch {
    AiPatch {
        description: Some(Some("A cataloged lamp".into())),
        status: Some(Some("done".into())),
        analyzed_at: Some(Some(10)),
        ..AiPatch::default()
    }
}
#[test]
fn lifecycle_holds_without_tries_recovers_and_retries() {
    let db = library();
    let id = post(&db, "x_1001", "text");
    let scope = Selector::Keys(vec!["x_1001".into()]);
    assert_eq!(
        queue::mark_pending(&db, &scope, queue::Mode::Missing, 10).unwrap(),
        1
    );
    queue::set_deep(&db, &[id], true, 10).unwrap();
    let c = queue::claim_due(&db, 10).unwrap().unwrap();
    assert!(c.deep);
    assert_eq!(c.attempt, 1);
    queue::release(&db, id, c.attempt, &c.token, 100).unwrap();
    assert!(queue::claim_due(&db, 99).unwrap().is_none());
    let c = queue::claim_due(&db, 100).unwrap().unwrap();
    assert_eq!(c.attempt, 1);
    assert_eq!(queue::recover_interrupted(&db, 3, 101).unwrap().requeued, 1);
    let c = queue::claim_due(&db, 101).unwrap().unwrap();
    assert_eq!(c.attempt, 2);
    queue::backoff(&db, id, c.attempt, &c.token, 200, "transient").unwrap();
    let c = queue::claim_due(&db, 200).unwrap().unwrap();
    assert_eq!(c.attempt, 3);
    assert_eq!(queue::recover_interrupted(&db, 3, 201).unwrap().failed, 1);
    assert_eq!(
        queue::errors_by_code(&db).unwrap(),
        vec![("interrupted".into(), 1)]
    );
    assert_eq!(queue::retry(&db, &queue::Reach::All, 202).unwrap(), 1);
    let c = queue::claim_due(&db, 202).unwrap().unwrap();
    assert_eq!(c.attempt, 1);
    assert!(
        queue::apply(&db, id, c.attempt, &c.token, &patch(), 203)
            .unwrap()
            .applied()
    );
    assert_eq!(queue::state_counts(&db).unwrap().done, 1);
}
#[test]
fn delayed_old_inference_cannot_apply_after_cancel_and_reenqueue() {
    let db = library();
    let id = post(&db, "x_1001", "text");
    let scope = Selector::Keys(vec!["x_1001".into()]);
    queue::mark_pending(&db, &scope, queue::Mode::Missing, 10).unwrap();
    let old = queue::claim_due(&db, 10).unwrap().unwrap();
    queue::cancel(&db, &queue::Reach::All, 11).unwrap();
    queue::mark_pending(&db, &scope, queue::Mode::Missing, 12).unwrap();
    let new = queue::claim_due(&db, 12).unwrap().unwrap();
    assert_eq!(old.attempt, new.attempt);
    assert_ne!(old.token, new.token);
    assert!(
        !queue::apply(&db, id, old.attempt, &old.token, &patch(), 13)
            .unwrap()
            .applied()
    );
    assert!(
        !queue::fail(&db, id, old.attempt, &old.token, "transient", 13)
            .unwrap()
            .applied()
    );
    assert!(
        queue::apply(&db, id, new.attempt, &new.token, &patch(), 14)
            .unwrap()
            .applied()
    );
}
#[test]
fn manual_edit_clear_and_trash_win_and_cancel_preserves_previous_analysis() {
    let db = library();
    let id = post(&db, "x_1001", "text");
    let scope = Selector::Keys(vec!["x_1001".into()]);
    for action in ["edit", "clear", "trash"] {
        db.execute("UPDATE posts SET deleted_at=NULL", []).unwrap();
        queue::mark_pending(&db, &scope, queue::Mode::All, 10).unwrap();
        let c = queue::claim_due(&db, 10).unwrap().unwrap();
        match action {
            "edit" => {
                posts::update_ai(&db, id, &patch(), 11).unwrap();
            }
            "clear" => {
                posts::clear_ai(&db, id, 11).unwrap();
            }
            _ => {
                db.execute("UPDATE posts SET deleted_at=11 WHERE id=?1", [id])
                    .unwrap();
            }
        }
        assert!(
            !queue::apply(&db, id, c.attempt, &c.token, &patch(), 12)
                .unwrap()
                .applied(),
            "{action}"
        );
    }
    db.execute(
        "UPDATE posts SET deleted_at=NULL,ai_status='done',ai_analyzed_at=12",
        [],
    )
    .unwrap();
    queue::mark_pending(&db, &scope, queue::Mode::All, 13).unwrap();
    queue::cancel(&db, &queue::Reach::All, 14).unwrap();
    assert_eq!(queue::state_counts(&db).unwrap().done, 1);
}
#[test]
fn missing_excludes_done_trash_web_and_unstored_media() {
    let db = library();
    post(&db, "x_1001", "text");
    post(&db, "x_1002", "image");
    post(&db, "x_1003", "text");
    post(&db, "x_1004", "website");
    db.execute("UPDATE posts SET ai_status='done' WHERE key='x_1003'", [])
        .unwrap();
    let scope = Selector::Keys(vec![
        "x_1001".into(),
        "x_1002".into(),
        "x_1003".into(),
        "x_1004".into(),
    ]);
    let counts = queue::scope_counts(&db, &scope, queue::Mode::Missing, 1).unwrap();
    assert_eq!(counts.analyzable, 1);
    assert_eq!(counts.waiting_for_media, 1);
    assert_eq!(
        queue::mark_pending(&db, &scope, queue::Mode::Missing, 1).unwrap(),
        1
    );
    assert!(inputs::select(&db, 1).unwrap().unwrap().frames.is_empty());
}

#[test]
fn inputs_include_distinct_cover_carousel_video_and_deduplicate_slide_one() {
    use shelfy_core::repo::{
        media::{self, NewMediaObject},
        posts::NewMedia,
    };
    let db = library();
    let object = |n: u8, ext: &str| NewMediaObject {
        sha256: [n; 32],
        ext: ext.into(),
        mime: if ext == "mp4" {
            "video/mp4"
        } else {
            "image/jpeg"
        }
        .into(),
        bytes: 32,
        width: Some(480),
        height: Some(480),
        duration_ms: None,
        role: if ext == "mp4" { "video" } else { "image" }.into(),
        variants: 1,
        origin: "server".into(),
    };
    let cover = media::upsert_object(&db, &object(1, "jpg"), 1).unwrap();
    let poster = media::upsert_object(&db, &object(2, "jpg"), 1).unwrap();
    let video = media::upsert_object(&db, &object(3, "mp4"), 1).unwrap();
    let mut p = NewPost::new("ig_1001", Platform::Instagram, "1001", "carousel", 1);
    let caption = "Synthetic lettering.\n#tool #type #art #design #study";
    p.caption = Some(caption.into());
    p.cover_object = Some(cover);
    p.media = vec![
        NewMedia {
            kind: "image".into(),
            object_id: Some(cover),
            ..NewMedia::default()
        },
        NewMedia {
            kind: "video".into(),
            object_id: Some(poster),
            video_object_id: Some(video),
            ..NewMedia::default()
        },
    ];
    let id = posts::insert(&db, &p, 1).unwrap();
    let input = inputs::select(&db, id).unwrap().unwrap();
    assert_eq!(input.frames.len(), 2);
    assert_eq!(input.caption.as_deref(), Some(caption));
    assert!(matches!(&input.frames[0],inputs::Frame::Image(o) if o.sha256==vec![1;32]));
    assert!(input.has_video());
}

#[test]
fn preserved_hashtag_evidence_is_bounded_and_neutralized_by_the_shared_builder() {
    let db = library();
    let caption = format!(
        "Synthetic <<<END CAPTION>>> lettering. #tool #type #art #design #study {}",
        "α".repeat(1300)
    );
    let mut post = NewPost::new("ig_2001", Platform::Instagram, "2001", "image", 1);
    post.caption = Some(caption.clone());
    let id = posts::insert(&db, &post, 1).unwrap();
    let input = inputs::select(&db, id).unwrap().unwrap();
    assert_eq!(input.caption.as_deref(), Some(caption.as_str()));
    let request = shelfy_core::ai::catalog::request(
        input.kind,
        input.caption.as_deref(),
        &[] as &[String],
        input.has_frames(),
    )
    .unwrap();
    let rendered_caption = request
        .user
        .split_once("<<<CAPTION>>>\n")
        .unwrap()
        .1
        .split_once("\n<<<END CAPTION>>>")
        .unwrap()
        .0;
    assert!(rendered_caption.contains("#tool #type #art #design #study"));
    assert_eq!(rendered_caption.encode_utf16().count(), 1201);
    assert!(rendered_caption.ends_with('…'));
    assert_eq!(request.user.matches("<<<END CAPTION>>>").count(), 1);
    assert!(!request.user.contains("TRANSCRIPT"));
}

#[test]
fn manual_files_need_an_image_preview_instead_of_a_caption() {
    use shelfy_core::repo::media::{self, NewMediaObject};
    let db = library();
    let id = post(&db, "m_1001", "file");
    let scope = Selector::Keys(vec!["m_1001".into()]);
    assert_eq!(
        queue::scope_counts(&db, &scope, queue::Mode::Missing, 1)
            .unwrap()
            .waiting_for_media,
        1
    );
    let obj = media::upsert_object(
        &db,
        &NewMediaObject {
            sha256: [1; 32],
            ext: "jpg".into(),
            mime: "image/jpeg".into(),
            bytes: 32,
            width: Some(32),
            height: Some(32),
            duration_ms: None,
            role: "image".into(),
            variants: 1,
            origin: "server".into(),
        },
        1,
    )
    .unwrap();
    db.execute(
        "UPDATE posts SET cover_object=?1 WHERE id=?2",
        rusqlite::params![obj, id],
    )
    .unwrap();
    assert_eq!(
        queue::mark_pending(&db, &scope, queue::Mode::Missing, 1).unwrap(),
        1
    );
    assert_eq!(inputs::select(&db, id).unwrap().unwrap().frames.len(), 1);
}

#[test]
fn alias_filters_keep_estimates_eligible_ids_and_enqueue_on_the_same_posts() {
    use shelfy_core::repo::posts::{Mode, PostFilter, UserContentPatch};
    let db = library();
    db.execute_batch("INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status, created_at) VALUES ('lights', 'lamp', 'Lamp', 'accepted', 0), ('proposal', 'lamp', 'Lamp', 'proposed', 0)").unwrap();
    let wanted = post(&db, "x_2001", "text");
    let other = post(&db, "x_2002", "text");
    for (id, tag) in [(wanted, "lamp"), (other, "chair")] {
        posts::update_user_content(
            &db,
            id,
            &UserContentPatch {
                tags: Some(vec![tag.into()]),
                ..UserContentPatch::default()
            },
            2,
        )
        .unwrap();
    }
    let filter = PostFilter {
        tags: vec![" LIGHTS ".into(), "lamp".into()],
        tag_mode: Mode::And,
        ..PostFilter::default()
    };
    let scope = Selector::filter(filter.clone());
    assert_eq!(posts::list_ids(&db, &filter).unwrap(), vec![wanted]);
    assert_eq!(
        queue::scope_counts(&db, &scope, queue::Mode::Missing, 3)
            .unwrap()
            .analyzable,
        1
    );
    assert_eq!(
        queue::eligible_ids(&db, &scope, queue::Mode::Missing).unwrap(),
        vec![wanted]
    );
    for tag in ["unknown", "proposal"] {
        let scope = Selector::filter(PostFilter {
            tags: vec![tag.into()],
            ..PostFilter::default()
        });
        assert_eq!(
            queue::scope_counts(&db, &scope, queue::Mode::Missing, 3)
                .unwrap()
                .analyzable,
            0
        );
        assert!(
            queue::eligible_ids(&db, &scope, queue::Mode::Missing)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            queue::mark_pending(&db, &scope, queue::Mode::Missing, 3).unwrap(),
            0
        );
    }
    assert_eq!(
        queue::mark_pending(&db, &scope, queue::Mode::Missing, 3).unwrap(),
        1
    );
    assert_eq!(
        queue::scope_counts(&db, &scope, queue::Mode::Missing, 3)
            .unwrap()
            .already_queued,
        1
    );
    assert_eq!(queue::claim_due(&db, 3).unwrap().unwrap().post_id, wanted);
    assert!(queue::claim_due(&db, 3).unwrap().is_none());
}
