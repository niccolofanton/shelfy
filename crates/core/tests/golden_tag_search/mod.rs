//! P3-05: byte-for-byte desktop keys and total on the tag-only fixture libraries.
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::json;
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, AiLayer, Mode, NewPost, PostFilter, SourceBucket};
use shelfy_core::schema::{self, Kind};

type Post = (String, String, i64, Vec<String>, Vec<String>);
type Alias = (String, String, String, String);
#[derive(Deserialize)]
struct Options {
    mode: Option<String>,
    source: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[test]
fn tag_search_matches_desktop() {
    super::check(
        "tag-search",
        |(posts, aliases, tags, options): (Vec<Post>, Vec<Alias>, Vec<String>, Options)| {
            let mut conn = Connection::open_in_memory().unwrap();
            schema::migrate(&mut conn, Kind::Library).unwrap();
            for (alias, norm, form, status) in aliases {
                conn.execute("INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status, created_at) VALUES (?1, ?2, ?3, ?4, 0)", params![alias, norm, form, status]).unwrap();
            }
            for (key, platform, time, tags, manual) in posts {
                let platform = match platform.as_str() {
                    "web" => Platform::Web,
                    "manual" => Platform::Manual,
                    _ => Platform::Instagram,
                };
                let mut post = NewPost::new(&key, platform, &key, "image", time);
                post.posted_at = Some(time);
                post.ai = Some(AiLayer {
                    tags,
                    general_tags: Some(vec!["design".into()]),
                    specific_tags: Some(vec!["lamp".into(), "glass".into()]),
                    ..AiLayer::default()
                });
                post.user_tags = manual;
                posts::insert(&conn, &post, time).unwrap();
            }
            let filter = PostFilter {
                tags,
                tag_mode: if options.mode.as_deref() == Some("and") {
                    Mode::And
                } else {
                    Mode::Or
                },
                source: match options.source.as_deref() {
                    Some("web") => Some(SourceBucket::Web),
                    Some("social") => Some(SourceBucket::Social),
                    _ => None,
                },
                ..PostFilter::default()
            };
            let ids = posts::rank(&conn, &filter).unwrap();
            let offset = options.offset.unwrap_or(0);
            let ids: Vec<_> = ids
                .into_iter()
                .skip(offset)
                .take(options.limit.unwrap_or(60))
                .collect();
            let keys = posts::keys_of(&conn, &ids).unwrap();
            let total = if filter.has_relevance() {
                posts::count(&conn, &filter).unwrap()
            } else {
                0
            };
            json!({ "keys": keys, "total": total })
        },
    );
}
