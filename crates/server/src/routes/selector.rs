//! What the routes that act on many posts take (plan §2.9, "Bulk
//! selector"): a [`PostSelector`], `{"keys": [...]}` or `{"filter": {...},
//! "exceptKeys": [...]}`, and its [`FilterParams`].
//!
//! [`FilterParams`] are the filters of `GET /api/v1/posts` without paging or
//! order, under the same names: the query of `GET /posts/count`, and, as
//! JSON, the `filter` of a selector. Both go through the list's own
//! conversion ([`PostsQuery`]: validation, then the core filter), so a count
//! or a selection always covers exactly the posts the list shows with the
//! same filters, every page of them. A test pins the three parameter sets to
//! each other.
//!
//! The selector resolves into [`shelfy_core::selector::Selector`], which
//! compiles to one SQL condition. Used by `POST /collections/{id}/posts` and
//! `POST /collections/from-query` (P1-03), and `POST /posts/bulk` (P1-11).

use std::fmt;

use serde::de::{self, Deserializer, Unexpected, Visitor};
use serde::{Deserialize, Serialize};
use shelfy_core::selector::{MAX_EXCEPT_KEYS, MAX_KEYS, Selector};
use utoipa::{IntoParams, ToSchema};

use super::listing::{MatchMode, YesNo};
use super::model::{MediaType, Platform};
use super::posts::{PostSource, PostsQuery};
use crate::error::ApiError;

/// The filters of `GET /api/v1/posts`, without paging or order. Every
/// filter is optional; the ones given combine with AND.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
#[serde(default, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct FilterParams {
    /// Only posts of this platform.
    pub platform: Option<Platform>,
    /// Only websites (`web`) or only everything else (`social`).
    pub source: Option<PostSource>,
    /// Only posts in this collection (its `id`).
    pub collection: Option<i64>,
    /// Only posts of these kinds: any of them. Repeatable in a query.
    #[param(style = Form, explode)]
    pub media_type: Vec<MediaType>,
    /// Only posts with (`yes`) or without (`no`) a stored object.
    pub stored: Option<YesNo>,
    /// Only posts with (`yes`) or without (`no`) AI tags; manual tags do not
    /// count.
    pub ai_tagged: Option<YesNo>,
    /// Only posts with this AI status.
    pub ai_status: Option<String>,
    /// Only posts with this tag (the gallery's tag chip; always a filter).
    pub tag: Option<String>,
    /// Tags of the AI views, combined by `tagMode`. Repeatable in a query.
    /// With `q` in `or` mode they widen the search.
    #[param(style = Form, explode)]
    pub tags: Vec<String>,
    /// How `tags` combine. Default `or`.
    pub tag_mode: Option<MatchMode>,
    /// Only posts with this AI entity.
    pub entity: Option<String>,
    /// Only posts with this AI category.
    pub category: Option<String>,
    /// Only posts with this AI content type.
    pub content_type: Option<String>,
    /// Free-text search (plan §2.14). At most 500 characters.
    pub q: Option<String>,
    /// Suggested concepts: more search terms, combined with `q` by
    /// `conceptMode`. Repeatable in a query.
    #[param(style = Form, explode)]
    pub concept: Vec<String>,
    /// How `q` and the concepts combine. Default `or`.
    pub concept_mode: Option<MatchMode>,
    /// The trash instead of the library: `true` (or `1` in a query).
    #[serde(deserialize_with = "flag_or_bool")]
    pub trash: Option<bool>,
}

impl From<FilterParams> for PostsQuery {
    fn from(f: FilterParams) -> Self {
        Self {
            platform: f.platform,
            source: f.source,
            collection: f.collection,
            media_type: f.media_type,
            stored: f.stored,
            ai_tagged: f.ai_tagged,
            ai_status: f.ai_status,
            tag: f.tag,
            tags: f.tags,
            tag_mode: f.tag_mode,
            entity: f.entity,
            category: f.category,
            content_type: f.content_type,
            q: f.q,
            concept: f.concept,
            concept_mode: f.concept_mode,
            trash: f.trash,
            sort: None,
            limit: None,
            cursor: None,
            include_total: None,
        }
    }
}

impl FilterParams {
    /// These filters as the list's query: no order, no paging.
    #[must_use]
    pub fn into_query(self) -> PostsQuery {
        self.into()
    }
}

/// A boolean, as JSON (`true`) or as a query value (`true`, `1`, `false`,
/// `0`; empty for none).
fn flag_or_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    struct Flag;
    impl<'de> Visitor<'de> for Flag {
        type Value = Option<bool>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("true, false, 1 or 0")
        }

        fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
            Ok(Some(value))
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            match value {
                "" => Ok(None),
                "true" | "1" => Ok(Some(true)),
                "false" | "0" => Ok(Some(false)),
                other => Err(E::invalid_value(Unexpected::Str(other), &self)),
            }
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(Flag)
}

/// Which posts an action applies to: exactly one of `keys` and `filter`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PostSelector {
    /// These posts, by key, in the trash or not: at most 500. Unknown keys
    /// are skipped.
    #[schema(nullable = false)]
    pub keys: Option<Vec<String>>,
    /// Every post `GET /posts` lists with these filters, over all its pages
    /// (`trash: true` selects in the trash).
    #[schema(nullable = false)]
    pub filter: Option<FilterParams>,
    /// With `filter`: posts to leave out, by key, at most 1,000 ("select all
    /// matching" minus the ones unticked).
    #[schema(nullable = false)]
    pub except_keys: Option<Vec<String>>,
}

impl PostSelector {
    /// The core selector. `field` names the selector in the request (its
    /// problems name `<field>.keys`, `<field>.filter.q`, …).
    ///
    /// # Errors
    ///
    /// 422 `validation_failed`: neither or both of `keys` and `filter`,
    /// `exceptKeys` without `filter`, more than 500 keys or 1,000
    /// exceptions, or an invalid filter value.
    pub fn resolve(self, field: &str) -> Result<Selector, ApiError> {
        let invalid =
            |name: &str, reason: String| ApiError::invalid_field(format!("{field}{name}"), reason);
        match (self.keys, self.filter) {
            (Some(_), Some(_)) => Err(invalid("", "takes `keys` or `filter`, not both".into())),
            (None, None) => Err(invalid("", "needs `keys` or `filter`".into())),
            (Some(keys), None) => {
                if self.except_keys.is_some() {
                    return Err(invalid(".exceptKeys", "goes with `filter` only".into()));
                }
                if keys.len() > MAX_KEYS {
                    return Err(invalid(".keys", format!("has more than {MAX_KEYS} keys")));
                }
                Ok(Selector::Keys(keys))
            }
            (None, Some(filter)) => {
                let except_keys = self.except_keys.unwrap_or_default();
                if except_keys.len() > MAX_EXCEPT_KEYS {
                    return Err(invalid(
                        ".exceptKeys",
                        format!("has more than {MAX_EXCEPT_KEYS} keys"),
                    ));
                }
                let query = filter.into_query();
                query
                    .validate()
                    .map_err(|err| nested(err, &format!("{field}.filter")))?;
                Ok(Selector::filter_except(query.filter(), except_keys))
            }
        }
    }
}

/// `err` with its fields under `prefix`.
fn nested(err: ApiError, prefix: &str) -> ApiError {
    let problem = err.problem();
    problem
        .errors
        .into_iter()
        .fold(ApiError::new(err.code()), |out, f| {
            out.with_field(format!("{prefix}.{}", f.field), f.reason)
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::error::ErrorCode;

    fn parse(value: serde_json::Value) -> Result<Selector, ApiError> {
        serde_json::from_value::<PostSelector>(value)
            .expect("well-formed JSON")
            .resolve("selector")
    }

    fn field_of(err: &ApiError) -> String {
        err.problem().errors[0].field.clone()
    }

    #[test]
    fn a_selector_is_keys_or_a_filter() {
        let keys = parse(json!({ "keys": ["ig_1", "x_2"] })).unwrap();
        assert_eq!(keys, Selector::keys(["ig_1", "x_2"]));
        let all = parse(json!({ "filter": {} })).unwrap();
        assert_eq!(all, Selector::filter(Default::default()));
        let some = parse(json!({
            "filter": { "platform": "instagram", "mediaType": ["video", "image"], "trash": true },
            "exceptKeys": ["ig_1"],
        }))
        .unwrap();
        let Selector::Filter {
            filter,
            except_keys,
        } = some
        else {
            panic!("a filter");
        };
        assert_eq!(except_keys, ["ig_1"]);
        assert!(filter.trash);
        assert_eq!(filter.media_types, ["video", "image"]);
    }

    #[test]
    fn malformed_selectors_name_their_field() {
        let cases = [
            (json!({}), "selector"),
            (json!({ "keys": [], "filter": {} }), "selector"),
            (
                json!({ "keys": [], "exceptKeys": [] }),
                "selector.exceptKeys",
            ),
            (json!({ "keys": vec!["k"; MAX_KEYS + 1] }), "selector.keys"),
            (
                json!({ "filter": {}, "exceptKeys": vec!["k"; MAX_EXCEPT_KEYS + 1] }),
                "selector.exceptKeys",
            ),
            (
                json!({ "filter": { "q": "q".repeat(501) } }),
                "selector.filter.q",
            ),
            (
                json!({ "filter": { "tags": vec!["t"; 51] } }),
                "selector.filter.tags",
            ),
        ];
        for (value, field) in cases {
            let err = parse(value.clone()).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ValidationFailed, "{value}");
            assert_eq!(field_of(&err), field, "{value}");
        }
    }

    #[test]
    fn unknown_fields_and_bad_values_are_refused() {
        assert!(serde_json::from_value::<PostSelector>(json!({ "key": [] })).is_err());
        assert!(
            serde_json::from_value::<PostSelector>(json!({ "filter": { "trash": "yes" } }))
                .is_err()
        );
        assert!(
            serde_json::from_value::<PostSelector>(json!({ "filter": { "platform": "tiktok" } }))
                .is_err()
        );
    }

    #[test]
    fn the_trash_flag_reads_json_and_query_forms() {
        for (value, trash) in [
            (json!({ "trash": true }), Some(true)),
            (json!({ "trash": false }), Some(false)),
            (json!({ "trash": null }), None),
            (json!({ "trash": "1" }), Some(true)),
            (json!({}), None),
        ] {
            let params: FilterParams = serde_json::from_value(value).unwrap();
            assert_eq!(params.trash, trash);
        }
        for (query, trash) in [
            ("trash=1", Some(true)),
            ("trash=false", Some(false)),
            ("trash=", None),
        ] {
            let params: FilterParams = serde_html_form::from_str(query).unwrap();
            assert_eq!(params.trash, trash, "{query}");
        }
        assert!(serde_html_form::from_str::<FilterParams>("trash=maybe").is_err());
        let params: FilterParams =
            serde_html_form::from_str("mediaType=video&mediaType=image&concept=a").unwrap();
        assert_eq!(params.media_type, [MediaType::Video, MediaType::Image]);
        assert_eq!(params.concept, ["a"]);
    }
}
