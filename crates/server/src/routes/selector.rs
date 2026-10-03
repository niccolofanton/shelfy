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
//! **Strict selectors, lenient queries.** A selector picks the posts a write
//! changes, so its `filter` refuses a member it does not know (422
//! `validation_failed` naming `selector.filter.<member>`): a misspelled
//! filter (`colection`) must not widen the selection to the whole library
//! (P1-03 review, M1). It also refuses a member that is null or blank
//! (`{"collection": null}`, `{"tag": ""}`, `{"tags": []}`), which would
//! filter nothing, and keys longer than any key can be (P1-11 review). Query
//! strings (`GET /posts`, `GET /posts/count`, `GET /search`) still ignore
//! unknown parameters and empty values, as HTTP tools and caches add their
//! own: there a typo only widens a view, never a change.
//!
//! The selector resolves into [`shelfy_core::selector::Selector`], which
//! compiles to one SQL condition. Used by `POST /collections/{id}/posts` and
//! `POST /collections/from-query` (P1-03), and `POST /posts/bulk` (P1-11).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::LazyLock;

use serde::de::{self, Deserializer, Unexpected, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use shelfy_core::selector::{MAX_EXCEPT_KEYS, MAX_KEYS, Selector};
use utoipa::{IntoParams, ToSchema};

use super::listing::{MatchMode, YesNo};
use super::model::{MediaType, Platform};
use super::posts::{MAX_KEY_BYTES, PostSource, PostsQuery};
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
    /// Only posts with this AI-detected language.
    pub ai_language: Option<String>,
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
            ai_language: f.ai_language,
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

    /// The members of a filter as JSON, `camelCase`: every field of the
    /// type, as its derived `Serialize` names them (it skips none).
    #[must_use]
    pub fn members() -> &'static BTreeSet<String> {
        static MEMBERS: LazyLock<BTreeSet<String>> =
            LazyLock::new(|| match serde_json::to_value(FilterParams::default()) {
                Ok(serde_json::Value::Object(members)) => members.keys().cloned().collect(),
                _ => unreachable!("a struct serializes to an object"),
            });
        &MEMBERS
    }
}

/// Most unknown members a refused selector filter names.
const MAX_UNKNOWN_NAMED: usize = 10;
/// Longest unknown member name repeated in a problem, in characters.
const MAX_UNKNOWN_CHARS: usize = 64;

/// The `filter` of a [`PostSelector`]: [`FilterParams`] as a JSON object,
/// plus the names of the members it does not know and of those that are
/// null or blank, which [`PostSelector::resolve`] refuses (see the module
/// docs). Serialized and documented as [`FilterParams`], without the
/// members that filter nothing (null, empty lists), so a job's payload stays
/// compact.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelectorFilter {
    params: FilterParams,
    unknown: Vec<String>,
    blank: Vec<String>,
}

impl SelectorFilter {
    /// A filter with these parameters and no unknown or blank member.
    #[must_use]
    pub fn new(params: FilterParams) -> Self {
        Self {
            params,
            unknown: Vec::new(),
            blank: Vec::new(),
        }
    }
}

impl From<FilterParams> for SelectorFilter {
    fn from(params: FilterParams) -> Self {
        Self::new(params)
    }
}

impl Serialize for SelectorFilter {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.params).map_err(serde::ser::Error::custom)?;
        if let serde_json::Value::Object(members) = &mut value {
            members.retain(|_, v| !(v.is_null() || v.as_array().is_some_and(Vec::is_empty)));
        }
        value.serialize(serializer)
    }
}

/// Whether a member's value filters nothing, so that a selection by it would
/// cover the whole library: null, a blank string, an empty list, or a list
/// with a null or blank item.
fn is_blank(value: &serde_json::Value) -> bool {
    let blank_text = |text: &str| {
        text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
            .is_empty()
    };
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(text) => blank_text(text),
        serde_json::Value::Array(items) => {
            items.is_empty()
                || items
                    .iter()
                    .any(|item| item.is_null() || item.as_str().is_some_and(blank_text))
        }
        _ => false,
    }
}

impl<'de> Deserialize<'de> for SelectorFilter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut members = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let known = FilterParams::members();
        let unknown: Vec<String> = members
            .keys()
            .filter(|name| !known.contains(name.as_str()))
            .cloned()
            .collect();
        members.retain(|name, _| known.contains(name.as_str()));
        let blank: Vec<String> = members
            .iter()
            .filter(|(_, value)| is_blank(value))
            .map(|(name, _)| name.clone())
            .collect();
        let params = FilterParams::deserialize(serde_json::Value::Object(members))
            .map_err(de::Error::custom)?;
        Ok(Self {
            params,
            unknown,
            blank,
        })
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
    /// These posts, by key, in the trash or not: at most 500, each at most
    /// 200 bytes. Unknown keys are skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub keys: Option<Vec<String>>,
    /// Every post `GET /posts` lists with these filters, over all its pages
    /// (`trash: true` selects in the trash). A member that is not a filter,
    /// or that is null or blank, is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<FilterParams>, nullable = false)]
    pub filter: Option<SelectorFilter>,
    /// With `filter`: posts to leave out, by key, at most 1,000 ("select all
    /// matching" minus the ones unticked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// exceptions, a key longer than 200 bytes, a filter member that is not
    /// a filter, a null or blank filter member, or an invalid filter value.
    pub fn resolve(self, field: &str) -> Result<Selector, ApiError> {
        let invalid =
            |name: &str, reason: String| ApiError::invalid_field(format!("{field}{name}"), reason);
        let too_long = |keys: &[String]| keys.iter().any(|key| key.len() > MAX_KEY_BYTES);
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
                if too_long(&keys) {
                    return Err(invalid(
                        ".keys",
                        format!("has a key longer than {MAX_KEY_BYTES} bytes"),
                    ));
                }
                Ok(Selector::Keys(keys))
            }
            (None, Some(filter)) => {
                if !filter.unknown.is_empty() {
                    return Err(unknown_members(field, &filter.unknown));
                }
                if !filter.blank.is_empty() {
                    return Err(blank_members(field, &filter.blank));
                }
                let except_keys = self.except_keys.unwrap_or_default();
                if except_keys.len() > MAX_EXCEPT_KEYS {
                    return Err(invalid(
                        ".exceptKeys",
                        format!("has more than {MAX_EXCEPT_KEYS} keys"),
                    ));
                }
                if too_long(&except_keys) {
                    return Err(invalid(
                        ".exceptKeys",
                        format!("has a key longer than {MAX_KEY_BYTES} bytes"),
                    ));
                }
                let query = filter.params.into_query();
                query
                    .validate()
                    .map_err(|err| nested(err, &format!("{field}.filter")))?;
                Ok(Selector::filter_except(query.filter(), except_keys))
            }
        }
    }
}

/// 422 `validation_failed` naming each member of `<field>.filter` that is
/// not a filter (at most [`MAX_UNKNOWN_NAMED`], names cut to
/// [`MAX_UNKNOWN_CHARS`] characters).
fn unknown_members(field: &str, unknown: &[String]) -> ApiError {
    unknown
        .iter()
        .take(MAX_UNKNOWN_NAMED)
        .map(|name| name.chars().take(MAX_UNKNOWN_CHARS).collect::<String>())
        .fold(
            ApiError::new(crate::error::ErrorCode::ValidationFailed),
            |out, name| out.with_field(format!("{field}.filter.{name}"), "is not a filter"),
        )
}

/// 422 `validation_failed` naming each member of `<field>.filter` that is
/// null or blank: it would filter nothing, so the selection would be the
/// whole library (P1-11 review). A query string still ignores such values.
fn blank_members(field: &str, blank: &[String]) -> ApiError {
    blank.iter().take(MAX_UNKNOWN_NAMED).fold(
        ApiError::new(crate::error::ErrorCode::ValidationFailed),
        |out, name| {
            out.with_field(
                format!("{field}.filter.{name}"),
                "is null or blank: leave it out to filter by nothing",
            )
        },
    )
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
        assert!(serde_json::from_value::<PostSelector>(json!({ "filter": [] })).is_err());
    }

    /// Review M1: a misspelled filter member must not widen the selection to
    /// the whole library.
    #[test]
    fn unknown_filter_members_are_named() {
        let err = parse(json!({ "filter": { "colection": 999 } })).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ValidationFailed);
        assert_eq!(field_of(&err), "selector.filter.colection");

        let err = parse(json!({
            "filter": { "platfrom": "pinterest", "platform": "pinterest", "x".repeat(100): 1 },
            "exceptKeys": ["ig_1"],
        }))
        .unwrap_err();
        let fields: Vec<String> = err.problem().errors.into_iter().map(|f| f.field).collect();
        assert_eq!(
            fields,
            [
                "selector.filter.platfrom".to_owned(),
                format!("selector.filter.{}", "x".repeat(MAX_UNKNOWN_CHARS)),
            ]
        );
        // A known member that is null or blank filters nothing: in a
        // selector that would select the whole library, so it is refused
        // (P1-11 review). A query string still ignores it.
        for (member, value) in [
            ("collection", json!(null)),
            ("tag", json!("")),
            ("q", json!("  ")),
            ("tags", json!([])),
            ("concept", json!(["lamp", " "])),
            ("trash", json!(null)),
        ] {
            let err = parse(json!({ "filter": { member: value } })).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ValidationFailed, "{member}");
            assert_eq!(field_of(&err), format!("selector.filter.{member}"));
        }
        let all = parse(json!({ "filter": { "trash": false } })).unwrap();
        assert_eq!(all, Selector::filter(Default::default()));
        let lenient: FilterParams = serde_html_form::from_str("collection=&tag=").unwrap();
        assert_eq!(lenient.tag.as_deref(), Some(""));
    }

    /// Keys longer than any key are refused; a filter serializes without
    /// the members that filter nothing (P1-11 review L5).
    #[test]
    fn selectors_stay_compact() {
        let long = "k".repeat(201);
        for (value, field) in [
            (json!({ "keys": [&long] }), "selector.keys"),
            (
                json!({ "filter": {}, "exceptKeys": ["ig_1", &long] }),
                "selector.exceptKeys",
            ),
        ] {
            let err = parse(value.clone()).unwrap_err();
            assert_eq!(field_of(&err), field, "{value}");
        }
        assert!(parse(json!({ "keys": ["k".repeat(200)] })).is_ok());
        let selector: PostSelector = serde_json::from_value(json!({
            "filter": { "platform": "instagram", "mediaType": ["video"] },
            "exceptKeys": ["ig_1"],
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&selector).unwrap(),
            json!({
                "filter": { "platform": "instagram", "mediaType": ["video"] },
                "exceptKeys": ["ig_1"],
            })
        );
    }

    #[test]
    fn filter_members_are_the_fields_of_filter_params() {
        let members = FilterParams::members();
        assert_eq!(members.len(), 18, "{members:?}");
        for name in [
            "collection",
            "mediaType",
            "aiTagged",
            "aiLanguage",
            "conceptMode",
            "trash",
        ] {
            assert!(members.contains(name), "{name}");
        }
        // The selector filter serializes as the parameters themselves.
        let filter = SelectorFilter::new(FilterParams {
            q: Some("lamp".into()),
            ..FilterParams::default()
        });
        assert_eq!(serde_json::to_value(&filter).unwrap()["q"], "lamp");
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
