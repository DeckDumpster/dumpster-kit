//! The deletion itself: **tombstone, then drop, then prove**.
//!
//! ```text
//!   1. tombstone   registry.sqlite : tenant_key(<id>) -> tombstoned
//!   2. drop        tenant/database_id=<id>/  emptied, object by object
//!   3. verify      every read path attempted, every one required to fail
//! ```
//!
//! ## Why the tombstone goes first
//!
//! There is no transaction across a SQLite file and an object store, so one of
//! the two happens first and a crash can land between them. The two orders
//! fail differently and only one of the failures is safe:
//!
//! * **Tombstone first** — a crash leaves a tenant whose key nothing can
//!   derive and whose objects are therefore ciphertext nobody holds a key
//!   for, with some of them still present. The shipper will not ship them
//!   again ([`dumpster_kit_ship::run`] takes the key before it opens the database),
//!   nothing can read them, and re-running finishes the drop. The state is
//!   *more* deleted than intended, never less.
//! * **Drop first** — a crash leaves an ACTIVE tenant whose partition has
//!   vanished. Their key still derives, the shipper still ships, and the next
//!   night puts fresh holdings back under a prefix that was supposed to be
//!   gone. The deletion silently un-happens.
//!
//! So the order is not a preference. It is the difference between an
//! interrupted deletion that is safe to resume and one that reverses itself.
//!
//! ## Idempotence
//!
//! Every step is re-runnable and the whole thing is meant to be re-run:
//! tombstoning twice keeps the first record (*when* a key was destroyed is
//! part of what a tombstone is for), dropping an already-empty prefix removes
//! nothing and succeeds, and the verification is a read. A deletion
//! interrupted anywhere is finished by running it again.
//!
//! ## What it deliberately does not do
//!
//! * **It does not touch the master key.** Destroying that destroys every
//!   tenant at once, which is an outage rather than a deletion — see
//!   [`dumpster_kit_keys::destroy`], whose whole module doc is this point.
//! * **It does not touch the tenant's SQLite database, or their registry
//!   row.** The online side of an account's removal is the site's `tenant detach`
//!   and `tenant purge`, which are a different command in a different
//!   binary on a different box's schedule. This is the offline half, and
//!   conflating them would put a tenant-zone credential in the online CLI.
//! * **It does not un-delete.** There is no reverse. A tombstone is never
//!   lifted and the objects are gone.

use dumpster_kit_keys::master::KeyEnv;
use dumpster_kit_lake::{ObjectPurge, TenantZoneConfig};
use rusqlite::Connection;

use crate::error::Result;
use crate::sweep::{Dropped, Sweep};
use crate::verify::{StrayCopy, Verdict, verify};

/// What one deletion did, and what it proved.
#[derive(Debug, Clone)]
pub struct Deletion {
    /// The tenant that was deleted.
    pub database_id: String,
    /// When the key was revoked (the FIRST time, if this was a re-run).
    pub tombstoned_at: String,
    /// Whether this run is the one that recorded the tombstone.
    pub tombstone_was_already_there: bool,
    /// The partition drop.
    pub dropped: Dropped,
    /// The proof.
    pub verdict: Verdict,
}

/// **Delete a tenant from the tenant zone.**
///
/// Steps 1-3 of the module docs, in that order, and the verdict is returned
/// rather than consulted here — [`Verdict::into_result`] is what turns an
/// unproven deletion into an error, and the caller decides where that lands.
///
/// `stray` is an optional copy of one of this tenant's objects, taken before
/// the deletion, to be proven unopenable after it. It is optional because
/// most deletions have nobody standing by to take one; it exists because the
/// bead asks for the claim to be checked against a copy that *survived*, not
/// only against a prefix that is empty.
pub fn delete(
    env: &KeyEnv,
    zone: &dyn ObjectPurge,
    config: &TenantZoneConfig,
    registry: &Connection,
    database_id: &str,
    reason: Option<&str>,
    stray: Option<&StrayCopy>,
) -> Result<Deletion> {
    // 1. The tombstone. Before anything is removed — see the module docs.
    let already = dumpster_kit_keys::state::find(registry, database_id)?
        .is_some_and(|r| r.state == dumpster_kit_keys::KeyState::Tombstoned);
    let row = dumpster_kit_keys::destroy::tombstone(registry, database_id, reason)?;

    // 1a. Identity bindings: personal data that must not survive a deletion.
    // Idempotent — zero rows removed on a re-run is not an error.
    dumpster_kit_tenancy::registry::identity_delete_all(registry, database_id)?;

    // 2. The partition drop.
    let dropped = Sweep::new(zone, config, database_id)?.drop_partition()?;

    // 3. The proof.
    let verdict = verify(env, zone, config, registry, database_id, stray)?;

    Ok(Deletion {
        database_id: database_id.to_string(),
        tombstoned_at: row
            .tombstoned_at
            .unwrap_or_else(|| "an unrecorded time".to_string()),
        tombstone_was_already_there: already,
        dropped,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{A, B, dir_zone, env, registry, seal_into, seed};
    use dumpster_kit_keys::KeyState;

    #[test]
    fn a_deletion_tombstones_drops_and_proves_in_one_call() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings", "valuations"]);
        let stray = seal_into(&world, A, &store, &config);

        let done = delete(
            &env(),
            &store,
            &config,
            &world,
            A,
            Some("account closed"),
            Some(&stray),
        )
        .unwrap();

        assert!(!done.tombstone_was_already_there);
        assert_eq!(done.dropped.count(), 5, "four seeded parts and the stray's");
        assert!(
            done.verdict.proven(),
            "{:?}",
            done.verdict
                .failures()
                .iter()
                .map(|p| &p.detail)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            dumpster_kit_keys::state::find(&world, A)
                .unwrap()
                .unwrap()
                .state,
            KeyState::Tombstoned
        );
        assert!(done.verdict.into_result().is_ok());
    }

    /// The resumed run. An interrupted deletion is finished by repeating it,
    /// and repeating a finished one is not an error.
    #[test]
    fn deleting_twice_finishes_rather_than_fails() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings"]);

        let first = delete(&env(), &store, &config, &world, A, Some("closed"), None).unwrap();
        let second = delete(
            &env(),
            &store,
            &config,
            &world,
            A,
            Some("something else"),
            None,
        )
        .unwrap();

        assert!(!first.tombstone_was_already_there);
        assert!(second.tombstone_was_already_there);
        assert_eq!(second.dropped.count(), 0);
        assert!(second.verdict.proven());
        assert_eq!(
            first.tombstoned_at, second.tombstoned_at,
            "when a key was destroyed is part of the record; a re-run must not restamp it"
        );
    }

    /// The crash between the two steps, in the safe order: the tombstone is
    /// recorded and the objects are still there. Nothing can read them, and
    /// the next run removes them.
    #[test]
    fn a_deletion_interrupted_after_the_tombstone_is_resumable_and_safe_meanwhile() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings", "valuations"]);
        let stray = seal_into(&world, A, &store, &config);

        // Step 1 only — the process dies here.
        dumpster_kit_keys::destroy::tombstone(&world, A, Some("closed")).unwrap();

        // Meanwhile: the objects are present and NOT readable.
        let sweep = Sweep::new(&store, &config, A).unwrap();
        assert_eq!(sweep.list().unwrap().len(), 5);
        let err = dumpster_kit_keys::tenant_key(&env(), &world, A).unwrap_err();
        assert!(err.is_deliberate_revocation());

        // The re-run finishes it.
        let done = delete(&env(), &store, &config, &world, A, None, Some(&stray)).unwrap();
        assert_eq!(done.dropped.count(), 5);
        assert!(done.verdict.proven());
    }

    /// One tenant's deletion, and only one tenant's.
    #[test]
    fn deleting_one_tenant_leaves_the_other_whole() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings"]);
        seed(&store, B, &["holdings", "valuations"]);

        delete(&env(), &store, &config, &world, A, None, None).unwrap();

        assert_eq!(
            dumpster_kit_keys::state::find(&world, B)
                .unwrap()
                .unwrap()
                .state,
            KeyState::Active
        );
        assert!(dumpster_kit_keys::tenant_key(&env(), &world, B).is_ok());
        assert_eq!(
            Sweep::new(&store, &config, B)
                .unwrap()
                .list()
                .unwrap()
                .len(),
            4
        );
    }

    /// Deletion must not depend on provisioning having been tidy: an id that
    /// was never registered is still deleted, and still proven.
    #[test]
    fn an_unregistered_id_can_still_be_deleted() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[B]); // A is not registered
        seed(&store, A, &["holdings"]);

        let done = delete(&env(), &store, &config, &world, A, Some("orphan"), None).unwrap();
        assert_eq!(done.dropped.count(), 2);
        assert!(
            done.verdict.proven(),
            "a tombstone on an unregistered id is still a revocation"
        );
    }
}
