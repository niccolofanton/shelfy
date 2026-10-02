//! `PATCH /api/v1/posts/{key}`: the user's edits of one post (plan §2.9;
//! desktop `post:updateUserContent` and `analyze:updateManual`).
//!
//! The body is a partial post, under the names the post has in responses:
//!
//! - `userNote` and `userTags`, the user layer. The note is stored as given;
//!   the tags replace the old ones, stored as given for display, and their
//!   tag rows are trimmed, lowercased, alias-resolved and deduped
//!   (`update_user_content`).
//! - `aiDescription`, `aiTags`, `aiSaveReason`, `aiCategory`,
//!   `aiContentType`, `aiLanguage`, `aiEntities` and `aiKeywords`: a manual
//!   edit of the AI layer. Any of them makes the post's AI status `done` and
//!   its model `manual`, and stamps `aiAnalyzedAt` (the desktop's
//!   `updateAiAnalysis` with status `done`); the fields not sent are kept.
//!   The layer is the user's from then on: `aiProvider`, `aiError` and
//!   `aiSchemaVersion` are cleared, since the user's text follows no
//!   model's output schema.
//!
//! Absent fields are left alone; `null` clears a field (`userTags: null`
//! like `[]`). An AI tag and a manual tag of the same name are two tags
//! (plan §1.2 #3). The search index is rewritten in the same transaction.
//! The answer is the post after the edit; an edit that changed something
//! announces `posts.changed` (`edit`, the key) and `stats.changed`.

use axum::extract::State;
use serde::{Deserialize, Deserializer, Serialize};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::posts::{self, AiPatch, UserContentPatch};
use utoipa::ToSchema;

use super::model::PostDetail;
use super::posts::MAX_KEY_BYTES;
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::{Json, Path};
use crate::ids::now_ms;
use crate::library;
use crate::state::AppState;

/// Longest note, description or save reason, in characters (like a caption).
pub const MAX_TEXT_CHARS: usize = 20_000;
/// Longest tag, entity, keyword, category, content type or language, in
/// characters.
pub const MAX_LABEL_CHARS: usize = 200;
/// Most items of a tag, entity or keyword list.
pub const MAX_LIST_ITEMS: usize = 100;

/// Changes to a post. Every field is optional: absent fields are left
/// alone, and `null` clears one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PostPatch {
    /// The user's note, at most 20,000 characters, stored as given.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub user_note: Option<Option<String>>,
    /// The user's tags, replacing the old ones: at most 100, of at most 200
    /// characters. `null` clears them, like `[]`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<Vec<String>>)]
    pub user_tags: Option<Option<Vec<String>>>,
    /// Manual AI edit: the description, at most 20,000 characters.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub ai_description: Option<Option<String>>,
    /// Manual AI edit: the AI tags, replacing the old ones (at most 100, of
    /// at most 200 characters).
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<Vec<String>>)]
    pub ai_tags: Option<Option<Vec<String>>>,
    /// Manual AI edit: why the post was saved, at most 20,000 characters.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub ai_save_reason: Option<Option<String>>,
    /// Manual AI edit: the category, at most 200 characters.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub ai_category: Option<Option<String>>,
    /// Manual AI edit: the content type, at most 200 characters.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub ai_content_type: Option<Option<String>>,
    /// Manual AI edit: the language, at most 200 characters.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub ai_language: Option<Option<String>>,
    /// Manual AI edit: the entities (at most 100, of at most 200 characters).
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<Vec<String>>)]
    pub ai_entities: Option<Option<Vec<String>>>,
    /// Manual AI edit: the keywords (at most 100, of at most 200 characters).
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<Vec<String>>)]
    pub ai_keywords: Option<Option<Vec<String>>>,
}

/// A field that is present, `null` included (`Some(None)`).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}

impl PostPatch {
    /// Refuses over-long text and lists (422 `validation_failed`).
    fn validate(&self) -> Result<(), ApiError> {
        let texts = [
            ("userNote", &self.user_note, MAX_TEXT_CHARS),
            ("aiDescription", &self.ai_description, MAX_TEXT_CHARS),
            ("aiSaveReason", &self.ai_save_reason, MAX_TEXT_CHARS),
            ("aiCategory", &self.ai_category, MAX_LABEL_CHARS),
            ("aiContentType", &self.ai_content_type, MAX_LABEL_CHARS),
            ("aiLanguage", &self.ai_language, MAX_LABEL_CHARS),
        ];
        for (field, value, max) in texts {
            if let Some(Some(text)) = value
                && text.chars().count() > max
            {
                return Err(ApiError::invalid_field(
                    field,
                    format!("longer than {max} characters"),
                ));
            }
        }
        let lists = [
            ("userTags", &self.user_tags),
            ("aiTags", &self.ai_tags),
            ("aiEntities", &self.ai_entities),
            ("aiKeywords", &self.ai_keywords),
        ];
        for (field, value) in lists {
            let Some(Some(items)) = value else {
                continue;
            };
            if items.len() > MAX_LIST_ITEMS {
                return Err(ApiError::invalid_field(
                    field,
                    format!("more than {MAX_LIST_ITEMS} items"),
                ));
            }
            if items
                .iter()
                .any(|item| item.chars().count() > MAX_LABEL_CHARS)
            {
                return Err(ApiError::invalid_field(
                    field,
                    format!("an item longer than {MAX_LABEL_CHARS} characters"),
                ));
            }
        }
        Ok(())
    }

    /// The change to the user layer.
    fn user(&self) -> UserContentPatch {
        UserContentPatch {
            note: self.user_note.clone(),
            tags: self.user_tags.clone().map(Option::unwrap_or_default),
        }
    }

    /// The manual AI edit, if any AI field is present.
    fn ai(&self) -> Option<AiPatch> {
        let patch = AiPatch {
            description: self.ai_description.clone(),
            tags: self.ai_tags.clone(),
            save_reason: self.ai_save_reason.clone(),
            category: self.ai_category.clone(),
            content_type: self.ai_content_type.clone(),
            language: self.ai_language.clone(),
            entities: self.ai_entities.clone(),
            keywords: self.ai_keywords.clone(),
            ..AiPatch::default()
        };
        (patch != AiPatch::default()).then(|| patch.manual())
    }
}

/// Edits a post's note and tags, or its AI fields (a manual edit: the AI
/// status becomes `done` and the model `manual`, and the provider, error
/// and schema version are cleared). Trashed posts can be edited too.
#[utoipa::path(
    patch,
    path = "/api/v1/posts/{key}",
    tag = "library",
    operation_id = "updatePost",
    params(
        ("key" = String, Path, description = "The post's key, for example `ig_3141592653589793238`."),
    ),
    request_body = PostPatch,
    responses(
        (status = OK, description = "The post after the edit.", body = PostDetail),
    )
)]
pub async fn update_post(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(key): Path<String>,
    Json(patch): Json<PostPatch>,
) -> Result<Json<PostDetail>, ApiError> {
    if key.len() > MAX_KEY_BYTES {
        return Err(ApiError::not_found());
    }
    patch.validate()?;
    let (user_patch, ai_patch) = (patch.user(), patch.ai());
    let now = now_ms();
    let lookup = key.clone();
    let written = library::write(&state, user.id(), move |tx| {
        let id = posts::id_for_key(tx, &lookup)?.ok_or(RepoError::NotFound)?;
        posts::update_user_content(tx, id, &user_patch, now)?;
        if let Some(ai) = &ai_patch {
            posts::update_ai(tx, id, ai, now)?;
        }
        posts::get(tx, &lookup)?.ok_or(RepoError::NotFound)
    })
    .await?;
    if written.changed {
        library::announce(&state, user.id(), Some(vec![key]));
    }
    Ok(Json(PostDetail::from(written.value)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::error::ErrorCode;

    fn patch(value: serde_json::Value) -> PostPatch {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn absent_null_and_values_differ() {
        let empty = patch(json!({}));
        assert_eq!(empty, PostPatch::default());
        assert_eq!(empty.user(), UserContentPatch::default());
        assert_eq!(empty.ai(), None);

        let user = patch(json!({ "userNote": null, "userTags": null })).user();
        assert_eq!(user.note, Some(None));
        assert_eq!(user.tags, Some(Vec::new()), "null tags clear like []");

        let ai = patch(json!({ "aiDescription": "A lamp", "aiTags": null }))
            .ai()
            .unwrap();
        assert_eq!(ai.description, Some(Some("A lamp".into())));
        assert_eq!(ai.tags, Some(None));
        assert_eq!(ai.status, Some(Some("done".into())));
        assert_eq!(ai.model, Some(Some(posts::MANUAL_AI_MODEL.into())));
        assert_eq!(
            (ai.provider, ai.schema_version, ai.error),
            (Some(None), Some(None), Some(None)),
            "the layer is the user's"
        );
        assert_eq!(ai.category, None, "absent fields are kept");
    }

    #[test]
    fn unknown_fields_and_over_long_values_are_refused() {
        assert!(serde_json::from_value::<PostPatch>(json!({ "note": "x" })).is_err());
        assert!(serde_json::from_value::<PostPatch>(json!({ "aiStatus": "done" })).is_err());
        let cases = [
            (
                json!({ "userNote": "n".repeat(MAX_TEXT_CHARS + 1) }),
                "userNote",
            ),
            (
                json!({ "aiCategory": "c".repeat(MAX_LABEL_CHARS + 1) }),
                "aiCategory",
            ),
            (
                json!({ "userTags": vec!["t"; MAX_LIST_ITEMS + 1] }),
                "userTags",
            ),
            (
                json!({ "aiKeywords": ["k".repeat(MAX_LABEL_CHARS + 1)] }),
                "aiKeywords",
            ),
        ];
        for (value, field) in cases {
            let err = patch(value).validate().unwrap_err();
            assert_eq!(err.code(), ErrorCode::ValidationFailed);
            assert_eq!(err.problem().errors[0].field, field);
        }
        let fine = patch(json!({
            "userNote": "é".repeat(MAX_TEXT_CHARS),
            "userTags": vec!["t"; MAX_LIST_ITEMS],
        }));
        assert!(fine.validate().is_ok());
    }
}
