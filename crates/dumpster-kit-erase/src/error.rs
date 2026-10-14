//! What can go wrong on the deletion path, and what each failure means.
//!
//! One distinction runs through this file and it is the same one
//! [`dumpster_kit_keys::error`] draws: **a thing that is not there** and **a thing
//! that was destroyed** are different events, and the deletion path has to
//! keep them apart in both directions.
//!
//! * On the way in, a tenant nobody has heard of is not a tenant who has been
//!   deleted, and [`EraseError::NotATenant`] says so rather than reporting a
//!   successful deletion of nothing.
//! * On the way out, a verification that failed to *run* is not a verification
//!   that *passed*, and [`EraseError::NotProven`] is what the whole item is
//!   about. Item 3's rule stated the other way round: the proof is that
//!   derivation refuses **deliberately**, so a box with no master key must
//!   never be able to report every tenant on it as deleted.

use std::path::PathBuf;

/// The deletion path's failures.
#[derive(Debug, thiserror::Error)]
pub enum EraseError {
    /// The name given does not resolve to anything.
    ///
    /// Not a silent success. A deletion asked for by a name that matches
    /// nobody is far more likely to be a typo than a tenant who was already
    /// removed, and reporting "deleted" for it would make the typo invisible
    /// exactly where being wrong is worst.
    #[error(
        "{name:?} is neither a registered handle nor a database_id.\n\
         The site's tenant listing says who exists on this box. If the registry row is already \
         gone and only the tenant zone's objects remain, pass the database_id itself — \
         deletion deliberately does not require provisioning to have been tidy, but it does \
         require you to name the partition being dropped."
    )]
    NotATenant {
        /// What was asked for.
        name: String,
    },

    /// The verification did not establish what it exists to establish.
    ///
    /// **This is the error the whole item turns on.** It is raised when a
    /// deletion ran and one of the read paths could not be shown closed —
    /// including the case where a path could not be *checked*, which is not
    /// the same as a path that is closed and must never be reported as one.
    #[error(
        "DELETION NOT PROVEN for {database_id}: {failures} of {checks} checks did not \
         establish that the data is unreachable.\n\
         The deletion may well have happened; what has not happened is the proof of it. \
         Nothing here retries or repairs on its own — see deploy/DELETION.md."
    )]
    NotProven {
        /// Whose deletion was not proven.
        database_id: String,
        /// How many checks were attempted.
        checks: usize,
        /// How many of them did not establish their claim.
        failures: usize,
    },

    /// The sweep was handed a key outside the prefix it is confined to.
    ///
    /// A bug rather than an operator error, and fatal on sight. The sweep
    /// deletes objects; a sweep that could be persuaded to address a key
    /// outside one tenant's prefix is a sweep that could delete another
    /// tenant's holdings, or the catalog.
    #[error(
        "REFUSING to delete {key:?}: it is not under {prefix:?}.\n\
         The deletion sweep is confined to exactly one tenant's prefix. A key outside it \
         reaching this point is a bug in how the sweep was built, not a misconfiguration, \
         and it is fatal because the next thing that would have happened is a delete."
    )]
    OutsideThePrefix {
        /// The key that was refused.
        key: String,
        /// The prefix the sweep is confined to.
        prefix: String,
    },

    /// A stray copy was named for the verification and could not be read.
    ///
    /// Distinct from "the copy did not open", which is the *result* being
    /// looked for: a file that is not there proves nothing at all.
    #[error(
        "the stray copy at {path} could not be read: {source}.\n\
         A copy that is not there is not a copy that failed to decrypt. Take the copy \
         BEFORE the deletion — see the site's erase `verify --help`."
    )]
    NoStrayCopy {
        /// Where the copy was expected.
        path: PathBuf,
        /// Why it could not be read.
        source: std::io::Error,
    },

    /// Key custody said no, or could not say anything.
    #[error(transparent)]
    Keys(#[from] dumpster_kit_keys::KeyError),

    /// The zone would not answer.
    #[error(transparent)]
    Lake(#[from] dumpster_kit_lake::LakeError),

    /// The registry would not answer.
    #[error(transparent)]
    Tenancy(#[from] dumpster_kit_tenancy::TenancyError),
}

/// This crate's result.
pub type Result<T> = std::result::Result<T, EraseError>;
