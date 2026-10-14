//! Error type for the `dumpster-kit-lake` crate.

use thiserror::Error;

/// Anything that can go wrong configuring or writing to the lake.
#[derive(Debug, Error)]
pub enum LakeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("s3: {0}")]
    S3(String),

    /// The landing zone was asked for but is not configured. The message
    /// names the file the operator has to write — an unconfigured lake is a
    /// refusal, never a silent skip.
    #[error("{0}")]
    NotConfigured(String),

    /// The landing zone is missing, short, or does not match its manifest.
    /// Read-side only; the writer cannot produce one of these.
    #[error("{0}")]
    Raw(String),

    /// A partition value would not survive being a path component — an id
    /// carrying a separator, a malformed date. Distinct from
    /// [`LakeError::Raw`] because it is caught before anything is written,
    /// and the thing at risk is a key landing outside the prefix its
    /// tenant's deletion drops. See [`crate::tenant`].
    #[error("{0}")]
    Layout(String),
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, LakeError>;
