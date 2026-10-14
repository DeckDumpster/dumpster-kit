//! **THE DESTRUCTION PATH.** One of two, and the other one is
//! [`crate::backup`].
//!
//! Destroying a tenant's key is recording a **tombstone**: a row in the
//! key-state registry saying this `database_id`'s key will not be derived
//! again. That is the entire act. It writes one row and touches nothing else
//! — in particular it does not touch, read, move, rewrite or even look for
//! the master key file.
//!
//! ## Why it is not "delete the key"
//!
//! There is one master key and every tenant's key comes out of it, so there
//! is no per-tenant key on disk to delete. Deleting the master key would
//! revoke *everybody*, which is not a deletion, it is an outage. The
//! tombstone is what makes revocation per-tenant at all.
//!
//! The trade is stated, not hidden: a tombstone stops **our code** deriving
//! that key. It is not itself an unrecoverable erasure of the master key's
//! ability to re-derive it — anybody holding the master key and willing to
//! bypass this registry could. That is exactly why the registry, and not the
//! master key, is what deletion touches, and it is why the tombstone is
//! defence in **depth**: the actual erasure is the partition drop (item 8),
//! and this is the second lock on the same door.
//!
//! ## Why it never touches the backup path
//!
//! See [`crate::backup`] for the table. The short version: a lost key and a
//! deleted tenant are cryptographically identical, so the *operational*
//! distinction has to be structural. This module can fail with the master key
//! perfectly healthy, and [`crate::backup`] can fail with every tombstone in
//! place, and neither failure can be mistaken for the other because neither
//! path runs through the other's code. `tests/separation.rs` asserts that
//! mechanically, so it stays true of code nobody has written yet.

use rusqlite::Connection;

use crate::error::Result;
use crate::state::{self, TenantKeyState};

/// Record that `database_id`'s key was destroyed on purpose.
///
/// After this, [`crate::derive::tenant_key`] refuses that id with
/// [`crate::error::KeyError::Tombstoned`] — the one error in this crate that
/// [`crate::error::KeyError::is_deliberate_revocation`] answers `true` for —
/// and keeps refusing it whether or not the master key is present, whether or
/// not the user row still exists, and whether or not this box is the one that
/// recorded it.
///
/// Idempotent, keeping the first tombstone: *when* a key was destroyed is
/// part of the record, so a second call does not restamp it.
///
/// This is what item 8's deletion path calls. It is not the deletion — the
/// partition drop is — it is the half that makes the drop irreversible.
pub fn tombstone(
    conn: &Connection,
    database_id: &str,
    reason: Option<&str>,
) -> Result<TenantKeyState> {
    state::tombstone(conn, database_id, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::KeyError;
    use crate::state::{KeyState, tests::registry};

    const A: &str = "01J0000000000000000000000A";
    const B: &str = "01J0000000000000000000000B";

    #[test]
    fn tombstoning_records_a_terminal_revocation() {
        let conn = registry();
        state::register(&conn, A).unwrap();
        let row = tombstone(&conn, A, Some("account deleted")).unwrap();
        assert_eq!(row.state, KeyState::Tombstoned);
        assert!(row.tombstoned_at.is_some());
        assert_eq!(row.reason.as_deref(), Some("account deleted"));

        let err = state::register(&conn, A).unwrap_err();
        assert!(matches!(err, KeyError::TombstoneIsTerminal { .. }), "{err}");
    }

    /// One tenant's destruction is one tenant's destruction. The master key
    /// is shared; this must not be.
    #[test]
    fn destroying_one_tenants_key_leaves_every_other_tenant_alone() {
        let conn = registry();
        state::register(&conn, A).unwrap();
        state::register(&conn, B).unwrap();
        tombstone(&conn, A, None).unwrap();
        assert_eq!(
            state::find(&conn, B).unwrap().unwrap().state,
            KeyState::Active
        );
    }

    /// The destruction path needs no master key, and that is the point: it
    /// runs to completion on a box where the backup path cannot run at all.
    /// (The converse, and the source-level rule behind both, are in
    /// `tests/separation.rs`.)
    #[test]
    fn destruction_needs_no_master_key() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("there-is-no-key-here");
        let _guard = crate::test_support::EnvGuard::set(&absent);

        let conn = registry();
        let row = tombstone(&conn, A, Some("no key on this box")).unwrap();
        assert_eq!(row.state, KeyState::Tombstoned);
        assert!(!absent.exists(), "destruction must not have created a key");
    }
}
