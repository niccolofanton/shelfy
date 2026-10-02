//! `POST /api/v1/me/consent` (plan §2.11, §7.2): the disclaimer and the
//! privacy notice the user accepted, with the time, audit-logged
//! (`consent.accept`). The web app's disclaimer gate records it here; the
//! desktop keeps its own in localStorage.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::control::audit::{self, Entry};
use crate::control::users;
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::Json;
use crate::ids::now_ms;
use crate::routes::auth::no_store;
use crate::state::{AppState, blocking};

/// Longest version string.
pub const MAX_VERSION_CHARS: usize = 32;

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(accept))
}

/// What the user accepted; `null` until accepted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Consent {
    /// The disclaimer's version.
    #[schema(required = true)]
    pub disclaimer_version: Option<String>,
    /// When the disclaimer was accepted, unix ms.
    #[schema(required = true)]
    pub disclaimer_accepted_at: Option<i64>,
    /// The privacy notice's version.
    #[schema(required = true)]
    pub privacy_version: Option<String>,
    /// When the privacy notice was accepted, unix ms.
    #[schema(required = true)]
    pub privacy_accepted_at: Option<i64>,
}

impl From<users::Consent> for Consent {
    fn from(consent: users::Consent) -> Self {
        Self {
            disclaimer_version: consent.disclaimer_version,
            disclaimer_accepted_at: consent.disclaimer_accepted_at,
            privacy_version: consent.privacy_version,
            privacy_accepted_at: consent.privacy_accepted_at,
        }
    }
}

/// Body of `POST /api/v1/me/consent`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConsentRequest {
    /// The version of the disclaimer the app showed (`2026-10`): 1–32
    /// characters among `A–Z`, `a–z`, `0–9`, `.`, `_` and `-`.
    pub disclaimer_version: String,
    /// The version of the privacy notice the app showed (`1`), alike.
    pub privacy_version: String,
}

/// Whether `version` is a version string (see [`ConsentRequest`]).
fn is_version(version: &str) -> bool {
    (1..=MAX_VERSION_CHARS).contains(&version.len())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Records that the user accepted the disclaimer and the privacy notice of
/// these versions, now. Accepting again (a new version) replaces both, with
/// the new time. 422 for a malformed version.
#[utoipa::path(
    post,
    path = "/api/v1/me/consent",
    tag = "account",
    operation_id = "acceptConsent",
    request_body = ConsentRequest,
    responses(
        (status = OK, description = "The consent, as recorded.", body = Consent),
    )
)]
pub async fn accept(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<ConsentRequest>,
) -> Result<Response, ApiError> {
    let mut invalid = None::<ApiError>;
    for (field, value) in [
        ("disclaimerVersion", &request.disclaimer_version),
        ("privacyVersion", &request.privacy_version),
    ] {
        if !is_version(value) {
            let reason = "must be 1 to 32 characters among letters, digits, '.', '_' and '-'";
            invalid = Some(match invalid {
                Some(err) => err.with_field(field, reason),
                None => ApiError::invalid_field(field, reason),
            });
        }
    }
    if let Some(err) = invalid {
        return Err(err);
    }
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let now = now_ms();
    let consent = blocking(move || {
        control.write(|tx| {
            let ConsentRequest {
                disclaimer_version,
                privacy_version,
            } = &request;
            if !users::set_consent(tx, &user_id, disclaimer_version, privacy_version, now)? {
                return Ok(None);
            }
            let meta = json!({
                "disclaimerVersion": disclaimer_version,
                "privacyVersion": privacy_version,
            });
            let entry = Entry {
                action: audit::CONSENT_ACCEPT,
                actor_user_id: Some(&user_id),
                target: Some(&user_id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            users::consent(tx, &user_id)
        })
    })
    .await?
    .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))?;
    Ok(no_store(Json(Consent::from(consent)).into_response()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_short_tokens() {
        for good in ["1", "2026-10", "v2.1_b", &"a".repeat(32)] {
            assert!(is_version(good), "{good}");
        }
        for bad in ["", " 1", "1 ", "v1/2", "é", &"a".repeat(33), "<script>"] {
            assert!(!is_version(bad), "{bad}");
        }
    }
}
