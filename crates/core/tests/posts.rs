//! The posts repository: keyset pagination, the desktop filter set, detail,
//! and the write primitives.

mod support;

use rusqlite::Connection;
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::media;
use shelfy_core::repo::posts::{
    self, AiLayer, Cursor, Mode, NewMedia, NewPost, PageRequest, PostFilter, PostSummary, Sort,
    SourceBucket, TagSource, UserContentPatch,
};
use shelfy_core::repo::{Platform, RepoError};
use support::{DAY, NOW, bare_post, fixture_library, insert_all, library, object, synthetic_posts};

fn all_pages(conn: &Connection, filter: &PostFilter, sort: Sort, limit: u32) -> Vec<String> {
    let mut keys = Vec::new();
    let mut cursor = None;
    loop {
        let page = posts::list(
            conn,
            filter,
            &PageRequest {
                sort,
                limit,
                cursor,
            },
        )
        .unwrap();
        assert!(page.items.len() <= limit as usize);
        keys.extend(page.items.into_iter().map(|p| p.key));
        match page.next_cursor {
            // Cursors survive their text form, as the API hands them out.
            Some(c) => cursor = Some(Cursor::parse(&c.to_string()).unwrap()),
            None => break,
        }
    }
    keys
}

fn expected_order(conn: &Connection, order: &str) -> Vec<String> {
    conn.prepare(&format!(
        "SELECT key FROM posts WHERE deleted_at IS NULL ORDER BY sort_ts {order}, id {order}"
    ))
    .unwrap()
    .query_map([], |r| r.get(0))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn keys(conn: &Connection, filter: &PostFilter) -> Vec<String> {
    all_pages(conn, filter, Sort::Newest, 200)
}

fn first_page(conn: &Connection, filter: &PostFilter, sort: Sort) -> Vec<PostSummary> {
    posts::list(
        conn,
        filter,
        &PageRequest {
            sort,
            ..PageRequest::default()
        },
    )
    .unwrap()
    .items
}

#[test]
fn live_language_and_missing_status_facets_select_the_same_population() {
    let conn = library();
    insert_all(
        &conn,
        &[
            bare_post("ig_901", Platform::Instagram, NOW),
            bare_post("ig_902", Platform::Instagram, NOW + 1),
            bare_post("ig_903", Platform::Instagram, NOW + 2),
        ],
    );
    conn.execute_batch(
        "UPDATE posts SET ai_language='it' WHERE key='ig_901';
        UPDATE posts SET ai_language='en',ai_status='done' WHERE key='ig_902';
        UPDATE posts SET ai_language='it',deleted_at=1 WHERE key='ig_903';",
    )
    .unwrap();
    let facets = shelfy_core::tags::facets::facets(&conn).unwrap();
    for row in &facets.language {
        let filter = PostFilter {
            ai_language: Some(row.value.clone()),
            ..PostFilter::default()
        };
        assert_eq!(posts::count(&conn, &filter).unwrap(), row.count);
    }
    let none = PostFilter {
        ai_status: Some("none".into()),
        ..PostFilter::default()
    };
    assert_eq!(keys(&conn, &none), ["ig_901"]);
    assert_eq!(posts::count(&conn, &none).unwrap(), 1);
    let combined = PostFilter {
        ai_language: Some("en".into()),
        ai_status: Some("none".into()),
        ..PostFilter::default()
    };
    assert!(keys(&conn, &combined).is_empty());
}

#[test]
fn keyset_pages_are_complete_and_stable() {
    let conn = library();
    let posts = synthetic_posts(500, 7);
    insert_all(&conn, &posts);
    let ties: i64 = conn
        .query_row(
            "SELECT count(*) FROM (SELECT sort_ts FROM posts GROUP BY sort_ts HAVING count(*) > 1)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(ties > 10, "the synthetic data has timestamp ties");

    let filter = PostFilter::default();
    for limit in [1, 37, 60, 200] {
        assert_eq!(
            all_pages(&conn, &filter, Sort::Newest, limit),
            expected_order(&conn, "DESC")
        );
    }
    assert_eq!(
        all_pages(&conn, &filter, Sort::Oldest, 41),
        expected_order(&conn, "ASC")
    );
    // Without search text, relevance falls back to newest.
    assert_eq!(
        all_pages(&conn, &filter, Sort::Relevance, 50),
        expected_order(&conn, "DESC")
    );
}

#[test]
fn pages_do_not_shift_when_posts_arrive() {
    let conn = library();
    insert_all(&conn, &synthetic_posts(120, 3));
    let first = posts::list(
        &conn,
        &PostFilter::default(),
        &PageRequest {
            limit: 50,
            ..PageRequest::default()
        },
    )
    .unwrap();
    // A newer post arrives between two page loads.
    posts::insert(
        &conn,
        &bare_post("ig_999", Platform::Instagram, NOW + DAY),
        NOW,
    )
    .unwrap();
    let second = posts::list(
        &conn,
        &PostFilter::default(),
        &PageRequest {
            limit: 50,
            cursor: first.next_cursor,
            ..PageRequest::default()
        },
    )
    .unwrap();
    let mut seen: Vec<String> = first
        .items
        .iter()
        .chain(&second.items)
        .map(|p| p.key.clone())
        .collect();
    let expected: Vec<String> = expected_order(&conn, "DESC")
        .into_iter()
        .skip(1)
        .take(100)
        .collect();
    assert_eq!(seen, expected);
    seen.dedup();
    assert_eq!(seen.len(), 100);
}

#[test]
fn cursors_of_another_sort_are_refused() {
    let conn = library();
    insert_all(&conn, &synthetic_posts(5, 1));
    let newest = Cursor::Newest {
        sort_ts: NOW,
        id: 3,
    };
    let req = |sort, cursor| PageRequest {
        sort,
        cursor: Some(cursor),
        ..PageRequest::default()
    };
    let f = PostFilter::default();
    assert!(matches!(
        posts::list(&conn, &f, &req(Sort::Oldest, newest)),
        Err(RepoError::InvalidCursor)
    ));
    let text = PostFilter {
        q: Some("lampada".into()),
        ..PostFilter::default()
    };
    assert!(matches!(
        posts::list(&conn, &text, &req(Sort::Relevance, newest)),
        Err(RepoError::InvalidCursor)
    ));
    assert!(matches!(
        posts::list(
            &conn,
            &f,
            &req(Sort::Newest, Cursor::Relevance { offset: 60 })
        ),
        Err(RepoError::InvalidCursor)
    ));
}

#[test]
fn list_uses_the_sort_index_without_a_temporary_sort() {
    let conn = library();
    insert_all(&conn, &synthetic_posts(50, 2));
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN SELECT p.id FROM posts p WHERE p.deleted_at IS NULL
               AND p.sort_ts <= 5 AND (p.sort_ts < 5 OR p.id < 9)
             ORDER BY p.sort_ts DESC, p.id DESC LIMIT 61",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let plan = plan.join(" | ");
    assert!(plan.contains("posts_sort"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

#[test]
fn source_filters() {
    let conn = library();
    fixture_library(&conn);
    let f = |filter: PostFilter| keys(&conn, &filter);

    assert_eq!(
        f(PostFilter {
            platform: Some(Platform::Instagram),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );
    assert_eq!(
        f(PostFilter {
            source: Some(SourceBucket::Web),
            ..PostFilter::default()
        }),
        ["web_00a1b2c3d4e5f6a7b8c9"]
    );
    // Manual bookmarks count as social, as on the desktop.
    assert_eq!(
        f(PostFilter {
            source: Some(SourceBucket::Social),
            ..PostFilter::default()
        }),
        [
            "m_01J9Z3B8K4QW6TFX0V7G2N5RCE",
            "x_1800000000000000001",
            "pin_900000000000000001",
            "ig_3101"
        ]
    );
    let lighting = collections::list(&conn).unwrap()[0].id;
    assert_eq!(
        f(PostFilter {
            collection_id: Some(lighting),
            ..PostFilter::default()
        }),
        ["pin_900000000000000001", "ig_3101"]
    );
    assert_eq!(
        f(PostFilter {
            media_types: vec!["text".into(), " website ".into(), "text".into()],
            ..PostFilter::default()
        }),
        ["x_1800000000000000001", "web_00a1b2c3d4e5f6a7b8c9"]
    );
    assert_eq!(
        f(PostFilter {
            date_from: Some(NOW - 6 * DAY),
            date_to: Some(NOW - 2 * DAY),
            ..PostFilter::default()
        }),
        ["x_1800000000000000001", "web_00a1b2c3d4e5f6a7b8c9"]
    );
}

#[test]
fn ai_and_tag_filters() {
    let conn = library();
    fixture_library(&conn);
    let f = |filter: PostFilter| keys(&conn, &filter);

    // A single tag is a hard filter, case-insensitive, AI or manual.
    assert_eq!(
        f(PostFilter {
            tag: Some(" GLASS ".into()),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );
    assert_eq!(
        f(PostFilter {
            tag: Some("work".into()),
            ..PostFilter::default()
        }),
        ["m_01J9Z3B8K4QW6TFX0V7G2N5RCE"]
    );
    // Tags match the alias-resolved norm.
    assert_eq!(
        f(PostFilter {
            tag: Some("headphones".into()),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );

    let tags = |list: &[&str], mode| PostFilter {
        tags: list.iter().map(|s| (*s).to_owned()).collect(),
        tag_mode: mode,
        ..PostFilter::default()
    };
    assert_eq!(
        f(tags(&["glass", "work"], Mode::Or)),
        ["m_01J9Z3B8K4QW6TFX0V7G2N5RCE", "ig_3101"]
    );
    assert!(f(tags(&["glass", "work"], Mode::And)).is_empty());
    assert_eq!(f(tags(&["glass", "lampada"], Mode::And)), ["ig_3101"]);

    assert_eq!(
        f(PostFilter {
            entity: Some("MURANO".into()),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );
    assert_eq!(
        f(PostFilter {
            category: Some("interior".into()),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );
    assert_eq!(
        f(PostFilter {
            content_type: Some("product".into()),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );
    assert_eq!(
        f(PostFilter {
            ai_status: Some("done".into()),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );
    assert_eq!(
        f(PostFilter {
            analyzed: Some(true),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );
    assert_eq!(
        f(PostFilter {
            analyzed: Some(false),
            ..PostFilter::default()
        })
        .len(),
        4
    );

    // Manual tags do not make a post "AI-tagged".
    assert_eq!(
        f(PostFilter {
            ai_tagged: Some(true),
            ..PostFilter::default()
        }),
        ["ig_3101"]
    );
    let untagged = f(PostFilter {
        ai_tagged: Some(false),
        ..PostFilter::default()
    });
    assert!(untagged.contains(&"m_01J9Z3B8K4QW6TFX0V7G2N5RCE".to_owned()));
    assert_eq!(untagged.len(), 4);
}

#[test]
fn stored_filter_counts_any_archived_object() {
    let conn = library();
    fixture_library(&conn);
    let stored = keys(
        &conn,
        &PostFilter {
            stored: Some(true),
            ..PostFilter::default()
        },
    );
    // The carousel has a cover and slides; the website has its page screenshot.
    assert_eq!(stored, ["web_00a1b2c3d4e5f6a7b8c9", "ig_3101"]);
    let link_only = keys(
        &conn,
        &PostFilter {
            stored: Some(false),
            ..PostFilter::default()
        },
    );
    assert_eq!(link_only.len(), 3);
}

#[test]
fn trash_is_hidden_and_listable() {
    let conn = library();
    fixture_library(&conn);
    assert!(!keys(&conn, &PostFilter::default()).contains(&"ig_3102".to_owned()));
    assert_eq!(
        keys(
            &conn,
            &PostFilter {
                trash: true,
                ..PostFilter::default()
            }
        ),
        ["ig_3102"]
    );
    let id = posts::id_for_key(&conn, "ig_3102").unwrap().unwrap();
    assert_eq!(posts::restore(&conn, &[id], NOW).unwrap(), 1);
    assert!(keys(&conn, &PostFilter::default()).contains(&"ig_3102".to_owned()));
    assert_eq!(posts::restore(&conn, &[id], NOW).unwrap(), 0);
}

#[test]
fn count_and_ids_agree_with_the_list() {
    let conn = library();
    insert_all(&conn, &synthetic_posts(300, 11));
    let filters = [
        PostFilter::default(),
        PostFilter {
            platform: Some(Platform::Twitter),
            ..PostFilter::default()
        },
        PostFilter {
            media_types: vec!["carousel".into()],
            ..PostFilter::default()
        },
        PostFilter {
            q: Some("lampada design".into()),
            ..PostFilter::default()
        },
        PostFilter {
            stored: Some(false),
            date_from: Some(NOW - 365 * DAY),
            ..PostFilter::default()
        },
    ];
    for filter in &filters {
        let listed = keys(&conn, filter);
        assert_eq!(
            posts::count(&conn, filter).unwrap(),
            listed.len() as u64,
            "{filter:?}"
        );
        assert_eq!(
            posts::list_ids(&conn, filter).unwrap().len(),
            listed.len(),
            "{filter:?}"
        );
    }
}

#[test]
fn detail_returns_every_layer() {
    let conn = library();
    fixture_library(&conn);
    let detail = posts::get(&conn, "ig_3101").unwrap().unwrap();
    let s = &detail.summary;
    assert_eq!(s.platform, Platform::Instagram);
    assert_eq!(s.shortcode.as_deref(), Some("C0ffeeAbCdE"));
    assert_eq!(s.media_count, 2);
    assert_eq!(s.cover.as_ref().unwrap().ext, "jpg");
    assert!(s.cover.as_ref().unwrap().has_g480);
    assert_eq!(s.cover.as_ref().unwrap().sha256.len(), 64);
    assert_eq!(s.media.len(), 2);
    assert_eq!(s.media[1].kind, "video");
    assert_eq!(s.media[1].video_object.as_ref().unwrap().ext, "mp4");
    assert_eq!(s.collection_ids.len(), 1);
    assert_eq!(s.ai_tags, ["lampada", "Glass", "cuffie"]);
    assert_eq!(s.user_tags, ["Lighting", "cuffie"]);
    assert_eq!(s.user_note.as_deref(), Some("per il soggiorno"));
    assert_eq!(detail.ai_keywords, ["blown glass", "desk lamp"]);
    assert_eq!(detail.ai_entities, ["Murano"]);
    assert_eq!(detail.entities[0].norm, "murano");
    // An AI and a manual tag of the same name are two rows (plan §1.2 #3), and
    // the alias "cuffie" resolved to "headphones" in both layers.
    let rows: Vec<(String, TagSource, Option<String>)> = detail
        .tags
        .iter()
        .map(|t| (t.norm.clone(), t.source, t.tier.clone()))
        .collect();
    assert_eq!(
        rows,
        [
            ("glass".into(), TagSource::Ai, Some("specific".into())),
            ("headphones".into(), TagSource::Ai, None),
            ("lampada".into(), TagSource::Ai, Some("general".into())),
            ("headphones".into(), TagSource::Manual, None),
            ("lighting".into(), TagSource::Manual, None),
        ]
    );

    let site = posts::get(&conn, "web_00a1b2c3d4e5f6a7b8c9")
        .unwrap()
        .unwrap();
    let capture = site.summary.web_capture.as_ref().unwrap();
    assert_eq!(capture.title.as_deref(), Some("Studio Example"));
    assert_eq!(capture.favicon.as_ref().unwrap().ext, "png");
    assert!(capture.palette.as_ref().unwrap().is_array());

    assert!(posts::get(&conn, "ig_missing").unwrap().is_none());
    // Serialized for the API: camelCase, no internal id.
    let json = serde_json::to_value(&detail).unwrap();
    assert!(json.get("id").is_none());
    assert_eq!(json["key"], "ig_3101");
    assert_eq!(json["mediaCount"], 2);
    assert_eq!(json["tags"][0]["source"], "ai");
}

#[test]
fn get_many_keeps_the_input_order() {
    let conn = library();
    fixture_library(&conn);
    let wanted = [
        "pin_900000000000000001",
        "missing",
        "ig_3101",
        "x_1800000000000000001",
    ]
    .map(String::from);
    let found: Vec<String> = posts::get_many(&conn, &wanted)
        .unwrap()
        .into_iter()
        .map(|p| p.key)
        .collect();
    assert_eq!(
        found,
        ["pin_900000000000000001", "ig_3101", "x_1800000000000000001"]
    );
}

#[test]
fn relevance_ranks_tag_matches_and_phrases_first() {
    let conn = library();
    let mut a = bare_post("ig_1", Platform::Instagram, NOW - DAY);
    a.caption = Some("A lamp next to a chair, lampada mentioned once".into());
    let mut b = bare_post("ig_2", Platform::Instagram, NOW - 2 * DAY);
    b.caption = Some("Desk setup".into());
    b.ai = Some(AiLayer {
        tags: vec!["lampada".into()],
        description: Some("Lampada in vetro".into()),
        ..AiLayer::default()
    });
    let mut c = bare_post("ig_3", Platform::Instagram, NOW);
    c.caption = Some("Nothing relevant here".into());
    let mut d = bare_post("ig_4", Platform::Instagram, NOW - 3 * DAY);
    d.caption = Some("lampada vetro soffiato, the exact phrase".into());
    insert_all(&conn, &[a, b, c, d]);

    let filter = PostFilter {
        q: Some("lampada vetro".into()),
        ..PostFilter::default()
    };
    let ranked: Vec<String> = first_page(&conn, &filter, Sort::Relevance)
        .into_iter()
        .map(|p| p.key)
        .collect();
    // Exact tag + description first, then both terms as a phrase in the caption,
    // then a single caption mention; the unrelated post is not a hit.
    assert_eq!(ranked, ["ig_2", "ig_4", "ig_1"]);
    // The same hits by date.
    let by_date: Vec<String> = first_page(&conn, &filter, Sort::Newest)
        .into_iter()
        .map(|p| p.key)
        .collect();
    assert_eq!(by_date, ["ig_1", "ig_2", "ig_4"]);
}

#[test]
fn relevance_pages_by_offset() {
    let conn = library();
    let many: Vec<NewPost> = (0..130)
        .map(|i| {
            let mut p = bare_post(&format!("x_{i}"), Platform::Twitter, NOW - i * 1000);
            p.caption = Some(format!("poster number {i}"));
            p
        })
        .collect();
    insert_all(&conn, &many);
    let filter = PostFilter {
        q: Some("poster".into()),
        ..PostFilter::default()
    };
    let all = all_pages(&conn, &filter, Sort::Relevance, 60);
    assert_eq!(all.len(), 130);
    let mut unique = all.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 130);
    let page = posts::list(
        &conn,
        &filter,
        &PageRequest {
            sort: Sort::Relevance,
            limit: 60,
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(page.next_cursor, Some(Cursor::Relevance { offset: 60 }));
}

#[test]
fn concepts_and_hybrid_tags_follow_the_desktop_blocks() {
    let conn = library();
    let mut a = bare_post("ig_1", Platform::Instagram, NOW - DAY);
    a.caption = Some("street photography in Milano".into());
    let mut b = bare_post("ig_2", Platform::Instagram, NOW - 2 * DAY);
    b.caption = Some("portrait session".into());
    b.user_tags = vec!["film".into()];
    let mut c = bare_post("ig_3", Platform::Instagram, NOW - 3 * DAY);
    c.caption = Some("street food".into());
    insert_all(&conn, &[a, b, c]);
    let f = |filter: PostFilter| keys(&conn, &filter);

    // Concepts broaden the text in OR mode and narrow it in AND mode.
    let base = PostFilter {
        q: Some("street".into()),
        concepts: vec!["Street photography".into()],
        ..PostFilter::default()
    };
    assert_eq!(f(base.clone()), ["ig_1", "ig_3"]);
    assert_eq!(
        f(PostFilter {
            concept_mode: Mode::And,
            ..base
        }),
        ["ig_1"]
    );
    // A concept alone is a search.
    assert_eq!(
        f(PostFilter {
            concepts: vec!["portrait".into()],
            ..PostFilter::default()
        }),
        ["ig_2"]
    );

    // Hybrid: with text, OR tags join the text instead of filtering...
    let hybrid = PostFilter {
        q: Some("milano".into()),
        tags: vec!["film".into()],
        ..PostFilter::default()
    };
    assert_eq!(f(hybrid.clone()), ["ig_1", "ig_2"]);
    // ...while AND tags stay hard filters.
    assert!(
        f(PostFilter {
            tag_mode: Mode::And,
            ..hybrid
        })
        .is_empty()
    );
    // The gallery's single tag chip always filters.
    assert!(
        f(PostFilter {
            q: Some("milano".into()),
            tag: Some("film".into()),
            ..PostFilter::default()
        })
        .is_empty()
    );
}

#[test]
fn search_text_is_literal() {
    let conn = library();
    let mut a = bare_post("ig_1", Platform::Instagram, NOW);
    a.caption = Some("near and far".into());
    insert_all(&conn, &[a]);
    for q in [
        "\"NEAR\" OR AND (",
        "col:umn* ^x",
        "!!!",
        "{tags}: x",
        "\"\"\"",
    ] {
        let filter = PostFilter {
            q: Some(q.into()),
            ..PostFilter::default()
        };
        posts::list(
            &conn,
            &filter,
            &PageRequest {
                sort: Sort::Relevance,
                ..PageRequest::default()
            },
        )
        .unwrap();
        posts::count(&conn, &filter).unwrap();
    }
    assert_eq!(
        keys(
            &conn,
            &PostFilter {
                q: Some("NEAR".into()),
                ..PostFilter::default()
            }
        ),
        ["ig_1"]
    );
    // Blank text is no search at all.
    assert_eq!(
        keys(
            &conn,
            &PostFilter {
                q: Some("   ".into()),
                ..PostFilter::default()
            }
        ),
        ["ig_1"]
    );
}

#[test]
fn insert_validates_and_detects_duplicates() {
    let conn = library();
    posts::insert(&conn, &bare_post("ig_1", Platform::Instagram, NOW), NOW).unwrap();
    let dup_key = bare_post("ig_1", Platform::Instagram, NOW);
    assert!(matches!(
        posts::insert(&conn, &dup_key, NOW),
        Err(RepoError::Conflict("post"))
    ));
    let mut dup_native = bare_post("ig_x", Platform::Instagram, NOW);
    dup_native.native_id = "1".into();
    assert!(matches!(
        posts::insert(&conn, &dup_native, NOW),
        Err(RepoError::Conflict("post"))
    ));

    let mut bad = bare_post("ig_2", Platform::Instagram, NOW);
    bad.caption = Some("x".repeat(posts::CAPTION_MAX_CHARS + 1));
    assert!(matches!(
        posts::insert(&conn, &bad, NOW),
        Err(RepoError::Invalid {
            field: "caption",
            ..
        })
    ));
    let mut bad = bare_post("ig_3", Platform::Instagram, NOW);
    bad.media = vec![NewMedia {
        kind: "gif".into(),
        ..NewMedia::default()
    }];
    assert!(matches!(
        posts::insert(&conn, &bad, NOW),
        Err(RepoError::Invalid {
            field: "media.kind",
            ..
        })
    ));
    let bad = NewPost::new(" ", Platform::Web, "x", "website", NOW);
    assert!(matches!(
        posts::insert(&conn, &bad, NOW),
        Err(RepoError::Invalid { field: "key", .. })
    ));

    // Undated posts sort by their import time.
    let mut undated = bare_post("ig_4", Platform::Instagram, NOW);
    undated.posted_at = None;
    undated.imported_at = NOW - 7 * DAY;
    let id = posts::insert(&conn, &undated, NOW).unwrap();
    let sort_ts: i64 = conn
        .query_row("SELECT sort_ts FROM posts WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(sort_ts, NOW - 7 * DAY);
}

#[test]
fn manual_tags_never_touch_ai_tags() {
    let conn = library();
    let mut p = bare_post("ig_1", Platform::Instagram, NOW);
    p.ai = Some(AiLayer {
        tags: vec!["Design".into()],
        ..AiLayer::default()
    });
    let id = posts::insert(&conn, &p, NOW).unwrap();
    let patch = UserContentPatch {
        tags: Some(vec![" design ".into(), "Design".into(), "".into()]),
        note: None,
    };
    posts::update_user_content(&conn, id, &patch, NOW).unwrap();
    let detail = posts::get(&conn, "ig_1").unwrap().unwrap();
    // The display list is stored as given (desktop `updateUserContent`); the
    // tag rows are normalized and deduped.
    assert_eq!(detail.summary.user_tags, [" design ", "Design", ""]);
    assert_eq!(
        detail.tags.len(),
        2,
        "one AI and one manual row for the same name"
    );

    // Clearing the manual tags keeps the AI tag (the desktop lost it until re-analysis).
    posts::update_user_content(
        &conn,
        id,
        &UserContentPatch {
            tags: Some(vec![]),
            note: None,
        },
        NOW,
    )
    .unwrap();
    let detail = posts::get(&conn, "ig_1").unwrap().unwrap();
    assert!(detail.summary.user_tags.is_empty());
    assert_eq!(detail.tags.len(), 1);
    assert_eq!(detail.tags[0].source, TagSource::Ai);
    let tagged = keys(
        &conn,
        &PostFilter {
            tag: Some("design".into()),
            ..PostFilter::default()
        },
    );
    assert_eq!(tagged, ["ig_1"]);

    // And clearing the AI layer keeps the manual tags.
    posts::update_user_content(
        &conn,
        id,
        &UserContentPatch {
            tags: Some(vec!["mine".into()]),
            note: Some(Some("for the studio".into())),
        },
        NOW,
    )
    .unwrap();
    posts::clear_ai(&conn, id, NOW).unwrap();
    let detail = posts::get(&conn, "ig_1").unwrap().unwrap();
    assert_eq!(detail.tags.len(), 1);
    assert_eq!(detail.tags[0].source, TagSource::Manual);
    assert_eq!(detail.summary.user_note.as_deref(), Some("for the studio"));
    assert!(detail.summary.ai_tags.is_empty());

    // `Some(None)` clears the note; `None` leaves fields alone.
    let clear = UserContentPatch {
        note: Some(None),
        tags: None,
    };
    posts::update_user_content(&conn, id, &clear, NOW).unwrap();
    let detail = posts::get(&conn, "ig_1").unwrap().unwrap();
    assert!(detail.summary.user_note.is_none());
    assert_eq!(detail.summary.user_tags, ["mine"]);

    assert!(matches!(
        posts::update_user_content(&conn, 999, &UserContentPatch::default(), NOW),
        Err(RepoError::NotFound)
    ));
}

#[test]
fn purge_cascades_and_releases_objects() {
    let conn = library();
    fixture_library(&conn);
    let shared = media::upsert_object(&conn, &object(1, "image", "jpg"), NOW).unwrap();
    // Another post keeps the shared cover alive.
    let mut other = bare_post("ig_9", Platform::Instagram, NOW);
    other.cover_object = Some(shared);
    posts::insert(&conn, &other, NOW).unwrap();

    let id = posts::id_for_key(&conn, "ig_3101").unwrap().unwrap();
    assert_eq!(posts::purge(&conn, &[id], NOW + 1).unwrap(), 1);
    assert!(posts::get(&conn, "ig_3101").unwrap().is_none());
    for table in [
        "post_media",
        "post_tags",
        "post_entities",
        "post_collections",
    ] {
        let n: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE post_id = ?1"),
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let unreferenced: Vec<(i64, Option<i64>)> = conn
        .prepare(
            "SELECT id, unreferenced_since FROM media_objects WHERE id IN (1, 2, 3, 4) ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        unreferenced,
        [
            (1, None),
            (2, Some(NOW + 1)),
            (3, Some(NOW + 1)),
            (4, Some(NOW + 1))
        ]
    );

    // Purging a website removes its captures and their assets too.
    let site = posts::id_for_key(&conn, "web_00a1b2c3d4e5f6a7b8c9")
        .unwrap()
        .unwrap();
    posts::purge(&conn, &[site], NOW + 2).unwrap();
    let assets: i64 = conn
        .query_row("SELECT count(*) FROM web_capture_assets", [], |r| r.get(0))
        .unwrap();
    assert_eq!(assets, 0);
    let band: Option<i64> = conn
        .query_row(
            "SELECT unreferenced_since FROM media_objects WHERE id = 7",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(band, Some(NOW + 2));
    // Re-recording an object revives it.
    media::upsert_object(&conn, &object(7, "band", "webp"), NOW + 3).unwrap();
    let band: Option<i64> = conn
        .query_row(
            "SELECT unreferenced_since FROM media_objects WHERE id = 7",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(band, None);
}

#[test]
fn collection_membership_shows_on_posts() {
    let conn = library();
    let ids = insert_all(&conn, &synthetic_posts(3, 5));
    let c = collections::create(
        &conn,
        &NewCollection {
            name: "A".into(),
            ..NewCollection::default()
        },
        NOW,
    )
    .unwrap();
    collections::add_posts(&conn, &ids[..2], &[c.id], NOW).unwrap();
    let page = posts::list(&conn, &PostFilter::default(), &PageRequest::default()).unwrap();
    let members = page
        .items
        .iter()
        .filter(|p| p.collection_ids == [c.id])
        .count();
    assert_eq!(members, 2);
}
