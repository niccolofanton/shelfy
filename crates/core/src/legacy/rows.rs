//! Typed rows, one struct per desktop table.
//!
//! Fields follow the catalog's column order ([`super::catalog`]): the reader
//! selects the columns in that order, with absent columns replaced by their
//! migration default. Values are read leniently, as SQLite's dynamic typing
//! allows any value in any column: text columns accept numbers (stringified),
//! integer columns accept integral reals and numeric text; anything else reads
//! as `None`.

use rusqlite::Row;
use rusqlite::types::ValueRef;

use crate::ids::Platform;

/// A desktop row that the reader can stream.
pub trait LegacyRecord: Sized {
    /// The desktop table, a key of [`super::catalog::TABLES`].
    const TABLE: &'static str;

    /// Builds the record from the next fields of a row, in catalog order.
    fn read(fields: &mut Fields<'_, '_>) -> rusqlite::Result<Self>;
}

/// Sequential, lenient access to the fields of a row.
pub struct Fields<'a, 'stmt> {
    row: &'a Row<'stmt>,
    next: usize,
}

impl<'a, 'stmt> Fields<'a, 'stmt> {
    pub(crate) fn new(row: &'a Row<'stmt>) -> Self {
        Fields { row, next: 0 }
    }

    /// Number of fields consumed so far.
    pub(crate) fn consumed(&self) -> usize {
        self.next
    }

    fn value(&mut self) -> rusqlite::Result<ValueRef<'a>> {
        let value = self.row.get_ref(self.next)?;
        self.next += 1;
        Ok(value)
    }

    /// A text field; numbers are stringified, NULL is `None`.
    pub fn text(&mut self) -> rusqlite::Result<Option<String>> {
        Ok(match self.value()? {
            ValueRef::Null => None,
            ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            ValueRef::Integer(i) => Some(i.to_string()),
            ValueRef::Real(f) => Some(f.to_string()),
        })
    }

    /// A text field of a `NOT NULL` column: NULL reads as the empty string.
    pub fn text_or_empty(&mut self) -> rusqlite::Result<String> {
        Ok(self.text()?.unwrap_or_default())
    }

    /// An integer field; integral reals and numeric text are accepted.
    pub fn int(&mut self) -> rusqlite::Result<Option<i64>> {
        Ok(match self.value()? {
            ValueRef::Integer(i) => Some(i),
            ValueRef::Real(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Some(f as i64),
            ValueRef::Text(bytes) => std::str::from_utf8(bytes)
                .ok()
                .and_then(|s| s.trim().parse().ok()),
            _ => None,
        })
    }

    /// A real field; integers and numeric text are accepted.
    pub fn real(&mut self) -> rusqlite::Result<Option<f64>> {
        Ok(match self.value()? {
            ValueRef::Real(f) => Some(f),
            ValueRef::Integer(i) => Some(i as f64),
            ValueRef::Text(bytes) => std::str::from_utf8(bytes)
                .ok()
                .and_then(|s| s.trim().parse().ok()),
            _ => None,
        })
    }
}

/// `posts`: one saved item (social post, website, manual bookmark).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostRow {
    pub id: String,
    pub platform: String,
    pub shortcode: Option<String>,
    pub post_url: Option<String>,
    pub profile_url: Option<String>,
    pub author_username: Option<String>,
    pub author_name: Option<String>,
    pub text: Option<String>,
    pub thumbnail_url: Option<String>,
    pub media_type: Option<String>,
    /// ISO 8601 text, `''` or NULL.
    pub timestamp: Option<String>,
    pub thumbnail_path: Option<String>,
    pub preview_path: Option<String>,
    pub image_path: Option<String>,
    pub video_path: Option<String>,
    pub media_count: Option<i64>,
    /// Unix seconds.
    pub imported_at: Option<i64>,
    pub ai_description: Option<String>,
    /// JSON array of strings.
    pub ai_tags: Option<String>,
    pub ai_status: Option<String>,
    pub ai_model: Option<String>,
    /// Unix seconds.
    pub ai_analyzed_at: Option<i64>,
    pub ai_category: Option<String>,
    pub ai_content_type: Option<String>,
    /// JSON array of strings.
    pub ai_entities: Option<String>,
    /// JSON array of strings.
    pub ai_keywords: Option<String>,
    pub ai_language: Option<String>,
    pub ai_save_reason: Option<String>,
    /// JSON object (v2 design catalog of a site).
    pub ai_web_json: Option<String>,
    pub user_note: Option<String>,
    /// JSON array of strings.
    pub user_tags: Option<String>,
    pub web_url: Option<String>,
    pub web_domain: Option<String>,
    pub web_final_url: Option<String>,
    pub web_palette_json: Option<String>,
    pub web_fonts_json: Option<String>,
    pub web_tech_json: Option<String>,
    pub web_awards_json: Option<String>,
    pub web_pages_json: Option<String>,
    pub web_meta_json: Option<String>,
    /// Unix seconds.
    pub web_captured_at: Option<i64>,
    /// Blur-up data URI; `''` = tried and ineligible.
    pub thumb_blur: Option<String>,
}

impl PostRow {
    /// The platform, when it is one of the five known values.
    pub fn platform(&self) -> Option<Platform> {
        Platform::parse(&self.platform)
    }
}

impl LegacyRecord for PostRow {
    const TABLE: &'static str = "posts";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(PostRow {
            id: f.text_or_empty()?,
            platform: f.text_or_empty()?,
            shortcode: f.text()?,
            post_url: f.text()?,
            profile_url: f.text()?,
            author_username: f.text()?,
            author_name: f.text()?,
            text: f.text()?,
            thumbnail_url: f.text()?,
            media_type: f.text()?,
            timestamp: f.text()?,
            thumbnail_path: f.text()?,
            preview_path: f.text()?,
            image_path: f.text()?,
            video_path: f.text()?,
            media_count: f.int()?,
            imported_at: f.int()?,
            ai_description: f.text()?,
            ai_tags: f.text()?,
            ai_status: f.text()?,
            ai_model: f.text()?,
            ai_analyzed_at: f.int()?,
            ai_category: f.text()?,
            ai_content_type: f.text()?,
            ai_entities: f.text()?,
            ai_keywords: f.text()?,
            ai_language: f.text()?,
            ai_save_reason: f.text()?,
            ai_web_json: f.text()?,
            user_note: f.text()?,
            user_tags: f.text()?,
            web_url: f.text()?,
            web_domain: f.text()?,
            web_final_url: f.text()?,
            web_palette_json: f.text()?,
            web_fonts_json: f.text()?,
            web_tech_json: f.text()?,
            web_awards_json: f.text()?,
            web_pages_json: f.text()?,
            web_meta_json: f.text()?,
            web_captured_at: f.int()?,
            thumb_blur: f.text()?,
        })
    }
}

/// `post_media`: one ordered slide of a post.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostMediaRow {
    pub post_id: String,
    pub position: i64,
    /// `image`, `video` or `file`.
    pub media_type: String,
    /// Remote URL; for manual posts the original file's local path; for web
    /// posts the page URL.
    pub source_url: Option<String>,
    pub local_path: Option<String>,
}

impl LegacyRecord for PostMediaRow {
    const TABLE: &'static str = "post_media";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(PostMediaRow {
            post_id: f.text_or_empty()?,
            position: f.int()?.unwrap_or_default(),
            media_type: f.text_or_empty()?,
            source_url: f.text()?,
            local_path: f.text()?,
        })
    }
}

/// `collections`: a user source or an imported platform folder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollectionRow {
    pub id: i64,
    pub name: String,
    pub color: String,
    /// Unix seconds.
    pub created_at: Option<i64>,
    /// NULL = manual; `instagram` / `pinterest` = native folder or board.
    pub platform: Option<String>,
    pub external_id: Option<String>,
    pub ig_name: Option<String>,
}

impl LegacyRecord for CollectionRow {
    const TABLE: &'static str = "collections";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(CollectionRow {
            id: f.int()?.unwrap_or_default(),
            name: f.text_or_empty()?,
            color: f.text_or_empty()?,
            created_at: f.int()?,
            platform: f.text()?,
            external_id: f.text()?,
            ig_name: f.text()?,
        })
    }
}

/// `post_collections`: membership of a post in a collection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostCollectionRow {
    pub post_id: String,
    pub collection_id: i64,
    /// Unix seconds.
    pub added_at: Option<i64>,
}

impl LegacyRecord for PostCollectionRow {
    const TABLE: &'static str = "post_collections";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(PostCollectionRow {
            post_id: f.text_or_empty()?,
            collection_id: f.int()?.unwrap_or_default(),
            added_at: f.int()?,
        })
    }
}

/// `post_tags`: derived tag index (AI tiers and manual tags).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostTagRow {
    pub post_id: String,
    pub tag_norm: String,
    pub tag_form: String,
    /// `general`, `specific`, `manual` or NULL (legacy AI tag).
    pub tier: Option<String>,
}

impl LegacyRecord for PostTagRow {
    const TABLE: &'static str = "post_tags";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(PostTagRow {
            post_id: f.text_or_empty()?,
            tag_norm: f.text_or_empty()?,
            tag_form: f.text_or_empty()?,
            tier: f.text()?,
        })
    }
}

/// `post_entities`: derived entity index.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostEntityRow {
    pub post_id: String,
    pub ent_norm: String,
    pub ent_form: String,
}

impl LegacyRecord for PostEntityRow {
    const TABLE: &'static str = "post_entities";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(PostEntityRow {
            post_id: f.text_or_empty()?,
            ent_norm: f.text_or_empty()?,
            ent_form: f.text_or_empty()?,
        })
    }
}

/// `post_facets`: derived design-facet index of web references.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostFacetRow {
    pub post_id: String,
    pub facet: String,
    pub value: String,
}

impl LegacyRecord for PostFacetRow {
    const TABLE: &'static str = "post_facets";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(PostFacetRow {
            post_id: f.text_or_empty()?,
            facet: f.text_or_empty()?,
            value: f.text_or_empty()?,
        })
    }
}

/// `tag_alias`: synonym → canonical tag.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagAliasRow {
    pub alias_norm: String,
    pub canonical_norm: String,
    pub canonical_form: String,
    /// `proposed` or `accepted`.
    pub status: String,
}

impl LegacyRecord for TagAliasRow {
    const TABLE: &'static str = "tag_alias";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(TagAliasRow {
            alias_norm: f.text_or_empty()?,
            canonical_norm: f.text_or_empty()?,
            canonical_form: f.text_or_empty()?,
            status: f.text_or_empty()?,
        })
    }
}

/// `tag_cluster`: an LLM-named group of tags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagClusterRow {
    pub id: i64,
    pub label: String,
    pub label_norm: Option<String>,
    pub status: String,
    /// `Date.now()` of the generating run (ms).
    pub run_id: Option<i64>,
    /// Unix seconds.
    pub created_at: Option<i64>,
    /// Unix seconds.
    pub updated_at: Option<i64>,
}

impl LegacyRecord for TagClusterRow {
    const TABLE: &'static str = "tag_cluster";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(TagClusterRow {
            id: f.int()?.unwrap_or_default(),
            label: f.text_or_empty()?,
            label_norm: f.text()?,
            status: f.text_or_empty()?,
            run_id: f.int()?,
            created_at: f.int()?,
            updated_at: f.int()?,
        })
    }
}

/// `tag_cluster_membership`: a tag's cluster.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagClusterMembershipRow {
    pub tag_norm: String,
    pub cluster_id: i64,
}

impl LegacyRecord for TagClusterMembershipRow {
    const TABLE: &'static str = "tag_cluster_membership";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(TagClusterMembershipRow {
            tag_norm: f.text_or_empty()?,
            cluster_id: f.int()?.unwrap_or_default(),
        })
    }
}

/// `web_snapshots`: an older version of a captured site.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WebSnapshotRow {
    pub id: i64,
    pub post_id: String,
    /// Unix seconds.
    pub captured_at: Option<i64>,
    pub title: Option<String>,
    pub web_pages_json: Option<String>,
    pub web_palette_json: Option<String>,
    pub web_fonts_json: Option<String>,
    pub web_tech_json: Option<String>,
    pub web_awards_json: Option<String>,
    pub web_meta_json: Option<String>,
    pub ai_description: Option<String>,
    pub ai_tags_json: Option<String>,
    pub ai_model: Option<String>,
    pub ai_status: Option<String>,
    pub ai_analyzed_at: Option<i64>,
    pub ai_category: Option<String>,
    pub ai_content_type: Option<String>,
    pub ai_entities_json: Option<String>,
    pub ai_keywords_json: Option<String>,
    pub ai_language: Option<String>,
    pub ai_save_reason: Option<String>,
    /// Unix seconds.
    pub created_at: Option<i64>,
    pub ai_web_json: Option<String>,
}

impl LegacyRecord for WebSnapshotRow {
    const TABLE: &'static str = "web_snapshots";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(WebSnapshotRow {
            id: f.int()?.unwrap_or_default(),
            post_id: f.text_or_empty()?,
            captured_at: f.int()?,
            title: f.text()?,
            web_pages_json: f.text()?,
            web_palette_json: f.text()?,
            web_fonts_json: f.text()?,
            web_tech_json: f.text()?,
            web_awards_json: f.text()?,
            web_meta_json: f.text()?,
            ai_description: f.text()?,
            ai_tags_json: f.text()?,
            ai_model: f.text()?,
            ai_status: f.text()?,
            ai_analyzed_at: f.int()?,
            ai_category: f.text()?,
            ai_content_type: f.text()?,
            ai_entities_json: f.text()?,
            ai_keywords_json: f.text()?,
            ai_language: f.text()?,
            ai_save_reason: f.text()?,
            created_at: f.int()?,
            ai_web_json: f.text()?,
        })
    }
}

/// `jobs`: the desktop's durable queue mirror.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobRow {
    pub kind: String,
    pub key: String,
    pub post_id: Option<String>,
    pub payload: Option<String>,
    pub status: String,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub attempts: Option<i64>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

impl LegacyRecord for JobRow {
    const TABLE: &'static str = "jobs";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(JobRow {
            kind: f.text_or_empty()?,
            key: f.text_or_empty()?,
            post_id: f.text()?,
            payload: f.text()?,
            status: f.text_or_empty()?,
            progress: f.real()?,
            error: f.text()?,
            attempts: f.int()?,
            created_at: f.int()?,
            updated_at: f.int()?,
        })
    }
}

/// `downloads`: dead table, never written by the desktop.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DownloadRow {
    pub id: i64,
    pub post_id: Option<String>,
    pub asset_type: String,
    pub status: String,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

impl LegacyRecord for DownloadRow {
    const TABLE: &'static str = "downloads";

    fn read(f: &mut Fields<'_, '_>) -> rusqlite::Result<Self> {
        Ok(DownloadRow {
            id: f.int()?.unwrap_or_default(),
            post_id: f.text()?,
            asset_type: f.text_or_empty()?,
            status: f.text_or_empty()?,
            progress: f.real()?,
            error: f.text()?,
            started_at: f.int()?,
            completed_at: f.int()?,
        })
    }
}
