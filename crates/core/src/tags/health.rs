//! Tag hygiene computed from live posts.
use crate::repo::Result;
use rusqlite::Connection;
use serde::Serialize;
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OrphanTag {
    pub tag: String,
    pub count: u64,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub orphan_tags: Vec<OrphanTag>,
    pub rare_tags: usize,
    pub unanalyzed_posts: u64,
    pub untagged_posts: u64,
}
pub fn health(conn: &Connection) -> Result<Health> {
    // For a tag present in exactly one live post, every form has frequency
    // one (including an AI/manual overlap); MIN chooses the same tied form.
    let rows:Vec<(String,u64)>=conn.prepare_cached("SELECT MIN(t.tag_form),COUNT(DISTINCT t.post_id) FROM post_tags t JOIN posts p ON p.id=t.post_id WHERE p.deleted_at IS NULL GROUP BY t.tag_norm HAVING COUNT(DISTINCT t.post_id)<=2")?.query_map([],|r|Ok((r.get(0)?,r.get::<_,i64>(1)? as u64)))?.collect::<rusqlite::Result<_>>()?;
    let rare_tags = rows.len();
    let mut orphan_tags: Vec<_> = rows
        .into_iter()
        .filter(|t| t.1 == 1)
        .map(|(tag, _)| OrphanTag { tag, count: 1 })
        .collect();
    orphan_tags.sort_by(|a, b| {
        super::js_cmp(&a.tag.to_lowercase(), &b.tag.to_lowercase())
            .then_with(|| super::js_cmp(&a.tag, &b.tag))
    });
    let (unanalyzed_posts,untagged_posts)=conn.query_row("SELECT COALESCE(SUM(ai_status IS NULL OR ai_status<>'done'),0),COALESCE(SUM(ai_status='done' AND NOT EXISTS(SELECT 1 FROM post_tags t WHERE t.post_id=posts.id)),0) FROM posts WHERE deleted_at IS NULL",[],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64)))?;
    Ok(Health {
        orphan_tags,
        rare_tags,
        unanalyzed_posts,
        untagged_posts,
    })
}
