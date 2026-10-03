//! Cookie-authenticated, one-recording dictation; no audio is persisted.
use crate::{
    ai::{CallHints, Caller},
    current_user::CurrentUser,
    error::{ApiError, ErrorCode},
    extract::{Json, Query},
    state::AppState,
};
use axum::{
    body::Bytes,
    extract::{State, rejection::BytesRejection},
    http::{HeaderMap, header},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct Language {
    pub language: Option<String>,
}
#[derive(ToSchema)]
#[schema(format = Binary)]
pub struct WavBody(pub String);
#[derive(Serialize, ToSchema)]
pub struct Transcription {
    pub text: String,
}
#[utoipa::path(post, path="/api/v1/stt/transcriptions", tag="library", operation_id="transcribeAudio",
    params(Language), request_body(content=WavBody, content_type="audio/wav"),
    responses((status=OK, description="Final transcript; audio is discarded. 16 kHz mono 16-bit PCM, at most 120 seconds and 25 MiB; 10 requests/minute per user.", body=Transcription)))]
pub async fn transcribe_audio(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<Language>,
    headers: HeaderMap,
    bytes: Result<Bytes, BytesRejection>,
) -> Result<Json<Transcription>, ApiError> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(';').next().unwrap_or_default().trim())
        != Some("audio/wav")
    {
        return Err(ApiError::new(ErrorCode::UnsupportedMediaType));
    }
    let language = query.language;
    if language
        .as_ref()
        .is_some_and(|l| l.len() != 2 || !l.bytes().all(|b| b.is_ascii_lowercase()))
    {
        return Err(ApiError::invalid_field(
            "language",
            "requires a two-letter lowercase language code",
        ));
    }
    let wav = bytes.map_err(|e| ApiError::from_status(e.status()))?;
    crate::ai::stt::validate_wav(&wav)?;
    let owner = crate::jobs::ai_drain::is_owner(&state, user.id()).await?;
    let request = shelfy_ai::TranscribeRequest {
        wav,
        language,
        model: None,
    };
    // Timeout/connection teardown drops the guard and cancels the provider call.
    let cancel = state.shutdown_token().child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let result = state
        .ai()
        .transcribe(
            &state,
            Caller::new(user.id(), owner),
            &request,
            CallHints::default().with_cancel(cancel),
        )
        .await?;
    Ok(Json(Transcription { text: result.text }))
}
