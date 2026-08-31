/// Unified error type for jj-lab. All crates in the workspace funnel domain
/// errors through this one so callers hold a single error path.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    Invalid(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("join error: {0}")]
    Join(String),
}

impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Self {
        Error::Join(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;