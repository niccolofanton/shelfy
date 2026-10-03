//! Shelfy media: each user's content-addressed store (CAS), the image
//! pipeline and the video tools (plan D3, D4, §2.5, §2.13).
//!
//! | Module | Contents |
//! |---|---|
//! | [`digest`] | SHA-256 digests, the identity of every object |
//! | [`kind`] | the media type allowlist and magic-byte sniffing |
//! | [`name`] | object and rendition file names, the `variants` bitmask |
//! | [`store`] | the per-user store: streaming ingest, atomic writes, dedupe |
//! | [`refs`] | `media_objects` rows, references and reference counting |
//! | [`render`] | decode, resize, WebP renditions and ThumbHash |
//! | [`pool`] | the dedicated 2-thread pool the pipeline runs on |
//! | [`video`] | the yt-dlp and ffmpeg tools: on-demand videos, remux, posters, keyframes |
//!
//! The usual flow, for an image fetched by the archive worker (P2):
//!
//! ```no_run
//! # async fn archive(
//! #     media: &shelfy_media::store::UserMedia,
//! #     db: &shelfy_core::db::UserDb,
//! #     body: impl tokio::io::AsyncRead + Unpin,
//! #     now: i64,
//! # ) -> Result<(), Box<dyn std::error::Error>> {
//! use shelfy_media::name::Rendition;
//! use shelfy_media::pool::ImagePool;
//! use shelfy_media::refs::{self, ObjectMeta, Origin, Role};
//! use shelfy_media::store::IngestLimits;
//!
//! // Stream, hash, sniff and cap the download into a temporary file.
//! let staged = media.ingest_async(body, IngestLimits::ARCHIVE_IMAGE).await?;
//! // Decode it once on the image pool: g480 WebP + ThumbHash.
//! let spec = Rendition::G480.spec();
//! let rendered = ImagePool::shared().render_file(staged.path().to_owned(), spec).await?;
//! let meta = ObjectMeta {
//!     width: Some(rendered.source_width),
//!     height: Some(rendered.source_height),
//!     ..ObjectMeta::new(Role::Image, Origin::Server)
//! };
//! // Publish the files and record the row in one write transaction (see `store`).
//! let renditions = [(Rendition::G480, rendered.webp.as_slice())];
//! let id = db.write(|tx| {
//!     let (id, _) = refs::publish_and_record(tx, media, staged, &renditions, &meta, now)?;
//!     // … link `id` from `posts.cover_object` or `post_media.object_id` …
//!     refs::set_cover_thumbhash(tx, id, &rendered.thumbhash, now)?;
//!     Ok::<_, shelfy_core::repo::RepoError>(id)
//! })?;
//! # let _ = id;
//! # Ok(())
//! # }
//! ```

pub mod digest;
pub mod kind;
pub mod name;
pub mod pool;
pub mod refs;
pub mod render;
pub mod store;
pub mod video;

pub use digest::Digest;
pub use kind::MediaKind;
pub use name::{ObjectName, Rendition, Variants};
