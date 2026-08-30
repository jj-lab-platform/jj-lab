//! Error type shared by the substrate traits.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("artifact unknown")]
    ArtifactUnknown,
    #[error("blob unknown")]
    BlobUnknown,
    #[error("upload unknown")]
    UploadUnknown,
    #[error("digest mismatch: expected {expected}, got {got}")]
    DigestMismatch { expected: String, got: String },
    #[error("invalid digest: {0}")]
    InvalidDigest(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("db error: {0}")]
    Db(String),
    #[error("http error: {0}")]
    Http(String),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("upstream {path}: status {status}")]
    UpstreamStatus { path: String, status: u16 },
    #[error("no upstream for {0}")]
    NoUpstream(String),
    #[error("{0}")]
    Other(String),
}

impl RegistryError {
    /// True when this is a plain "not found" condition.
    pub fn is_unknown(&self) -> bool {
        matches!(
            self,
            RegistryError::ArtifactUnknown
                | RegistryError::BlobUnknown
                | RegistryError::UploadUnknown
        )
    }
}

pub type Result<T> = std::result::Result<T, RegistryError>;
