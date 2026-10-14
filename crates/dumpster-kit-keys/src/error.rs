//! What can go wrong, kept **distinguishable on purpose**.
//!
//! This module is the load-bearing half of `pd-ulds`. Crypto-shredding has a
//! property that is a feature and a hazard at the same time:
//!
//! > **A lost key is indistinguishable from a deleted tenant.**
//!
//! Cryptographically that is the whole point — ciphertext nobody holds a key
//! for is ciphertext nobody can read, and *why* nobody holds the key does not
//! change the maths. Operationally it is a trap, because the two causes call
//! for opposite responses:
//!
//! * *the key was destroyed on purpose* — the system is working; the account
//!   is gone and must stay gone.
//! * *the key cannot be found* — the system is broken; restore it from the
//!   backup, and page somebody until that happens.
//!
//! Collapse those into one error and the failure is silent in both
//! directions. A backup failure starts reading as a legitimate deletion (data
//! loss wearing compliance as a costume), or a deletion gets implemented as
//! "we lost the backup", which revokes nothing the moment the backup turns up.
//!
//! So they are separate variants, and [`KeyError::is_deliberate_revocation`]
//! is the ONLY sanctioned way to ask "was this on purpose". It answers `true`
//! for exactly one variant. A caller — item 8's deletion path above all —
//! that wants to report "this tenant is gone" has to go through it, and
//! cannot get a `true` out of a missing file, an unreadable registry or an
//! I/O error no matter how it phrases the question.

use std::path::PathBuf;

/// Everything key custody can refuse to do.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// The master key was **destroyed on purpose for this tenant** — a
    /// tombstone was recorded, and derivation refuses from then on.
    ///
    /// The one variant that means "working as designed". Nothing else in
    /// this enum ever means that.
    #[error(
        "tenant key for {database_id} was REVOKED at {tombstoned_at} and will not be derived \
         again{}\n\
         This is a deliberate revocation, not a missing key: the tombstone in the registry is \
         what refuses, and it is recorded independently of whether the master key exists.",
        .reason.as_ref().map(|r| format!(" (reason: {r})")).unwrap_or_default()
    )]
    Tombstoned {
        /// The database whose key is revoked.
        database_id: String,
        /// When the tombstone was recorded (RFC 3339).
        tombstoned_at: String,
        /// Whatever the operator said at the time.
        reason: Option<String>,
    },

    /// No key state has ever been recorded for this `database_id`.
    ///
    /// Fail-closed, and deliberately not the same as [`Self::Tombstoned`]: a
    /// registry that came back empty from a restore must refuse *everything*
    /// loudly, rather than quietly deriving keys again for tenants whose
    /// tombstones went missing with it.
    #[error(
        "no key state is registered for {database_id} — refusing to derive a key.\n\
         This is NOT a revocation: nothing has been recorded about this database either way. \
         Register it (`keys register {database_id}`) if it is a live tenant. If a \
         registry was just restored and this is unexpected, STOP — a registry missing its rows \
         is also missing its tombstones."
    )]
    NotRegistered {
        /// The database nothing is recorded about.
        database_id: String,
    },

    /// The master key is not where it should be, or cannot be read.
    ///
    /// An **operational failure**, never a revocation. Whatever is downstream
    /// of this should stop and page, not conclude anything about a tenant.
    #[error(
        "the master key is unavailable at {}: {detail}\n\
         This is an OPERATIONAL FAILURE, not a deletion. No tenant has been revoked by it. \
         Restore the key from wherever the Litestream bootstrap credentials are kept (see \
         deploy/KEYS.md) — do not treat unreadable holdings as deleted ones.",
        .path.display()
    )]
    MasterKeyUnavailable {
        /// Where the key was looked for.
        path: PathBuf,
        /// Why it could not be used.
        detail: String,
    },

    /// The master key file is there but is not a master key file.
    #[error(
        "the master key at {} is malformed: {detail}\n\
         This is an OPERATIONAL FAILURE, not a deletion.",
        .path.display()
    )]
    MasterKeyMalformed {
        /// Where the bad file is.
        path: PathBuf,
        /// What is wrong with it.
        detail: String,
    },

    /// Refusing to write over a master key that already exists.
    ///
    /// Overwriting one destroys every tenant's key at once, which is the one
    /// irreversible act in this crate. It is never implicit.
    #[error(
        "a master key already exists at {} — refusing to overwrite it.\n\
         Overwriting the master key destroys EVERY tenant's derived key at once, and there is \
         no undo. If that is genuinely what you want, move the existing file aside by hand \
         first.",
        .path.display()
    )]
    MasterKeyExists {
        /// The file that would have been clobbered.
        path: PathBuf,
    },

    /// The master key file's permissions are wider than mode 600.
    #[error(
        "the master key at {} is mode {found:o}, not 600 — refusing to use it.\n\
         Fix it with: chmod 600 {}",
        .path.display(), .path.display()
    )]
    MasterKeyPermissions {
        /// The over-permissive file.
        path: PathBuf,
        /// The permission bits actually found.
        found: u32,
    },

    /// A `database_id` that is not a `database_id`.
    #[error("{0}")]
    InvalidDatabaseId(String),

    /// A tombstone is terminal — it is never lifted.
    #[error(
        "{database_id} is tombstoned; its key state cannot be set back to active.\n\
         A tombstone is the record that a key was destroyed on purpose. Reversing it would \
         make revocation a thing that can be undone by accident, which is the property this \
         registry exists to deny."
    )]
    TombstoneIsTerminal {
        /// The database somebody tried to reactivate.
        database_id: String,
    },

    /// Anything the filesystem said no to.
    #[error("key custody I/O: {0}")]
    Io(#[from] std::io::Error),

    /// Anything the key-state registry said no to.
    #[error("key state registry: {0}")]
    Registry(#[from] rusqlite::Error),

    /// Anything the tenancy kernel said no to on the way to the registry.
    #[error("key state registry: {0}")]
    Db(#[from] dumpster_kit_tenancy::TenancyError),
}

impl KeyError {
    /// Was this refusal a **deliberate revocation**?
    ///
    /// `true` for [`KeyError::Tombstoned`] and nothing else, forever. This is
    /// the only sanctioned way to ask the question, and the reason it exists
    /// as a method rather than as a `matches!` at each call site is that a
    /// call site is where the two get conflated: `Err(_) => "deleted"` is one
    /// keystroke away from turning a missing backup into a compliance claim.
    ///
    /// In particular a missing master key answers `false`. That is the whole
    /// point — see the module docs.
    pub fn is_deliberate_revocation(&self) -> bool {
        matches!(self, KeyError::Tombstoned { .. })
    }

    /// Is this an **operational** failure — something an operator has to go
    /// and fix, that says nothing at all about any tenant's status?
    ///
    /// The complement of [`Self::is_deliberate_revocation`] over the variants
    /// that can come out of a derivation, stated positively so a caller can
    /// branch on the thing it actually wants to act on.
    pub fn is_operational_failure(&self) -> bool {
        matches!(
            self,
            KeyError::MasterKeyUnavailable { .. }
                | KeyError::MasterKeyMalformed { .. }
                | KeyError::MasterKeyPermissions { .. }
                | KeyError::Io(_)
                | KeyError::Registry(_)
                | KeyError::Db(_)
        )
    }
}

/// This crate's result type.
pub type Result<T> = std::result::Result<T, KeyError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn tombstoned() -> KeyError {
        KeyError::Tombstoned {
            database_id: "01J000000000000000000000AA".into(),
            tombstoned_at: "2026-08-13T00:00:00Z".into(),
            reason: Some("account deleted".into()),
        }
    }

    /// The classification, variant by variant. Exhaustive on purpose: adding
    /// a variant without deciding which side it falls on fails to compile
    /// here, which is the only moment anybody is thinking about it.
    #[test]
    fn only_a_tombstone_is_a_deliberate_revocation() {
        let path = PathBuf::from("/nowhere/tenant-master.key");
        let cases: Vec<(KeyError, bool)> = vec![
            (tombstoned(), true),
            (
                KeyError::NotRegistered {
                    database_id: "x".into(),
                },
                false,
            ),
            (
                KeyError::MasterKeyUnavailable {
                    path: path.clone(),
                    detail: "No such file or directory".into(),
                },
                false,
            ),
            (
                KeyError::MasterKeyMalformed {
                    path: path.clone(),
                    detail: "not hex".into(),
                },
                false,
            ),
            (KeyError::MasterKeyExists { path: path.clone() }, false),
            (
                KeyError::MasterKeyPermissions {
                    path: path.clone(),
                    found: 0o644,
                },
                false,
            ),
            (KeyError::InvalidDatabaseId("nope".into()), false),
            (
                KeyError::TombstoneIsTerminal {
                    database_id: "x".into(),
                },
                false,
            ),
            (KeyError::Io(std::io::Error::other("disk on fire")), false),
        ];

        for (err, expected) in cases {
            assert_eq!(
                err.is_deliberate_revocation(),
                expected,
                "is_deliberate_revocation() misclassified: {err}"
            );
        }
    }

    /// The single most dangerous confusion in the crate, pinned on its own so
    /// a regression here fails with a sentence about what it means.
    #[test]
    fn a_missing_master_key_is_never_a_revocation() {
        let err = KeyError::MasterKeyUnavailable {
            path: PathBuf::from("/nowhere/tenant-master.key"),
            detail: "No such file or directory".into(),
        };
        assert!(
            !err.is_deliberate_revocation(),
            "a key we cannot find must never report as a tenant we deleted"
        );
        assert!(err.is_operational_failure());
        // …and it says so to a human, not just to a `match`.
        let text = err.to_string();
        assert!(text.contains("OPERATIONAL FAILURE"), "{text}");
        assert!(text.contains("not a deletion"), "{text}");
    }

    /// …and the converse, which is the other way the same bug shows up: a
    /// revocation reported as "we could not read it" revokes nothing once the
    /// thing that could not be read turns up again.
    #[test]
    fn a_revocation_is_never_an_operational_failure() {
        let err = tombstoned();
        assert!(err.is_deliberate_revocation());
        assert!(!err.is_operational_failure());
        let text = err.to_string();
        assert!(text.contains("REVOKED"), "{text}");
        assert!(text.contains("not a missing key"), "{text}");
        assert!(text.contains("account deleted"), "{text}");
    }
}
