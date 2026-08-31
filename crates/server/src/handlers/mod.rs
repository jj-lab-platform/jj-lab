//! HTTP handler submodules, split from the monolith `lib.rs` by domain. Each
//! module re-imports the shared surface from the crate root via `super::*`
//! (`crate::*`), so the helpers/types (AppState, json_err, Level, ...) and the
//! handlers stay nameable in `build_router` with a single `use` glob.

pub mod actions;
pub mod explore;
pub mod git_http;
pub mod git_read;
pub mod metadata;
pub mod mirror;
pub mod mr;
pub mod ops;
pub mod read_ext;
pub mod releases;
pub mod write;

pub use actions::*;
pub use explore::*;
pub use git_http::*;
pub use git_read::*;
pub use metadata::*;
pub use mirror::*;
pub use mr::*;
pub use ops::*;
pub use read_ext::*;
pub use releases::*;
pub use write::*;