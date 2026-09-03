//! pkglab-common — the only crate shared by protocol crates.
//!
//! It contains:
//! - the neutral artifact model ([`Artifact`], [`Descriptor`], [`Hashes`]),
//! - storage traits ([`ArtifactStore`], [`BlobStore`], [`Registry`]),
//! - the auth trait ([`Auth`]),
//! - upstream access mechanics (proxy-aware client factory, TTL index cache,
//!   generic pull-through),
//! - small utilities (multipart field extraction, glob matching).
//!
//! Protocol crates depend on this crate only; they never depend on each other
//! or on the SQLite-backed reference implementation in `pkglab-core`.

pub mod artifact;
pub mod auth;
pub mod blob;
pub mod cache;
pub mod globutil;
pub mod httphelpers;
pub mod multipart;
pub mod pullthrough;
pub mod registry;
pub mod remote;
pub mod store;
pub mod upstreams;
pub mod versioncmp;

pub use artifact::{Artifact, Descriptor, Hashes, ReleaseMeta, FORMAT_RELEASE};
pub use auth::{Action, Auth};
pub use blob::{BlobStore, UploadRecord};
pub use pullthrough::Registry;
pub use registry::RegistryError;
pub use store::{ArtifactStore, PackageSummary};
