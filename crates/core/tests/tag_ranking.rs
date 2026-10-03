//! Tag-only ranking: tier/source-independent scores, exact filters and capped paging.
mod support;
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{
    self, AiLayer, Cursor, Mode, PageRequest, PostFilter, Sort, SourceBucket,
};
use support::{NOW, bare_post, insert_all, library};

fn keys(conn: &rusqlite::Connection, filter: &PostFilter) -> Vec<String> {
    posts::keys_of(conn, &posts::rank(conn, filter).unwrap()).unwrap()
}

#[test]
fn idf_counts_distinct_posts_and_ignores_tiers_and_filtered_population() {
    let conn = library();
    let mut fixtures = Vec::new();
    for n in 0..30 {
        let mut p = bare_post(&format!("ig_{n}"), Platform::Instagram, NOW - n);
        let tags = match n {
            0 => vec!["rare", "common"],
            1 => vec!["rare"],
            _ => vec!["common"],
        };
        p.ai = Some(AiLayer {
            tags: tags.iter().map(|t| (*t).into()).collect(),
            general_tags: Some(vec!["common".into()]),
            specific_tags: Some(vec!["rare".into()]),
            ..AiLayer::default()
        });
        // Two physical rows for one matching tag must never double its weight.
        if n == 2 {
            p.user_tags = vec!["common".into()];
        }
        fixtures.push(p);
    }
    let mut web = bare_post("web_1", Platform::Web, NOW + 100);
    web.user_tags = vec!["common".into()];
    fixtures.push(web);
    let mut untagged = bare_post("ig_999", Platform::Instagram, NOW + 200);
    untagged.caption = Some("rare common".into());
    untagged.user_note = Some("rare".into());
    untagged.ai = Some(AiLayer {
        description: Some("rare".into()),
        keywords: vec!["rare".into()],
        ..AiLayer::default()
    });
    fixtures.push(untagged);
    insert_all(&conn, &fixtures);
    // Make the rare-only post older than every common-only post.
    conn.execute(
        "UPDATE posts SET sort_ts = ?1 WHERE key = 'ig_1'",
        [NOW - 1000],
    )
    .unwrap();
    let filter = PostFilter {
        tags: vec!["rare".into(), "common".into()],
        ..PostFilter::default()
    };
    let order = keys(&conn, &filter);
    assert_eq!(&order[..4], ["ig_0", "ig_1", "web_1", "ig_2"]);
    assert_eq!(posts::count(&conn, &filter).unwrap(), 31);
    assert_eq!(
        keys(
            &conn,
            &PostFilter {
                tag_mode: Mode::And,
                ..filter.clone()
            }
        ),
        ["ig_0"]
    );
    assert_eq!(
        keys(
            &conn,
            &PostFilter {
                source: Some(SourceBucket::Web),
                ..filter.clone()
            }
        ),
        ["web_1"]
    );
    assert_eq!(
        &keys(
            &conn,
            &PostFilter {
                source: Some(SourceBucket::Social),
                ..filter.clone()
            }
        )[..3],
        ["ig_0", "ig_1", "ig_2"]
    );
    // Unknown, prefix or description-only tags never match exact tag filters.
    assert!(
        keys(
            &conn,
            &PostFilter {
                tags: vec!["rar".into(), "absent".into()],
                ..PostFilter::default()
            }
        )
        .is_empty()
    );
    assert_eq!(
        keys(
            &conn,
            &PostFilter {
                date_from: Some(NOW - 4),
                ..filter.clone()
            }
        ),
        ["ig_0", "web_1", "ig_2", "ig_3", "ig_4"]
    );
    conn.execute("UPDATE posts SET deleted_at = 1 WHERE key = 'ig_0'", [])
        .unwrap();
    assert_eq!(posts::count(&conn, &filter).unwrap(), 30);
    assert_eq!(keys(&conn, &filter)[0], "ig_1");
}

#[test]
fn identical_score_and_time_use_id_desc_and_paging_stops_at_the_window() {
    let conn = library();
    let fixtures: Vec<_> = (0..1005)
        .map(|n| {
            let mut p = bare_post(&format!("ig_{n}"), Platform::Instagram, NOW);
            p.user_tags = vec!["tag".into()];
            p
        })
        .collect();
    insert_all(&conn, &fixtures);
    let filter = PostFilter {
        tags: vec!["tag".into()],
        ..PostFilter::default()
    };
    assert_eq!(posts::count(&conn, &filter).unwrap(), 1005);
    let ranked = keys(&conn, &filter);
    assert_eq!(ranked.len(), 1000);
    assert_eq!(&ranked[..2], ["ig_1004", "ig_1003"]);
    let mut all = Vec::new();
    let mut cursor = None;
    loop {
        let page = posts::list(
            &conn,
            &filter,
            &PageRequest {
                sort: Sort::Relevance,
                limit: 137,
                cursor,
            },
        )
        .unwrap();
        all.extend(page.items.into_iter().map(|p| p.key));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(all, ranked);
    assert!(
        posts::list(
            &conn,
            &filter,
            &PageRequest {
                sort: Sort::Relevance,
                limit: 10,
                cursor: Some(Cursor::Relevance { offset: 1000 })
            }
        )
        .unwrap()
        .items
        .is_empty()
    );
}

#[test]
fn alias_filters_select_the_same_posts_for_lists_counts_and_bulk_delete() {
    use shelfy_core::bulk::{self, Action};
    use shelfy_core::selector::{self, Selector};
    let conn = library();
    conn.execute_batch("INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status, created_at) VALUES ('lights', 'lamps', 'Lamps', 'accepted', 0), ('lamps', 'lamp', 'Lamp', 'accepted', 0), ('proposal', 'lamp', 'Lamp', 'proposed', 0)").unwrap();
    let mut a = bare_post("ig_1", Platform::Instagram, NOW);
    a.user_tags = vec!["lamp".into(), "glass".into()];
    let mut b = bare_post("ig_2", Platform::Instagram, NOW - 1);
    b.user_tags = vec!["lamp".into()];
    let mut c = bare_post("ig_3", Platform::Instagram, NOW - 2);
    c.user_tags = vec!["glass".into()];
    insert_all(&conn, &[a, b, c]);
    let filter = PostFilter {
        tags: vec![
            "LIGHTS".into(),
            "lamps".into(),
            "lamp".into(),
            "glass".into(),
        ],
        tag_mode: Mode::And,
        ..PostFilter::default()
    };
    let selector = Selector::filter(filter.clone());
    let ids = posts::list_ids(&conn, &filter).unwrap();
    assert_eq!(posts::keys_of(&conn, &ids).unwrap(), ["ig_1"]);
    assert_eq!(selector::ids(&conn, &selector).unwrap(), ids);
    assert_eq!(
        selector::count(&conn, &selector).unwrap(),
        posts::count(&conn, &filter).unwrap()
    );
    assert_eq!(bulk::count(&conn, &selector).unwrap(), 1);
    for unknown in ["missing", "proposal"] {
        let filter = PostFilter {
            tags: vec![unknown.into()],
            ..PostFilter::default()
        };
        let selector = Selector::filter(filter.clone());
        assert!(posts::list_ids(&conn, &filter).unwrap().is_empty());
        assert_eq!(selector::count(&conn, &selector).unwrap(), 0);
        assert_eq!(
            bulk::apply(&conn, &selector, &Action::Delete, NOW)
                .unwrap()
                .selected,
            0
        );
    }
    let result = bulk::apply(&conn, &selector, &Action::Delete, NOW).unwrap();
    assert_eq!(result.selected, 1);
    assert_eq!(result.changed.len(), 1);
    assert!(posts::list_ids(&conn, &filter).unwrap().is_empty());
    assert_eq!(posts::count(&conn, &PostFilter::default()).unwrap(), 2);
}

#[test]
fn text_field_weights_and_whole_token_priority_stay_unchanged() {
    let conn = library();
    let mut fixtures = Vec::new();
    for n in 0..6 {
        let mut p = bare_post(&format!("ig_{n}"), Platform::Instagram, NOW);
        p.ai = Some(AiLayer::default());
        match n {
            0 => p.ai.as_mut().unwrap().tags = vec!["lantern".into()],
            1 => p.ai.as_mut().unwrap().keywords = vec!["lantern".into()],
            2 => p.ai.as_mut().unwrap().description = Some("lantern".into()),
            3 => p.user_note = Some("lantern".into()),
            4 => p.caption = Some("lantern".into()),
            _ => p.caption = Some("lanterns".into()),
        }
        fixtures.push(p);
    }
    insert_all(&conn, &fixtures);
    let filter = PostFilter {
        q: Some("lantern".into()),
        ..PostFilter::default()
    };
    // Description and note share weight 4; ties still use the descending id.
    assert_eq!(
        keys(&conn, &filter),
        ["ig_0", "ig_1", "ig_3", "ig_2", "ig_4", "ig_5"]
    );
}
