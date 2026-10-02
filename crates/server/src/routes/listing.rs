//! What `GET /api/v1/posts` and `GET /api/v1/search` share: the filter value
//! types, input limits, the page size, opaque cursors bound to their query,
//! and the query itself (one page, and the total when asked, in one read
//! snapshot).
//!
//! **Paging** (plan §2.9, §2.14). Browsing orders (`newest`, `oldest`) page by
//! keyset on `(sortTs, key)`: a write between two pages never duplicates or
//! skips a post that was already in the list. Relevance pages by offset within
//! the first 1,000 results; a write between two pages can shift the ranking,
//! which is the accepted cost of ranked search.
//!
//! **Cursors** are opaque to clients: base64url of the core position plus a
//! fingerprint of the view and its filters. A cursor sent with other filters,
//! another sort or to the other route answers 400 `invalid_cursor`. The page
//! size is not part of the fingerprint, so a client may change it between pages.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use shelfy_core::db::UserDb;
use shelfy_core::repo::RepoError;
use shelfy_core::repo::posts::{
    self, Cursor, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, Mode, Page, PageRequest, PostFilter,
    PostSummary, Sort,
};
use utoipa::ToSchema;

use crate::error::{ApiError, ErrorCode};
use crate::state::blocking;

/// Longest free-text query, in characters.
pub const MAX_QUERY_CHARS: usize = 500;
/// Longest single filter value (a tag, an entity, a concept…), in characters.
pub const MAX_VALUE_CHARS: usize = 200;
/// Most values of a repeatable filter (`tags`, `concept`, `mediaType`).
pub const MAX_VALUES: usize = 50;
/// Longest cursor accepted, in characters.
const MAX_CURSOR_CHARS: usize = 256;

/// How several values of a list filter combine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    /// Any of them.
    #[default]
    Or,
    /// All of them.
    And,
}

impl From<MatchMode> for Mode {
    fn from(mode: MatchMode) -> Self {
        match mode {
            MatchMode::Or => Self::Or,
            MatchMode::And => Self::And,
        }
    }
}

/// Order of a list of posts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PostSort {
    /// Most recent first (by `sortTs`).
    Newest,
    /// Oldest first.
    Oldest,
    /// Best search match first, then newest. Applies only with search text
    /// (`q` or `concept`); without it the order is `newest`.
    Relevance,
}

impl PostSort {
    /// The order actually used: `requested`, else relevance when there is
    /// search text and newest otherwise; relevance without text is newest.
    #[must_use]
    pub fn effective(requested: Option<Self>, has_text: bool) -> Self {
        match requested {
            Some(Self::Relevance) | None if has_text => Self::Relevance,
            Some(Self::Oldest) => Self::Oldest,
            _ => Self::Newest,
        }
    }
}

impl From<PostSort> for Sort {
    fn from(sort: PostSort) -> Self {
        match sort {
            PostSort::Newest => Self::Newest,
            PostSort::Oldest => Self::Oldest,
            PostSort::Relevance => Self::Relevance,
        }
    }
}

/// A yes/no filter value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum YesNo {
    /// Only posts that have it.
    Yes,
    /// Only posts that do not.
    No,
}

impl YesNo {
    /// `true` for [`YesNo::Yes`].
    #[must_use]
    pub fn is_yes(self) -> bool {
        self == Self::Yes
    }
}

/// The page size: `limit`, else 60, clamped to 1–200.
#[must_use]
pub fn page_size(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE)
}

/// Deserializes a boolean flag: `true`/`1` or `false`/`0` (the plan writes
/// `trash=1`; generated clients send `true`).
///
/// # Errors
///
/// Any other value.
pub fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    let value = String::deserialize(deserializer)?;
    match value.as_str() {
        "" => Ok(None),
        "true" | "1" => Ok(Some(true)),
        "false" | "0" => Ok(Some(false)),
        other => Err(serde::de::Error::invalid_value(
            serde::de::Unexpected::Str(other),
            &"true, false, 1 or 0",
        )),
    }
}

/// Refuses a single text value longer than `max` characters (422
/// `validation_failed` naming `field`).
///
/// # Errors
///
/// The value is too long.
pub fn check_text(field: &'static str, value: Option<&str>, max: usize) -> Result<(), ApiError> {
    match value {
        Some(v) if v.chars().count() > max => Err(ApiError::invalid_field(
            field,
            format!("longer than {max} characters"),
        )),
        _ => Ok(()),
    }
}

/// Refuses a repeatable parameter given more than [`MAX_VALUES`] times.
///
/// # Errors
///
/// Too many values.
pub fn check_count(field: &'static str, count: usize) -> Result<(), ApiError> {
    if count > MAX_VALUES {
        return Err(ApiError::invalid_field(
            field,
            format!("more than {MAX_VALUES} values"),
        ));
    }
    Ok(())
}

/// Refuses a repeatable text parameter with more than [`MAX_VALUES`] values
/// or a value longer than [`MAX_VALUE_CHARS`] characters.
///
/// # Errors
///
/// Too many values, or one is too long.
pub fn check_values(field: &'static str, values: &[String]) -> Result<(), ApiError> {
    check_count(field, values.len())?;
    values
        .iter()
        .try_for_each(|v| check_text(field, Some(v), MAX_VALUE_CHARS))
}

/// Encodes and checks the cursors of one view with one set of filters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursors {
    fingerprint: String,
}

impl Cursors {
    /// Cursors of `view` (`posts`, `search`) for `filters`: the normalized
    /// filter parameters and the effective sort, without the page size or
    /// the cursor itself.
    ///
    /// # Panics
    ///
    /// When `filters` cannot be serialized to JSON; filter types always can.
    #[must_use]
    pub fn new(view: &str, filters: &impl Serialize) -> Self {
        let filters = serde_json::to_vec(filters).expect("filters serialize");
        let mut hash = Sha256::new();
        hash.update(view.as_bytes());
        hash.update([0]);
        hash.update(&filters);
        let digest = hash.finalize();
        let fingerprint = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        Self { fingerprint }
    }

    /// The opaque text of `cursor`.
    #[must_use]
    pub fn encode(&self, cursor: &Cursor) -> String {
        URL_SAFE_NO_PAD.encode(format!("{}.{cursor}", self.fingerprint))
    }

    /// The position in `text`, if this view and these filters produced it.
    ///
    /// # Errors
    ///
    /// 400 `invalid_cursor` for anything else.
    pub fn decode(&self, text: &str) -> Result<Cursor, ApiError> {
        let invalid = || ApiError::new(ErrorCode::InvalidCursor);
        if text.len() > MAX_CURSOR_CHARS {
            return Err(invalid());
        }
        let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| invalid())?;
        let payload = String::from_utf8(bytes).map_err(|_| invalid())?;
        let (fingerprint, position) = payload.split_once('.').ok_or_else(invalid)?;
        if fingerprint != self.fingerprint {
            return Err(
                invalid().with_detail("the cursor belongs to other filters or another sort")
            );
        }
        Cursor::parse(position).map_err(|_| invalid())
    }
}

/// One page of posts and, when asked, the total.
pub struct PostsResult {
    /// The page.
    pub page: Page<PostSummary>,
    /// Posts matching the filter.
    pub total: Option<u64>,
}

/// Runs the list query, and the count when `with_total`, in one read snapshot
/// of `db`, on the blocking pool.
///
/// # Errors
///
/// `invalid_cursor` when the position belongs to another order; database
/// errors otherwise.
pub async fn fetch(
    db: Arc<UserDb>,
    filter: PostFilter,
    page: PageRequest,
    with_total: bool,
) -> Result<PostsResult, ApiError> {
    blocking(move || {
        db.read(|conn| {
            let page = posts::list(conn, &filter, &page)?;
            let total = if with_total {
                Some(posts::count(conn, &filter)?)
            } else {
                None
            };
            Ok::<_, RepoError>(PostsResult { page, total })
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_effective_sort_follows_the_search_text() {
        use PostSort::{Newest, Oldest, Relevance};
        assert_eq!(PostSort::effective(None, false), Newest);
        assert_eq!(PostSort::effective(None, true), Relevance);
        assert_eq!(PostSort::effective(Some(Relevance), false), Newest);
        assert_eq!(PostSort::effective(Some(Relevance), true), Relevance);
        assert_eq!(PostSort::effective(Some(Oldest), true), Oldest);
        assert_eq!(PostSort::effective(Some(Newest), true), Newest);
    }

    #[test]
    fn page_sizes_are_clamped() {
        assert_eq!(page_size(None), 60);
        assert_eq!(page_size(Some(0)), 1);
        assert_eq!(page_size(Some(25)), 25);
        assert_eq!(page_size(Some(10_000)), 200);
    }

    #[test]
    fn cursors_round_trip_only_with_their_filters() {
        let cursors = Cursors::new("posts", &("platform", "instagram", "newest"));
        let position = Cursor::Newest {
            sort_ts: 1_790_899_200_000,
            id: 42,
        };
        let text = cursors.encode(&position);
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "{text}"
        );
        assert_eq!(cursors.decode(&text).unwrap(), position);

        let others = [
            Cursors::new("posts", &("platform", "twitter", "newest")),
            Cursors::new("search", &("platform", "instagram", "newest")),
        ];
        for other in others {
            let err = other.decode(&text).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidCursor);
        }
        let garbage = [
            String::new(),
            "not base64!".to_owned(),
            URL_SAFE_NO_PAD.encode("no-separator"),
            URL_SAFE_NO_PAD.encode(format!("{}.n.x.y", cursors.fingerprint)),
            URL_SAFE_NO_PAD.encode([0xff, 0xfe]),
            "A".repeat(MAX_CURSOR_CHARS + 1),
        ];
        for text in garbage {
            let err = cursors.decode(&text).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidCursor, "{text}");
        }
    }

    #[test]
    fn long_values_fail_validation() {
        assert!(check_text("q", Some(&"a".repeat(MAX_QUERY_CHARS)), MAX_QUERY_CHARS).is_ok());
        let err =
            check_text("q", Some(&"é".repeat(MAX_QUERY_CHARS + 1)), MAX_QUERY_CHARS).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ValidationFailed);
        assert_eq!(err.problem().errors[0].field, "q");
        let many = vec!["tag".to_owned(); MAX_VALUES + 1];
        assert!(check_values("tags", &many[..MAX_VALUES]).is_ok());
        assert!(check_values("tags", &many).is_err());
        assert!(check_values("tags", &["x".repeat(MAX_VALUE_CHARS + 1)]).is_err());
        assert!(check_count("mediaType", MAX_VALUES + 1).is_err());
    }
}
