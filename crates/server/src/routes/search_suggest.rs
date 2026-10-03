//! Cookie-only suggestions for gallery concept filters.
use super::search::SearchScope;
use crate::{current_user::CurrentUser, error::ApiError, extract::Json, state::AppState};
use axum::extract::State;
use serde::Deserialize;
use shelfy_core::repo::posts::SourceBucket;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestBody {
    pub q: String,
    #[serde(default)]
    pub scope: SearchScope,
}

#[utoipa::path(post,path="/api/v1/search/suggest",tag="library",operation_id="searchSuggest",request_body=SuggestBody,
    responses((status=OK,description="Related tags present in this user's live scoped vocabulary; empty with a reason when unavailable.",body=crate::ai::suggest::SuggestResult)))]
pub async fn search_suggest(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<SuggestBody>,
) -> Result<Json<crate::ai::suggest::SuggestResult>, ApiError> {
    super::listing::check_text("q", Some(&body.q), super::listing::MAX_QUERY_CHARS)?;
    let source = match body.scope {
        SearchScope::All => None,
        SearchScope::Sites => Some(SourceBucket::Web),
        SearchScope::Social => Some(SourceBucket::Social),
    };
    crate::ai::suggest::suggest(&state, user.id(), body.q, source)
        .await
        .map(Json)
}
