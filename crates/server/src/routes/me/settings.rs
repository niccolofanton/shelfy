//! `GET,PUT /api/v1/me/settings` (plan §2.9 Account, §4.2): the account's
//! settings, kept in the user's library (`settings`,
//! [`shelfy_core::repo::settings`]). Only the allowlisted keys exist: the UI
//! language and the asset types to archive. The desktop's other preferences
//! are desktop-only.

use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use shelfy_core::ingest::archive::{ArchivePolicy, Scope, refresh_states};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::settings::{self, SettingsChange};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::extract::Json;
use crate::ids::now_ms;
use crate::jobs::archive;
use crate::library;
use crate::routes::auth::no_store;
use crate::state::{AppState, blocking};

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_settings, put_settings))
}

/// A language of the app's interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Italian.
    It,
    /// English.
    En,
}

impl From<settings::Language> for Language {
    fn from(language: settings::Language) -> Self {
        match language {
            settings::Language::It => Self::It,
            settings::Language::En => Self::En,
        }
    }
}

impl From<Language> for settings::Language {
    fn from(language: Language) -> Self {
        match language {
            Language::It => Self::It,
            Language::En => Self::En,
        }
    }
}

/// Which assets of a post are archived: the desktop's "asset types to
/// download".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveAssetTypes {
    /// Covers and video posters.
    pub thumbnail: bool,
    /// Image slides.
    pub image: bool,
    /// Videos.
    pub video: bool,
}

impl From<settings::ArchiveAssetTypes> for ArchiveAssetTypes {
    fn from(types: settings::ArchiveAssetTypes) -> Self {
        Self {
            thumbnail: types.thumbnail,
            image: types.image,
            video: types.video,
        }
    }
}

impl From<ArchiveAssetTypes> for settings::ArchiveAssetTypes {
    fn from(types: ArchiveAssetTypes) -> Self {
        Self {
            thumbnail: types.thumbnail,
            image: types.image,
            video: types.video,
        }
    }
}

/// The account's settings, defaults filled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// The interface language; `null` until chosen, when the app follows
    /// the browser's.
    #[schema(required = true)]
    pub language: Option<Language>,
    /// The asset types to archive; all of them by default.
    pub archive_asset_types: ArchiveAssetTypes,
}

impl From<settings::Settings> for Settings {
    fn from(stored: settings::Settings) -> Self {
        Self {
            language: stored.language.map(Language::from),
            archive_asset_types: stored.archive_asset_types.into(),
        }
    }
}

/// Body of `PUT /api/v1/me/settings`: the settings to change; the others
/// keep their values. Other keys are refused (422).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SettingsUpdate {
    /// The interface language.
    #[serde(default)]
    #[schema(nullable = false)]
    pub language: Option<Language>,
    /// The asset types to archive, all three.
    #[serde(default)]
    #[schema(nullable = false)]
    pub archive_asset_types: Option<ArchiveAssetTypes>,
}

/// The account's settings.
#[utoipa::path(
    get,
    path = "/api/v1/me/settings",
    tag = "account",
    operation_id = "getSettings",
    responses(
        (status = OK, description = "The settings.", body = Settings),
    )
)]
pub async fn get_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let db = state.user_db(user.id()).await?;
    let stored = blocking(move || db.read(settings::read)).await?;
    Ok(no_store(Json(Settings::from(stored)).into_response()))
}

/// Changes some of the account's settings; answers all of them. A setting
/// that already has the value is not written again.
#[utoipa::path(
    put,
    path = "/api/v1/me/settings",
    tag = "account",
    operation_id = "updateSettings",
    request_body = SettingsUpdate,
    responses(
        (status = OK, description = "The settings after the change.", body = Settings),
    )
)]
pub async fn put_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(update): Json<SettingsUpdate>,
) -> Result<Response, ApiError> {
    let change = SettingsChange {
        language: update.language.map(Into::into),
        archive_asset_types: update.archive_asset_types.map(Into::into),
    };
    let db = state.user_db(user.id()).await?;
    let now = now_ms();
    let stored = if change == SettingsChange::default() {
        blocking(move || db.read(settings::read)).await?
    } else {
        // Other asset types change what the archive wants of every post
        // (P2-10): the states are derived again in the same transaction.
        let modes = archive::modes(&state);
        let (stored, refreshed) = blocking(move || {
            db.write(|tx| -> Result<_, RepoError> {
                let before = settings::read(tx)?.archive_asset_types;
                let stored = settings::update(tx, &change, now)?;
                let refreshed = if stored.archive_asset_types == before {
                    None
                } else {
                    let policy = ArchivePolicy {
                        modes,
                        assets: stored.archive_asset_types,
                    };
                    Some(refresh_states(tx, Scope::All, &policy, now)?)
                };
                Ok((stored, refreshed))
            })
        })
        .await?;
        if let Some(refreshed) = refreshed {
            if refreshed.changed > 0 {
                library::announce(state.events(), user.id(), ChangeReason::Archive, None);
            }
            // The sweeper re-arms it within 10 minutes should this fail.
            if refreshed.counts.server_work() > 0
                && let Err(err) = archive::enqueue(state.jobs(), user.id()).await
            {
                tracing::warn!(error = %err, "cannot enqueue the archive drain");
            }
        }
        stored
    };
    Ok(no_store(Json(Settings::from(stored)).into_response()))
}
