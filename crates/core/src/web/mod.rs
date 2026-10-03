//! Websites (plan §2.7, §2.18, §2.9 Websites): sites are `web` posts, and
//! each capture of a site is a version of it.
//!
//! - [`captures`]: the versions of a site, the post's mirror of its current
//!   version, the frozen AI layer of the older ones, and the two delete
//!   modes (P4-04).
//! - [`sites`]: the Websites listing, its facet filter and counts, and the
//!   colour filter and sort (P4-05).
//! - [`color`]: the OKLab colour math `sites` and `similar` share.
//! - [`similar`]: the facet-overlap "Similar" tab (P4-05).

pub mod captures;
pub mod color;
pub mod similar;
pub mod sites;

/// What a file of a version is: the `web_capture_assets.role` vocabulary
/// (`screenshot`, `hero`, `band`, `filmstrip`, `section`, `footer`, `og`,
/// `favicon`, `video`, `video_preview`, `video_poster`). The migration of
/// desktop libraries (P1-19) writes the same strings, so migrated and new
/// versions read alike.
pub use crate::legacy::web::WebAssetRole as AssetRole;
