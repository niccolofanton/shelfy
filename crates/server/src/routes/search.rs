//! `GET /api/v1/search`: the hybrid tag and text search of the AI views
//! (plan §2.9 Search; App. A `search:byText`, `search:byTags`,
//! `search:hybrid`).
//!
//! It is a separate route because the plan gives it its own parameters
//! (`scope`, `tags` + `tagMode`), its own budget (§6.2: p95 ≤ 60 ms against
//! 40 ms for a gallery page) and a search-shaped answer: always ranked, always
//! with the total, empty without criteria. The engine is the gallery's:
//! `GET /api/v1/posts?q=…` with the same filters returns the same ranking, so
//! the search-eval gate (P1-05) covers both.
//!
//! With text, results are ranked by relevance. Tags alone are listed newest
//! first: the desktop's tag-weight ranking (`searchPostsByTags`, Σ idf of the
//! matched tags) has no core equivalent yet (P3, with the AI views).

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use shelfy_core::repo::posts::PostFilter;
use utoipa::{IntoParams, ToSchema};

use super::listing::{
    self, Cursors, ListRequest, MAX_QUERY_CHARS, MatchMode, PostSort, check_text, check_values,
    page_size,
};
use super::model::{Post, SearchPage};
use super::posts::{PostSource, PostsQuery};
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::{Json, Query};
use crate::state::AppState;

/// Which posts a search covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchScope {
    /// Every post.
    #[default]
    All,
    /// Websites only.
    Sites,
    /// Everything but websites.
    Social,
}

/// Criteria and paging of `GET /api/v1/search`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, IntoParams)]
#[serde(default, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// Free text, matched against tags, keywords, entities, description, note,
    /// caption, author and page text (plan §2.14). At most 500 characters.
    pub q: Option<String>,
    /// Tags, combined by `tagMode`. Repeatable. With `q` in `or` mode, a post
    /// with one of the tags or a text match is a result, and its tags add to
    /// its score; in `and` mode every tag is required.
    #[param(style = Form, explode)]
    pub tags: Vec<String>,
    /// How `tags` combine. Default `or`.
    pub tag_mode: Option<MatchMode>,
    /// Suggested concepts: more search terms, combined with `q` by
    /// `conceptMode`. Repeatable.
    #[param(style = Form, explode)]
    pub concept: Vec<String>,
    /// How `q` and the concepts combine. Default `or`.
    pub concept_mode: Option<MatchMode>,
    /// `all` (default), `sites` or `social`.
    pub scope: Option<SearchScope>,
    /// Page size, 1–200 (larger values are clamped). Default 60.
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    /// `nextCursor` of the previous page, with the same criteria.
    pub cursor: Option<String>,
}

impl SearchQuery {
    fn validate(&self) -> Result<(), ApiError> {
        check_text("q", self.q.as_deref(), MAX_QUERY_CHARS)?;
        check_values("tags", &self.tags)?;
        check_values("concept", &self.concept)
    }

    /// The same criteria as filters of `GET /posts`: that list with these
    /// filters ranks the same posts, and the two share cached rankings and
    /// counts.
    #[must_use]
    pub fn as_posts_query(&self) -> PostsQuery {
        PostsQuery {
            source: match self.scope.unwrap_or_default() {
                SearchScope::All => None,
                SearchScope::Sites => Some(PostSource::Web),
                SearchScope::Social => Some(PostSource::Social),
            },
            tags: self.tags.clone(),
            tag_mode: self.tag_mode,
            q: self.q.clone(),
            concept: self.concept.clone(),
            concept_mode: self.concept_mode,
            ..PostsQuery::default()
        }
    }

    fn filter(&self) -> PostFilter {
        self.as_posts_query().filter()
    }
}

/// Whether `filter` has anything to search for: text, a concept or a tag. The
/// core ignores blank values (JavaScript's `trim`: Unicode white space and the
/// BOM), so a blank-only search must not list the whole library.
fn has_criteria(filter: &PostFilter) -> bool {
    let blank = |c: char| c.is_whitespace() || c == '\u{feff}';
    filter.has_text()
        || filter
            .tags
            .iter()
            .any(|t| !t.trim_matches(blank).is_empty())
}

/// Search the library: ranked by relevance when there is text, with the total.
/// Without text, tags or concepts the answer is empty.
#[utoipa::path(
    get,
    path = "/api/v1/search",
    tag = "search",
    operation_id = "search",
    params(SearchQuery, ConditionalHeaders),
    responses(
        (
            status = OK,
            description = "One page of results, best match first.",
            body = SearchPage,
            headers(
                ("ETag" = String, description = "Weak ETag of this page of this library state."),
                ("Cache-Control" = String, description = "`private, no-cache`."),
            )
        ),
        (
            status = NOT_MODIFIED,
            description = "The page is unchanged since the ETag in `If-None-Match`; no body.",
            headers(("ETag" = String, description = "The same ETag."))
        ),
    )
)]
pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Response, ApiError> {
    serve_search(&state, user.id(), &headers, query).await
}

/// Answers `GET /search` for `user_id`: the work of the route after
/// authentication, callable in process (`admin bench`, P1-05).
///
/// # Errors
///
/// Like the route: invalid input, an invalid cursor, the database.
pub async fn serve_search(
    state: &AppState,
    user_id: &str,
    headers: &HeaderMap,
    query: SearchQuery,
) -> Result<Response, ApiError> {
    query.validate()?;
    let filter = query.filter();
    let sort = PostSort::effective(None, filter.has_text());
    let limit = page_size(query.limit);
    let filters = SearchQuery {
        limit: None,
        cursor: None,
        ..query.clone()
    };
    let cursors = Cursors::new("search", &(&filters, sort));
    let position = query
        .cursor
        .as_deref()
        .map(|text| cursors.decode(text))
        .transpose()?;

    let db = state.user_db(user_id).await?;
    // Read before the snapshot (`crate::conditional`).
    let generation = db.generation();
    let etag = ETag::for_view(
        "search",
        user_id,
        generation,
        &(&filters, limit, &query.cursor),
    );
    if etag.matches(headers) {
        return Ok(etag.not_modified());
    }
    if !has_criteria(&filter) {
        let empty = SearchPage {
            items: Vec::new(),
            next_cursor: None,
            total: 0,
        };
        return Ok(etag.respond(Json(empty)));
    }
    let request = ListRequest {
        filter,
        key: listing::cache_key(&query.as_posts_query()),
        sort: sort.into(),
        limit,
        position,
        with_total: true,
    };
    let result = listing::fetch(state, user_id, db, generation, request).await?;
    let body = SearchPage {
        items: result.page.items.into_iter().map(Post::from).collect(),
        next_cursor: result.next.map(|p| cursors.encode(&p)),
        total: result.total.unwrap_or(0),
    };
    Ok(etag.respond(Json(body)))
}

#[cfg(test)]
mod tests {
    use shelfy_core::repo::posts::SourceBucket;

    use super::*;

    #[test]
    fn blank_values_are_no_criteria() {
        let blank = SearchQuery {
            q: Some(" \u{feff}\t".into()),
            tags: vec!["\u{feff}".into(), " ".into()],
            concept: vec!["\n".into()],
            ..SearchQuery::default()
        };
        assert!(!has_criteria(&blank.filter()));
        assert!(!has_criteria(&SearchQuery::default().filter()));
        let given = [
            SearchQuery {
                q: Some("lamp".into()),
                ..SearchQuery::default()
            },
            SearchQuery {
                tags: vec![" glass ".into()],
                ..SearchQuery::default()
            },
            SearchQuery {
                concept: vec!["wood".into()],
                ..SearchQuery::default()
            },
        ];
        for query in given {
            assert!(has_criteria(&query.filter()), "{query:?}");
        }
    }

    #[test]
    fn scopes_map_to_source_buckets() {
        let scoped = |scope| {
            SearchQuery {
                scope: Some(scope),
                ..SearchQuery::default()
            }
            .filter()
            .source
        };
        assert_eq!(scoped(SearchScope::All), None);
        assert_eq!(scoped(SearchScope::Sites), Some(SourceBucket::Web));
        assert_eq!(scoped(SearchScope::Social), Some(SourceBucket::Social));
    }
}
