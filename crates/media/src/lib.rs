//! Shelfy media: each user's content-addressed store (CAS) and the image
//! pipeline (plan D3, D4, §2.5, §2.13).
//!
//! | Module | Contents |
//! |---|---|
//! | [`digest`] | SHA-256 digests, the identity of every object |
//! | [`kind`] | the media type allowlist and magic-byte sniffing |
//! | [`name`] | object and rendition file names, the `variants` bitmask |
//! | [`store`] | the per-user store: streaming ingest, atomic writes, dedupe |

pub mod digest;
pub mod kind;
pub mod name;
pub mod store;

pub use digest::Digest;
pub use kind::MediaKind;
pub use name::{ObjectName, Rendition, Variants};
