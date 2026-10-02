//! Library counters (desktop `getStats`, DATA-20).
//!
//! Differences from the desktop: `by_platform` groups over every platform, so
//! manual bookmarks are counted (the desktop dropped them, DATA-20); trashed
//! posts are left out and counted apart; "downloaded" becomes "stored", with the
//! same rule as the `stored` list filter.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use super::{Platform, Result};

/// Library counters, trash excluded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Posts in the library.
    pub total: u64,
    /// Posts per platform; every platform is present, zero included.
    pub by_platform: BTreeMap<Platform, u64>,
    /// Posts per media type (only types that occur).
    pub by_media_type: BTreeMap<String, u64>,
    /// Posts with at least one archived object.
    pub stored: u64,
    /// Posts per kind of archived object.
    pub stored_by_kind: StoredByKind,
    /// Posts in the trash.
    pub trashed: u64,
}

/// Posts with an archived object of each kind (desktop `downloadedByType`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredByKind {
    /// Archived cover.
    pub covers: u64,
    /// At least one archived image slide.
    pub images: u64,
    /// At least one kept video.
    pub videos: u64,
}

/// Computes the counters with four aggregate queries.
///
/// # Errors
///
/// Database errors.
pub fn get(conn: &Connection) -> Result<Stats> {
    let (total, stored, covers, images, videos) = conn
        .prepare_cached(
            "SELECT count(*),
               coalesce(sum(p.cover_object IS NOT NULL OR EXISTS (SELECT 1 FROM post_media pm
                 WHERE pm.post_id = p.id
                   AND (pm.object_id IS NOT NULL OR pm.video_object_id IS NOT NULL))), 0),
               coalesce(sum(p.cover_object IS NOT NULL), 0),
               coalesce(sum(EXISTS (SELECT 1 FROM post_media pm WHERE pm.post_id = p.id
                 AND pm.kind = 'image' AND pm.object_id IS NOT NULL)), 0),
               coalesce(sum(EXISTS (SELECT 1 FROM post_media pm WHERE pm.post_id = p.id
                 AND pm.video_object_id IS NOT NULL)), 0)
             FROM posts p WHERE p.deleted_at IS NULL",
        )?
        .query_row([], |r| {
            Ok((
                count(r.get(0)?),
                count(r.get(1)?),
                count(r.get(2)?),
                count(r.get(3)?),
                count(r.get(4)?),
            ))
        })?;

    let mut by_platform: BTreeMap<Platform, u64> =
        Platform::ALL.into_iter().map(|p| (p, 0)).collect();
    let mut stmt = conn.prepare_cached(
        "SELECT platform, count(*) FROM posts WHERE deleted_at IS NULL GROUP BY platform",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, Platform>(0)?, count(r.get(1)?))))? {
        let (platform, n) = row?;
        by_platform.insert(platform, n);
    }

    let mut by_media_type = BTreeMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT media_type, count(*) FROM posts WHERE deleted_at IS NULL GROUP BY media_type",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, count(r.get(1)?))))? {
        let (media_type, n) = row?;
        by_media_type.insert(media_type, n);
    }

    let trashed = conn
        .prepare_cached("SELECT count(*) FROM posts WHERE deleted_at IS NOT NULL")?
        .query_row([], |r| Ok(count(r.get(0)?)))?;

    Ok(Stats {
        total,
        by_platform,
        by_media_type,
        stored,
        stored_by_kind: StoredByKind {
            covers,
            images,
            videos,
        },
        trashed,
    })
}

fn count(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}
