//! Live-library taxonomy statistics; overlapping AI/manual tags count once.
use crate::repo::{Result, tags};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    General,
    Specific,
    Manual,
    #[default]
    All,
}
impl Tier {
    fn sql(self) -> &'static str {
        match self {
            Self::General => " AND t.tier='general'",
            Self::Specific => " AND t.tier='specific'",
            Self::Manual => " AND t.source='manual'",
            Self::All => "",
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CategoryCount {
    pub category: String,
    pub count: u64,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContentTypeCount {
    pub content_type: String,
    pub count: u64,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LanguageCount {
    pub language: String,
    pub count: u64,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub total: u64,
    pub analyzed: u64,
    pub unanalyzed: u64,
    pub by_category: Vec<CategoryCount>,
    pub by_content_type: Vec<ContentTypeCount>,
    pub languages: Vec<LanguageCount>,
    pub unique_tags: u64,
    pub tagged_posts: u64,
}
pub(crate) fn counts(
    conn: &Connection,
    column: &str,
    include_none: bool,
) -> Result<Vec<(String, u64)>> {
    // Callers supply only hard-coded schema column names.
    let filter = if include_none {
        ""
    } else {
        " AND v IS NOT NULL AND v<>''"
    };
    let expression = if include_none {
        format!("COALESCE({column},'none')")
    } else {
        column.to_owned()
    };
    let sql = format!(
        "SELECT v,COUNT(*) FROM (SELECT {expression} AS v FROM posts WHERE deleted_at IS NULL) WHERE 1=1 {filter} GROUP BY v ORDER BY COUNT(*) DESC,v DESC"
    );
    Ok(conn
        .prepare_cached(&sql)?
        .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?
        .collect::<rusqlite::Result<_>>()?)
}
pub fn overview(conn: &Connection) -> Result<Overview> {
    let (total, analyzed) = conn.query_row(
        "SELECT COUNT(*),COALESCE(SUM(ai_status='done'),0) FROM posts WHERE deleted_at IS NULL",
        [],
        |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
    )?;
    let (unique_tags,tagged_posts)=conn.query_row("SELECT COUNT(DISTINCT t.tag_norm),COUNT(DISTINCT t.post_id) FROM post_tags t JOIN posts p ON p.id=t.post_id WHERE p.deleted_at IS NULL",[],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64)))?;
    Ok(Overview {
        total,
        analyzed,
        unanalyzed: total - analyzed,
        unique_tags,
        tagged_posts,
        by_category: counts(conn, "ai_category", false)?
            .into_iter()
            .map(|(category, count)| CategoryCount { category, count })
            .collect(),
        by_content_type: counts(conn, "ai_content_type", false)?
            .into_iter()
            .map(|(content_type, count)| ContentTypeCount {
                content_type,
                count,
            })
            .collect(),
        languages: counts(conn, "ai_language", false)?
            .into_iter()
            .map(|(language, count)| LanguageCount { language, count })
            .collect(),
    })
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TagStat {
    pub tag: String,
    pub count: u64,
    pub last_used: Option<i64>,
    pub categories: Vec<CategoryCount>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EntityStat {
    pub entity: String,
    pub count: u64,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RelatedTag {
    pub tag: String,
    pub count: u64,
}
pub fn stats(conn: &Connection, tier: Tier, limit: usize) -> Result<Vec<TagStat>> {
    let sql = format!(
        "SELECT t.tag_norm,COUNT(DISTINCT t.post_id),MAX(p.posted_at) FROM post_tags t JOIN posts p ON p.id=t.post_id WHERE p.deleted_at IS NULL{} GROUP BY t.tag_norm ORDER BY COUNT(DISTINCT t.post_id) DESC,t.tag_norm LIMIT ?1",
        tier.sql()
    );
    let rows: Vec<(String, u64, Option<i64>)> = conn
        .prepare_cached(&sql)?
        .query_map([limit.min(500) as i64], |r| {
            Ok((r.get(0)?, r.get::<_, i64>(1)? as u64, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let keys = serde_json::to_string(&rows.iter().map(|r| &r.0).collect::<Vec<_>>())
        .expect("tag strings serialize");
    let mut categories: HashMap<String, Vec<CategoryCount>> = HashMap::new();
    let cats:Vec<(String,String,u64)>=conn.prepare_cached("SELECT t.tag_norm,p.ai_category,COUNT(DISTINCT t.post_id) FROM post_tags t JOIN posts p ON p.id=t.post_id WHERE t.tag_norm IN (SELECT value FROM json_each(?1)) AND p.deleted_at IS NULL AND p.ai_category IS NOT NULL AND p.ai_category<>'' GROUP BY t.tag_norm,p.ai_category ORDER BY t.tag_norm,COUNT(DISTINCT t.post_id) DESC,p.ai_category")?.query_map([keys],|r|Ok((r.get(0)?,r.get(1)?,r.get::<_,i64>(2)? as u64)))?.collect::<rusqlite::Result<_>>()?;
    for (key, category, count) in cats {
        categories
            .entry(key)
            .or_default()
            .push(CategoryCount { category, count });
    }
    rows.into_iter()
        .map(|(key, count, last_used)| {
            Ok(TagStat {
                tag: best_form(conn, &key)?,
                count,
                last_used,
                categories: categories.remove(&key).unwrap_or_default(),
            })
        })
        .collect()
}
pub(crate) fn best_form(conn: &Connection, key: &str) -> Result<String> {
    Ok(conn.query_row("SELECT t.tag_form FROM post_tags t JOIN posts p ON p.id=t.post_id WHERE t.tag_norm=?1 AND p.deleted_at IS NULL GROUP BY t.tag_form ORDER BY COUNT(DISTINCT t.post_id) DESC,t.tag_form LIMIT 1",[key],|r|r.get(0))?)
}
pub fn entities(conn: &Connection, limit: usize) -> Result<Vec<EntityStat>> {
    Ok(conn.prepare_cached("SELECT (SELECT f.ent_form FROM post_entities f JOIN posts fp ON fp.id=f.post_id WHERE f.ent_norm=e.ent_norm AND fp.deleted_at IS NULL GROUP BY f.ent_form ORDER BY COUNT(*) DESC,f.ent_form LIMIT 1),COUNT(*) FROM post_entities e JOIN posts p ON p.id=e.post_id WHERE p.deleted_at IS NULL GROUP BY e.ent_norm ORDER BY COUNT(*) DESC,e.ent_norm LIMIT ?1")?.query_map([limit.min(500) as i64],|r|Ok(EntityStat{entity:r.get(0)?,count:r.get::<_,i64>(1)? as u64}))?.collect::<rusqlite::Result<_>>()?)
}
pub fn related(conn: &Connection, tag: &str, limit: usize) -> Result<Vec<RelatedTag>> {
    let key = tags::resolve_alias(conn, &super::norm(tag))?.norm;
    let rows:Vec<(String,u64)>=conn.prepare_cached("SELECT b.tag_norm,COUNT(DISTINCT b.post_id) FROM post_tags b JOIN posts p ON p.id=b.post_id WHERE p.deleted_at IS NULL AND b.tag_norm<>?1 AND EXISTS(SELECT 1 FROM post_tags a WHERE a.post_id=b.post_id AND a.tag_norm=?1) GROUP BY b.tag_norm ORDER BY COUNT(DISTINCT b.post_id) DESC,b.tag_norm DESC LIMIT ?2")?.query_map(params![key,limit.min(12) as i64],|r|Ok((r.get(0)?,r.get::<_,i64>(1)? as u64)))?.collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|(key, count)| {
            Ok(RelatedTag {
                tag: best_form(conn, &key)?,
                count,
            })
        })
        .collect()
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    And,
    #[default]
    Or,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PostKeys {
    pub keys: Vec<String>,
    pub truncated: bool,
}
pub fn post_keys(conn: &Connection, wanted: &[String], mode: MatchMode) -> Result<PostKeys> {
    let mut keys = std::collections::BTreeSet::new();
    for tag in wanted {
        let key = tags::resolve_alias(conn, &super::norm(tag))?.norm;
        if !key.is_empty() {
            keys.insert(key);
        }
    }
    if keys.is_empty() {
        return Ok(PostKeys {
            keys: vec![],
            truncated: false,
        });
    }
    let json = serde_json::to_string(&keys).expect("tag strings serialize");
    let having = if mode == MatchMode::And {
        " HAVING COUNT(DISTINCT t.tag_norm)=?2"
    } else {
        " HAVING ?2>=0"
    };
    let order = if mode == MatchMode::And {
        "p.id"
    } else {
        "MIN(t.tag_norm),p.id"
    };
    let sql = format!(
        "SELECT p.key FROM post_tags t JOIN posts p ON p.id=t.post_id WHERE p.deleted_at IS NULL AND t.tag_norm IN (SELECT value FROM json_each(?1)) GROUP BY p.id{having} ORDER BY {order} LIMIT 10001"
    );
    let mut out: Vec<String> = conn
        .prepare_cached(&sql)?
        .query_map(params![json, keys.len() as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let truncated = out.len() > 10000;
    out.truncate(10000);
    Ok(PostKeys {
        keys: out,
        truncated,
    })
}
