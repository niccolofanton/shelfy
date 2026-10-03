//! Facet values of live posts. Missing AI status is represented by `none`.
use crate::repo::Result;
use rusqlite::Connection;
use serde::Serialize;
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Facet {
    pub value: String,
    pub count: u64,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    pub category: Vec<Facet>,
    pub content_type: Vec<Facet>,
    pub status: Vec<Facet>,
    pub language: Vec<Facet>,
}
pub fn facets(conn: &Connection) -> Result<Facets> {
    let read = |column, none| -> Result<Vec<Facet>> {
        Ok(super::explore::counts(conn, column, none)?
            .into_iter()
            .map(|(value, count)| Facet { value, count })
            .collect())
    };
    Ok(Facets {
        category: read("ai_category", false)?,
        content_type: read("ai_content_type", false)?,
        status: read("ai_status", true)?,
        language: read("ai_language", false)?,
    })
}
