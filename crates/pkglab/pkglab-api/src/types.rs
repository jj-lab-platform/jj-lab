//! Neutral request/response types shared by the facade and the per-protocol
//! wrappers.

/// A single file carried in a publish request.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FileSpec {
    /// Logical filename (e.g. `pkg-1.0.0.tgz`, `foo-1.0.whl`). May be empty
    /// for formats whose blob name is derivable from name+version.
    #[serde(default)]
    pub name: String,
    /// Raw content bytes.
    #[serde(default)]
    pub data: Vec<u8>,
    /// Optional media type of this file.
    #[serde(default)]
    pub media_type: String,
}

/// A protocol-agnostic publish request.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PublishRequest {
    /// Protocol tag: `npm`, `pypi`, `cargo`, `go`, `maven`, `composer`,
    /// `nuget`, `rubygems`, `hex`, `pub`, `swift`, `conan`, `helm`,
    /// `generic`, `oci`.
    pub format: String,
    /// Fully-qualified name/coordinates (npm `@scope/pkg`, maven artifactId,
    /// swift `scope.name`, generic name, ...).
    pub name: String,
    /// Version.
    pub version: String,
    /// The files making up this release.
    #[serde(default)]
    pub files: Vec<FileSpec>,
    /// Protocol-specific extras (serialized JSON), carried verbatim into the
    /// artifact `proprietary` field when no wrapper interprets them.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

/// A blob returned by a fetch, independent of protocol.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlobOutput {
    /// Logical filename.
    pub name: String,
    /// Media type.
    #[serde(default)]
    pub media_type: String,
    /// sha256 hex.
    #[serde(default)]
    pub sha256: String,
    /// Byte length.
    pub size: i64,
    /// Raw bytes.
    #[serde(default)]
    pub data: Vec<u8>,
}
