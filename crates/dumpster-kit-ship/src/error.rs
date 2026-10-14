//! What can go wrong, kept apart from what a tenant's own state can say.
//!
//! The distinction that matters here is the one [`dumpster_kit_keys::KeyError`]
//! draws and this crate must not blur: a tenant that cannot be shipped
//! because its key was **destroyed on purpose** is the system working, and a
//! tenant that cannot be shipped because something is **broken** is not.
//! [`crate::run`] turns the first into a skip nobody is paged for and the
//! second into one somebody is, so the two never arrive as one error string.

use std::path::PathBuf;

/// This crate's result type.
pub type Result<T> = std::result::Result<T, ShipError>;

/// A failure while shipping.
#[derive(Debug, thiserror::Error)]
pub enum ShipError {
    /// The outbox holds a row whose `occurred_at` is not a UTC timestamp.
    ///
    /// Fatal for that tenant rather than skipped past, because `as_of` is a
    /// partition value: a row whose date cannot be read is a row that would
    /// have to be filed under a made-up one, and a made-up partition is data
    /// nobody will ever find again.
    #[error(
        "outbox row {seq} has occurred_at {value:?}, which is not an ISO-8601 UTC instant.\n\
         `as_of` is a partition value derived from it, so there is no safe default: filing \
         the row under a guessed date would put it where nothing looks for it. The only \
         writer of this column is the trigger in schema_user.sql — a value it could not \
         have produced means something wrote the outbox by hand."
    )]
    Timestamp {
        /// The row's sequence number.
        seq: i64,
        /// What was in the column.
        value: String,
    },

    /// A tenant's database is registered but not on this box.
    #[error("no database at {} for {database_id}", .path.display())]
    NoDatabase {
        /// The registered id.
        database_id: String,
        /// Where its file should have been.
        path: PathBuf,
    },

    /// The object store refused, or the ciphertext did not authenticate.
    #[error("{0}")]
    Zone(String),

    /// The sealed object is not one of ours, or has been tampered with.
    #[error(
        "{key}: {detail}\n\
         Every object in the tenant zone is AES-256-GCM under that tenant's derived key, with \
         its own key string as associated data — so this also fails for an object that is \
         intact but has been MOVED, which is deliberate: a part decrypts under the prefix it \
         was written to and nowhere else."
    )]
    Ciphertext {
        /// The object key it was read from.
        key: String,
        /// What was wrong with it.
        detail: String,
    },

    /// Key custody refused, or could not answer.
    #[error(transparent)]
    Keys(#[from] dumpster_kit_keys::KeyError),

    /// The tenancy kernel refused.
    #[error(transparent)]
    Tenancy(#[from] dumpster_kit_tenancy::TenancyError),

    /// A SQLite error.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The lake's own configuration or object store.
    #[error(transparent)]
    Lake(#[from] dumpster_kit_lake::LakeError),

    /// Encoding a batch as Parquet.
    #[error(transparent)]
    Parquet(#[from] parquet::errors::ParquetError),
}
