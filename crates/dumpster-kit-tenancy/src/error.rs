//! Error type for the tenancy kernel.

use thiserror::Error;

/// Anything that can go wrong in the kernel. A consumer's own error type wraps it with `From`.
#[derive(Debug, Error)]
pub enum TenancyError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The environment, a claim or a configuration value is not what the kernel will accept.
    #[error("environment: {0}")]
    Env(String),

    /// A referenced row does not exist. Maps to HTTP 404 at the API boundary.
    #[error("not found: {0}")]
    NotFound(String),

    /// A write would violate an invariant (a taken handle, a second active tenant for one
    /// email). Maps to HTTP 409.
    #[error("conflict: {0}")]
    Conflict(String),

    /// A database's `PRAGMA user_version` is higher than the build understands, so it was
    /// refused rather than opened. Never the caller's fault and never retryable: the fix is a
    /// different binary.
    #[error("schema version: {0}")]
    SchemaVersion(String),
}

pub type Result<T> = std::result::Result<T, TenancyError>;
