//! `POST /api/v1/links` (plan §2.17; P2 contract C7; P2-11): a shared link
//! becomes a post at once.
//!
//! The Android share target and the bookmarklet reach it through the web
//! app's `/share` page (a session, through the CSRF guard); the iOS
//! Shortcut sends a `links:create` token ([`crate::routes::TOKEN_ROUTES`]).
//!
//! - **The link** ([`link::classify`]): an Instagram, X or Pinterest post
//!   URL names that post (`ig_<pk>`, `x_<id>`, `pin_<id>`); a `pin.it`
//!   short link is resolved first ([`short_link`]); any other http(s) URL
//!   is a web post (`web_<sha1:20>`). Hostile and unusable URLs answer 422
//!   `unsupported_link`; a short link that cannot be resolved now, 503
//!   `unavailable`.
//! - **A new key** inserts a placeholder in one write, with the note and the
//!   manual tags: a social post of media type `image` (P2-G12: the API's
//!   closed set has no "unknown", and the card shows the platform fallback
//!   while it has no media), `pending` until `link.hydrate` fills it in
//!   ([`crate::jobs::hydrate`], enqueued after the write); a web post from
//!   [`captures::placeholder`], `link_only` until P4 captures it (P2-G13:
//!   P4-14 enqueues `capture.site` here). 201, `created: true`.
//! - **A known key**: the manual tags are united with the stored ones
//!   (case-insensitively, stored first, at most 100), the note is set when
//!   the post has none and joined below it otherwise (§4.2's rule for two
//!   copies of a post), a trashed post comes back from the trash, and a
//!   social post still waiting for its media is hydrated again. 200,
//!   `created: false`.
//!
//! Both announce `posts.changed` (`ingest`, the key) and `stats.changed`,
//! also when the known post did not change.

use axum::extract::State;
use axum::http::StatusCode;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use shelfy_core::ids::link::{self, Link, LinkError, PostLink, WebLink};
use shelfy_core::ingest::archive::{self, ArchivePolicy, Scope};
use shelfy_core::repo::posts::{self, NewPost, UserContentPatch};
use shelfy_core::repo::{self, RepoError};
use shelfy_core::web::captures;
use utoipa::ToSchema;

use super::model::Platform;
use super::post_edit::{MAX_LABEL_BYTES, MAX_LIST_ITEMS, MAX_TEXT_BYTES};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::events::model::ChangeReason;
use crate::extract::Json;
use crate::ids::now_ms;
use crate::jobs::hydrate::{self, short_link};
use crate::library::{self, Change};
use crate::state::AppState;

/// The media type of a social post before its hydration (P2-G12).
pub const PLACEHOLDER_MEDIA_TYPE: &str = "image";

/// A link to save (contract C7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LinkCreate {
    /// The link: http or https, at most 4,096 characters. Instagram, X and
    /// Pinterest post links (and `pin.it` short links) name the post; any
    /// other site becomes a web post. Tracking parameters are dropped.
    pub url: String,
    /// A note for the post, at most 20,000 bytes of UTF-8. A known post
    /// keeps its note and gets this one below it.
    #[serde(default)]
    pub note: Option<String>,
    /// Manual tags, at most 100 of at most 200 bytes each, united with the
    /// post's own.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// The post a link names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkCreated {
    /// The post's key: `/p/{key}` opens it.
    pub key: String,
    /// Its platform.
    pub platform: Platform,
    /// Whether the link created the post; `false` when it was saved before.
    pub created: bool,
}

impl LinkCreate {
    /// The note and tags as stored: trimmed, blank ones dropped, tags
    /// deduplicated case-insensitively. Refuses over-long values (422).
    fn user_layer(&self) -> Result<(Option<String>, Vec<String>), ApiError> {
        let note = self
            .note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty());
        if note.is_some_and(|note| note.len() > MAX_TEXT_BYTES) {
            return Err(ApiError::invalid_field(
                "note",
                format!("longer than {MAX_TEXT_BYTES} bytes"),
            ));
        }
        let tags = self.tags.clone().unwrap_or_default();
        if tags.len() > MAX_LIST_ITEMS {
            return Err(ApiError::invalid_field(
                "tags",
                format!("more than {MAX_LIST_ITEMS} items"),
            ));
        }
        if tags.iter().any(|tag| tag.len() > MAX_LABEL_BYTES) {
            return Err(ApiError::invalid_field(
                "tags",
                format!("an item longer than {MAX_LABEL_BYTES} bytes"),
            ));
        }
        Ok((note.map(str::to_owned), unite(&[], &tags)))
    }
}

/// `stored`, then the `added` tags it lacks (trimmed, compared without
/// case), at most [`MAX_LIST_ITEMS`].
fn unite(stored: &[String], added: &[String]) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in stored.iter().chain(added) {
        let tag = tag.trim();
        let known = tags.iter().any(|t| t.to_lowercase() == tag.to_lowercase());
        if !tag.is_empty() && !known && tags.len() < MAX_LIST_ITEMS {
            tags.push(tag.to_owned());
        }
    }
    tags
}

/// A note joined below `stored` (§4.2), unless it is already there or the
/// result would be over [`MAX_TEXT_BYTES`].
fn join_note(stored: Option<&str>, added: &str) -> Option<String> {
    match stored.map(str::trim).filter(|s| !s.is_empty()) {
        None => Some(added.to_owned()),
        Some(stored) if stored.contains(added) => None,
        Some(stored) => {
            let joined = format!("{stored}\n\n{added}");
            (joined.len() <= MAX_TEXT_BYTES).then_some(joined)
        }
    }
}

fn unsupported(err: LinkError) -> ApiError {
    ApiError::new(ErrorCode::UnsupportedLink).with_detail(err.to_string())
}

/// What the write did.
struct Saved {
    created: bool,
    /// Enqueue `link.hydrate` for the post.
    hydrate: bool,
}

/// Saves a shared link as a post (contract C7): a new post, or the one the
/// link already names. Social posts are filled in in the background
/// (`link.hydrate`); web posts stay links until they are captured.
///
/// A signed-in session, or an API token with the `links:create` scope (the
/// iOS Shortcut) or `library:write` (an API client).
#[utoipa::path(
    post,
    path = "/api/v1/links",
    tag = "library",
    operation_id = "createLink",
    security(("session" = []), ("bearer" = ["links:create"]), ("bearer" = ["library:write"])),
    request_body = LinkCreate,
    responses(
        (status = CREATED, description = "The link created the post.", body = LinkCreated),
        (status = OK, description = "The link names a post saved before; its note and tags were added.", body = LinkCreated),
    )
)]
pub async fn create_link(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<LinkCreate>,
) -> Result<(StatusCode, Json<LinkCreated>), ApiError> {
    let (note, tags) = request.user_layer()?;
    let link = match link::classify(&request.url).map_err(unsupported)? {
        Link::PinterestShort(short) => match short_link::resolve(state.outbound(), &short).await {
            Ok(post) => Link::Post(post),
            Err(short_link::ShortLinkError::NotAPin) => {
                return Err(ApiError::new(ErrorCode::UnsupportedLink)
                    .with_detail("the short link does not lead to a pin"));
            }
            Err(short_link::ShortLinkError::Unavailable) => {
                return Err(ApiError::new(ErrorCode::Unavailable)
                    .with_detail("the short link cannot be resolved now"));
            }
        },
        other => other,
    };
    let now = now_ms();
    let (placeholder, social) = match &link {
        Link::Post(post) => (social_placeholder(post, now), true),
        Link::Web(web) => (web_placeholder(web, now)?, false),
        Link::PinterestShort(_) => unreachable!("resolved above"),
    };
    let key = placeholder.key.clone();
    let platform = placeholder.platform;
    let modes = hydrate::archive_modes(&state);
    let write_key = key.clone();
    let written = library::write(&state, user.id(), ChangeReason::Ingest, move |tx| {
        let saved = save(tx, placeholder, note, tags, social, modes, now)?;
        Ok(Change {
            value: saved,
            keys: Some(vec![write_key]),
        })
    })
    .await?;
    if !written.changed {
        // C7: a known link announces too, so open views show it saved.
        library::announce(
            state.events(),
            user.id(),
            ChangeReason::Ingest,
            Some(vec![key.clone()]),
        );
    }
    let saved = written.value;
    if saved.hydrate
        && let Err(err) = hydrate::enqueue(state.jobs(), user.id(), &key).await
    {
        // The post is saved; sharing the link again enqueues the hydration.
        tracing::warn!(error = %err, "cannot enqueue a link hydration");
    }
    // P2-G13 seam: a new web post stays `link_only` here; P4-14 enqueues its
    // `capture.site` job at this point (`!social && saved.created`).
    let status = if saved.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(LinkCreated {
            key,
            platform: platform.into(),
            created: saved.created,
        }),
    ))
}

/// A social post that waits for its hydration (P2-G12).
fn social_placeholder(post: &PostLink, now: i64) -> NewPost {
    let platform = match post.id.platform() {
        shelfy_core::ids::Platform::Instagram => repo::Platform::Instagram,
        shelfy_core::ids::Platform::Twitter => repo::Platform::Twitter,
        _ => repo::Platform::Pinterest,
    };
    let mut new = NewPost::new(
        post.id.key(),
        platform,
        post.id.native_id(),
        PLACEHOLDER_MEDIA_TYPE,
        now,
    );
    new.post_url = Some(post.url.clone());
    new.shortcode.clone_from(&post.shortcode);
    new
}

/// A web post that stays a link until P4 captures it (P2-G13).
fn web_placeholder(web: &WebLink, now: i64) -> Result<NewPost, ApiError> {
    let post =
        captures::placeholder(&web.url, now).map_err(|_| unsupported(LinkError::Malformed))?;
    debug_assert_eq!(post.key, web.id.key());
    Ok(post)
}

/// A known post's id, note, tags (JSON) and trash time.
type StoredUser = (i64, Option<String>, Option<String>, Option<i64>);

/// Inserts the placeholder, or adds the note and tags to the post it names.
fn save(
    tx: &Transaction<'_>,
    mut placeholder: NewPost,
    note: Option<String>,
    tags: Vec<String>,
    social: bool,
    modes: shelfy_core::ingest::archive::ArchiveModes,
    now: i64,
) -> Result<Saved, RepoError> {
    let stored: Option<StoredUser> = tx
        .prepare_cached(
            "SELECT id, user_note, user_tags_json, deleted_at FROM posts WHERE key = ?1",
        )?
        .query_row([&placeholder.key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((id, stored_note, stored_tags, deleted_at)) = stored else {
        placeholder.user_note = note;
        placeholder.user_tags = tags;
        let id = posts::insert(tx, &placeholder, now)?;
        let policy = ArchivePolicy::read(tx, modes)?;
        archive::refresh_states(tx, Scope::Posts(&[id]), &policy, now)?;
        return Ok(Saved {
            created: true,
            hydrate: social,
        });
    };
    let stored_tags: Vec<String> = stored_tags
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    let united = unite(&stored_tags, &tags);
    let patch = UserContentPatch {
        note: note
            .and_then(|note| join_note(stored_note.as_deref(), &note))
            .map(Some),
        tags: (united != stored_tags).then_some(united),
    };
    if patch != UserContentPatch::default() {
        posts::update_user_content(tx, id, &patch, now)?;
    }
    if deleted_at.is_some() {
        posts::restore(tx, &[id], now)?;
    }
    let hydrate = social && hydrate::waits_for_server(tx, &placeholder.key)?;
    if hydrate {
        // A post whose last hydration failed tries again.
        tx.prepare_cached(
            "UPDATE posts SET archive_state = 'pending' WHERE id = ?1 AND archive_state = 'failed'",
        )?
        .execute(params![id])?;
    }
    Ok(Saved {
        created: false,
        hydrate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_unite_without_case_and_notes_join() {
        let stored = vec!["Lighting".to_owned(), "wood".to_owned()];
        let added = vec![" lighting ".to_owned(), "Oak".to_owned(), String::new()];
        assert_eq!(unite(&stored, &added), ["Lighting", "wood", "Oak"]);
        let many: Vec<String> = (0..150).map(|n| format!("t{n}")).collect();
        assert_eq!(unite(&[], &many).len(), MAX_LIST_ITEMS);

        assert_eq!(join_note(None, "new").as_deref(), Some("new"));
        assert_eq!(join_note(Some("  "), "new").as_deref(), Some("new"));
        assert_eq!(join_note(Some("old"), "new").as_deref(), Some("old\n\nnew"));
        assert_eq!(join_note(Some("old new"), "new"), None);
        let long = "a".repeat(MAX_TEXT_BYTES);
        assert_eq!(join_note(Some(&long), "new"), None);
    }

    #[test]
    fn over_long_notes_and_tags_are_refused() {
        let request = |note: Option<String>, tags: Option<Vec<String>>| LinkCreate {
            url: "https://example.com/".to_owned(),
            note,
            tags,
        };
        let refused = |r: LinkCreate| r.user_layer().unwrap_err().code();
        assert_eq!(
            refused(request(Some("a".repeat(MAX_TEXT_BYTES + 1)), None)),
            ErrorCode::ValidationFailed
        );
        assert_eq!(
            refused(request(None, Some(vec!["a".repeat(MAX_LABEL_BYTES + 1)]))),
            ErrorCode::ValidationFailed
        );
        assert_eq!(
            refused(request(
                None,
                Some(vec!["a".to_owned(); MAX_LIST_ITEMS + 1])
            )),
            ErrorCode::ValidationFailed
        );
        let (note, tags) = request(Some("  hi ".to_owned()), Some(vec!["A".into(), "a".into()]))
            .user_layer()
            .unwrap();
        assert_eq!(note.as_deref(), Some("hi"));
        assert_eq!(tags, ["A"]);
        assert_eq!(
            request(Some("   ".to_owned()), None)
                .user_layer()
                .unwrap()
                .0,
            None
        );
    }
}
