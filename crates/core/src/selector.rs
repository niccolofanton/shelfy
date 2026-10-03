//! Selections of posts (plan §2.9, "Bulk selector"): what an action on many
//! posts applies to, sent by the client instead of every id (DATA-17, UI-49).
//!
//! A [`Selector`] is one of
//!
//! - [`Selector::Keys`]: these posts, by key, in the trash or not;
//! - [`Selector::Filter`]: every post the gallery list returns for the filter
//!   (`GET /posts`, which hides the trash unless `trash` is set), minus the
//!   posts in `except_keys` ("select all matching" with deselections); or
//! - [`Selector::TrashedAt`]: the posts one delete moved to the trash, which
//!   all carry its time as `deleted_at` ([`crate::trash`]): the undo of that
//!   delete (P1-11).
//!
//! A selector compiles into a SQL condition on `posts p` ([`Selector::sql`]),
//! so an action runs as one statement whatever the selection's size:
//! `INSERT … SELECT p.id FROM posts p WHERE <condition>`, `UPDATE posts SET …
//! WHERE id IN (SELECT p.id FROM posts p WHERE <condition>)`. [`count`] and
//! [`ids`] answer how many and which (newest first, the gallery's order). An
//! action that runs in chunks goes through the selection in id order with
//! [`crate::bulk::next_chunk`].
//!
//! **The filter is the list's.** The condition of a filter is built by the
//! same code as the list and its count ([`posts::count`]), so a selection by
//! filter is exactly what the gallery shows with that filter, every page of
//! it: search text matches without the 1,000-result cap of relevance
//! paging.
//!
//! **Caps.** At most [`MAX_KEYS`] keys and [`MAX_EXCEPT_KEYS`] exceptions;
//! [`Selector::validate`] refuses more. Unknown keys are skipped. Each action
//! decides what it does with the posts it is given (adding to a collection
//! skips trashed ones, for example).
//!
//! The selection is resolved inside the action's transaction, so it sees the
//! same snapshot as the write. Used by the collection routes (P1-03) and the
//! bulk and trash routes (P1-11).

use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::repo::posts::{self, PostFilter};
use crate::repo::{RepoError, Result};

/// Most keys a [`Selector::Keys`] takes: one inline batch of a bulk action
/// (plan §2.9: up to 500 posts run inline).
pub const MAX_KEYS: usize = 500;
/// Most exceptions a [`Selector::Filter`] takes.
pub const MAX_EXCEPT_KEYS: usize = 1_000;

/// Which posts an action applies to.
#[derive(Clone, Debug, PartialEq)]
pub enum Selector {
    /// These posts, by key, in the trash or not. Unknown keys are skipped.
    Keys(Vec<String>),
    /// Every post the list returns for `filter`, except these keys.
    Filter {
        /// The list filter.
        filter: Box<PostFilter>,
        /// Posts left out of the selection.
        except_keys: Vec<String>,
    },
    /// The posts in the trash whose `deleted_at` is this time (unix ms): the
    /// posts one delete moved there ([`crate::trash`]).
    TrashedAt(i64),
}

/// A selector as SQL: a condition on the table alias `p` of `posts`, and
/// its parameters in order.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectorSql {
    /// The condition, for `… FROM posts p WHERE <condition>`.
    pub condition: String,
    /// The values of its `?` placeholders, in order.
    pub params: Vec<Value>,
}

impl Selector {
    /// These posts.
    #[must_use]
    pub fn keys(keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Keys(keys.into_iter().map(Into::into).collect())
    }

    /// Every post `filter` lists.
    #[must_use]
    pub fn filter(filter: PostFilter) -> Self {
        Self::filter_except(filter, Vec::new())
    }

    /// Every post `filter` lists, except `except_keys`.
    #[must_use]
    pub fn filter_except(filter: PostFilter, except_keys: Vec<String>) -> Self {
        Self::Filter {
            filter: Box::new(filter),
            except_keys,
        }
    }

    /// Refuses a selector over its caps.
    ///
    /// # Errors
    ///
    /// [`RepoError::Invalid`] naming `keys` (more than [`MAX_KEYS`]) or
    /// `exceptKeys` (more than [`MAX_EXCEPT_KEYS`]).
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Keys(keys) if keys.len() > MAX_KEYS => Err(RepoError::Invalid {
                field: "keys",
                reason: "has more than 500 keys",
            }),
            Self::Filter { except_keys, .. } if except_keys.len() > MAX_EXCEPT_KEYS => {
                Err(RepoError::Invalid {
                    field: "exceptKeys",
                    reason: "has more than 1000 keys",
                })
            }
            _ => Ok(()),
        }
    }

    /// The condition selecting the posts, after [`Selector::validate`].
    ///
    /// # Errors
    ///
    /// The selector is over its caps, or alias lookup fails.
    pub fn sql(&self, conn: &Connection) -> Result<SelectorSql> {
        self.validate()?;
        let keys_json =
            |keys: &[String]| Value::Text(serde_json::to_string(keys).expect("strings serialize"));
        Ok(match self {
            Self::Keys(keys) => SelectorSql {
                condition: "p.key IN (SELECT value FROM json_each(?))".to_owned(),
                params: vec![keys_json(keys)],
            },
            Self::Filter {
                filter,
                except_keys,
            } => {
                let filter = posts::tag_filter(conn, filter)?;
                let (mut condition, mut params) = posts::filter_condition(&filter);
                if !except_keys.is_empty() {
                    condition.push_str(" AND p.key NOT IN (SELECT value FROM json_each(?))");
                    params.push(keys_json(except_keys));
                }
                SelectorSql { condition, params }
            }
            // Written literally so the partial index `posts_trash` applies.
            Self::TrashedAt(at) => SelectorSql {
                condition: "p.deleted_at IS NOT NULL AND p.deleted_at = ?".to_owned(),
                params: vec![Value::Integer(*at)],
            },
        })
    }
}

/// How many posts `selector` selects.
///
/// # Errors
///
/// [`RepoError::Invalid`] over the caps; database errors.
pub fn count(conn: &Connection, selector: &Selector) -> Result<u64> {
    let sql = selector.sql(conn)?;
    let n: i64 = conn
        .prepare_cached(&format!(
            "SELECT count(*) FROM posts p WHERE {}",
            sql.condition
        ))?
        .query_row(params_from_iter(sql.params.iter()), |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Internal ids of the posts `selector` selects, newest first (the gallery's
/// default order).
///
/// # Errors
///
/// [`RepoError::Invalid`] over the caps; database errors.
pub fn ids(conn: &Connection, selector: &Selector) -> Result<Vec<i64>> {
    let sql = selector.sql(conn)?;
    let ids = conn
        .prepare_cached(&format!(
            "SELECT p.id FROM posts p WHERE {} ORDER BY p.sort_ts DESC, p.id DESC",
            sql.condition
        ))?
        .query_map(params_from_iter(sql.params.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_are_enforced() {
        let keys = |n: usize| (0..n).map(|i| format!("ig_{i}")).collect::<Vec<_>>();
        assert!(Selector::Keys(keys(MAX_KEYS)).validate().is_ok());
        assert!(matches!(
            Selector::Keys(keys(MAX_KEYS + 1)).validate(),
            Err(RepoError::Invalid { field: "keys", .. })
        ));
        let filter = |n: usize| Selector::filter_except(PostFilter::default(), keys(n));
        assert!(filter(MAX_EXCEPT_KEYS).validate().is_ok());
        assert!(matches!(
            filter(MAX_EXCEPT_KEYS + 1).sql(&Connection::open_in_memory().unwrap()),
            Err(RepoError::Invalid {
                field: "exceptKeys",
                ..
            })
        ));
    }

    #[test]
    fn the_reasons_name_the_caps() {
        let Err(RepoError::Invalid { reason, .. }) =
            Selector::Keys(vec![String::new(); 501]).validate()
        else {
            panic!("over the cap");
        };
        assert!(reason.contains(&MAX_KEYS.to_string()));
        let over = Selector::filter_except(
            PostFilter::default(),
            vec![String::new(); MAX_EXCEPT_KEYS + 1],
        );
        let Err(RepoError::Invalid { reason, .. }) = over.validate() else {
            panic!("over the cap");
        };
        assert!(reason.contains(&MAX_EXCEPT_KEYS.to_string()));
    }
}
