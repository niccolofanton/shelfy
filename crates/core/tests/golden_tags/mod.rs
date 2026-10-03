//! Byte-for-byte explorer/maintenance fixtures on synthetic desktop data.
use super::check;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use shelfy_core::schema::{self, Kind};
use shelfy_core::tags::{explore, health, merge};
#[derive(Deserialize)]
struct Post {
    tags: Vec<String>,
    manual: Vec<String>,
    general: Vec<String>,
    specific: Vec<String>,
    entities: Vec<String>,
    category: Option<String>,
    #[serde(rename = "type")]
    content_type: Option<String>,
    language: Option<String>,
    status: Option<String>,
    timestamp: i64,
}
fn seed(posts: &[Post]) -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    c.pragma_update(None, "foreign_keys", "ON").unwrap();
    schema::migrate(&mut c, Kind::Library).unwrap();
    for (i, p) in posts.iter().enumerate() {
        let id = (i + 1) as i64;
        c.execute("INSERT INTO posts(id,key,platform,native_id,media_type,imported_at,sort_ts,updated_at,posted_at,ai_tags_json,user_tags_json,ai_category,ai_content_type,ai_language,ai_status) VALUES(?1,?2,'instagram',?2,'image',1,1,1,?3,?4,?5,?6,?7,?8,?9)",params![id,id.to_string(),p.timestamp,serde_json::to_string(&p.tags).unwrap(),serde_json::to_string(&p.manual).unwrap(),p.category,p.content_type,p.language,p.status]).unwrap();
        for t in &p.tags {
            let tier = if p.specific.contains(t) {
                Some("specific")
            } else if p.general.contains(t) {
                Some("general")
            } else {
                None
            };
            c.execute("INSERT INTO post_tags(post_id,tag_norm,tag_form,source,tier) VALUES(?1,?2,?3,'ai',?4)",params![id,t.to_lowercase(),t,tier]).unwrap();
        }
        for t in &p.manual {
            c.execute(
                "INSERT INTO post_tags(post_id,tag_norm,tag_form,source) VALUES(?1,?2,?3,'manual')",
                params![id, t.to_lowercase(), t],
            )
            .unwrap();
        }
        for t in &p.entities {
            c.execute(
                "INSERT INTO post_entities(post_id,ent_norm,ent_form) VALUES(?1,?2,?3)",
                params![id, t.to_lowercase(), t],
            )
            .unwrap();
        }
    }
    c.execute("INSERT INTO tag_cluster(id,label,status,run_id,created_at,updated_at) VALUES(1,'Furniture','accepted',1,1,1)",[]).unwrap();
    c.execute(
        "INSERT INTO tag_cluster_membership(cluster_id,tag_norm) VALUES(1,'chair')",
        [],
    )
    .unwrap();
    c
}
#[test]
fn reads_match_desktop_bytes() {
    check("ai/tags/overview", |(p,): (Vec<Post>,)| {
        explore::overview(&seed(&p)).unwrap()
    });
    check("ai/tags/stats", |(p, tier): (Vec<Post>, explore::Tier)| {
        explore::stats(&seed(&p), tier, 200).unwrap()
    });
    check("ai/tags/entities", |(p,): (Vec<Post>,)| {
        explore::entities(&seed(&p), 60).unwrap()
    });
    check("ai/tags/related", |(p, tag): (Vec<Post>, String)| {
        explore::related(&seed(&p), &tag, 12).unwrap()
    });
    check("ai/tags/health", |(p,): (Vec<Post>,)| {
        health::health(&seed(&p)).unwrap()
    });
    check("ai/tags/suggestions", |(p,): (Vec<Post>,)| {
        merge::suggestions(&seed(&p)).unwrap()
    });
    check(
        "ai/tags/post-keys",
        |(p, tags, mode): (Vec<Post>, Vec<String>, explore::MatchMode)| {
            explore::post_keys(&seed(&p), &tags, mode).unwrap().keys
        },
    );
}
#[derive(Serialize)]
struct PostRow {
    id: String,
    ai: serde_json::Value,
    manual: serde_json::Value,
}
#[derive(Serialize)]
struct TagRow {
    id: String,
    norm: String,
    form: String,
    tier: Option<String>,
}
#[derive(Serialize)]
struct Member {
    cluster: i64,
    norm: String,
}
#[derive(Serialize)]
struct Output {
    updated: usize,
    posts: Vec<PostRow>,
    rows: Vec<TagRow>,
    members: Vec<Member>,
}
#[test]
fn merge_and_rename_match_desktop_bytes() {
    check(
        "ai/tags/merge",
        |(p, sources, target): (Vec<Post>, Vec<String>, String)| {
            let mut c = seed(&p);
            let tx = c.transaction().unwrap();
            let result = merge::merge(&tx, &sources, &target, 1).unwrap();
            tx.commit().unwrap();
            let posts = c
                .prepare(
                    "SELECT CAST(id AS TEXT),ai_tags_json,user_tags_json FROM posts ORDER BY id",
                )
                .unwrap()
                .query_map([], |r| {
                    Ok(PostRow {
                        id: r.get(0)?,
                        ai: serde_json::from_str(&r.get::<_, String>(1)?).unwrap(),
                        manual: serde_json::from_str(&r.get::<_, String>(2)?).unwrap(),
                    })
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let rows=c.prepare("SELECT CAST(post_id AS TEXT),tag_norm,tag_form,CASE WHEN source='manual' THEN 'manual' ELSE tier END FROM post_tags ORDER BY post_id,tag_norm").unwrap().query_map([],|r|Ok(TagRow{id:r.get(0)?,norm:r.get(1)?,form:r.get(2)?,tier:r.get(3)?})).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            let members = c
                .prepare("SELECT cluster_id,tag_norm FROM tag_cluster_membership ORDER BY tag_norm")
                .unwrap()
                .query_map([], |r| {
                    Ok(Member {
                        cluster: r.get(0)?,
                        norm: r.get(1)?,
                    })
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            Output {
                updated: result.updated,
                posts,
                rows,
                members,
            }
        },
    );
}
