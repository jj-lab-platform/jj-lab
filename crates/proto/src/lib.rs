//! Protobuf strong types generated from `jjlab.proto`. These are the single
//! source of truth for the domain model (Change / Conflict / Operation /
//! Bookmark); `server` serializes them for HTTP, and a future tonic service
//! layer reuses the same types.

pub mod jjlab {
    include!(concat!(env!("OUT_DIR"), "/jjlab.rs"));
}

pub use jjlab::*;