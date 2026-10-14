//! **The partition drop.** One tenant's prefix, emptied.
//!
//! ```text
//! tenant/database_id=<id>/          <- the whole of what a Sweep may touch
//!     dataset=holdings/as_of=…/part-seq-…
//!     dataset=valuations/as_of=…/part-…
//! ```
//!
//! ## Why this is a prefix and not a query
//!
//! `database_id` is the FIRST partition component of the tenant zone
//! (`dumpster_kit_lake::tenant`), above `dataset=`, and that ordering was chosen for
//! this module: one prefix covers a tenant's holdings *and* their valuations,
//! so derived artifacts inherit the deletion obligation without a second sweep
//! having to go and find them. It is also why the zone is plain Parquet rather
//! than Iceberg — dropping a prefix is a delete per object, where an Iceberg
//! row delete is a file rewrite.
//!
//! ## The confinement
//!
//! A [`Sweep`] is constructed for one `database_id` and computes its own
//! prefix from it. Every key it is about to delete is checked against that
//! prefix first, and a key outside it is [`EraseError::OutsideThePrefix`] —
//! fatal, not skipped. The keys come from listing that same prefix, so under
//! normal operation the check can never fire; it is there because the cost of
//! being wrong is another tenant's data, and because a listing is an answer
//! from a remote service rather than a fact.
//!
//! ## What a second run does
//!
//! Finishes. Deletion is re-run after a crash and after a partial failure,
//! `delete` on an absent key is not an error at either store, and a prefix
//! holding nothing lists empty — so [`Sweep::drop_partition`] on an
//! already-dropped tenant reports zero objects and succeeds. That is the
//! property that lets the tombstone go first: an interrupted deletion leaves a
//! tenant nothing can derive a key for, and the sweep can be finished at
//! leisure.

use dumpster_kit_lake::{ObjectPurge, TenantZoneConfig};

use crate::error::{EraseError, Result};

/// A drop confined to one tenant's prefix.
pub struct Sweep<'a> {
    zone: &'a dyn ObjectPurge,
    database_id: String,
    /// The prefix, already rooted at the configured zone root — so a test
    /// tier's `DUMPSTAR_TENANT_S3_PREFIX` and production's `tenant/` are the
    /// same code path, and the confinement is against the prefix that will
    /// actually be addressed rather than the one the constant names.
    prefix: String,
}

/// What one drop removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// The tenant whose partition it was.
    pub database_id: String,
    /// The prefix that was emptied.
    pub prefix: String,
    /// The keys removed, in listing order. Kept rather than counted: a
    /// deletion is a thing an operator may have to account for afterwards,
    /// and "44 objects" is not an account of anything.
    pub keys: Vec<String>,
}

impl Dropped {
    /// How many objects were removed.
    pub fn count(&self) -> usize {
        self.keys.len()
    }
}

impl<'a> Sweep<'a> {
    /// Confine a sweep to `database_id`'s prefix.
    ///
    /// Fails if the id could not be a partition component — the same refusal
    /// `dumpster_kit_lake::tenant_prefix` makes for the same reason, reached here
    /// before anything is listed rather than after.
    pub fn new(
        zone: &'a dyn ObjectPurge,
        config: &TenantZoneConfig,
        database_id: &str,
    ) -> Result<Self> {
        let prefix = config.rooted(dumpster_kit_lake::tenant_prefix(database_id)?);
        Ok(Self {
            zone,
            database_id: database_id.to_string(),
            prefix,
        })
    }

    /// The prefix this sweep is confined to.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The tenant this sweep is about.
    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    /// Every object currently under this tenant's prefix.
    ///
    /// The read half, used by the drop and again by the verification — the
    /// same listing both times, so "empty afterwards" is measured the way
    /// "these are the objects" was.
    pub fn list(&self) -> Result<Vec<String>> {
        Ok(self.zone.list(&self.prefix)?)
    }

    /// **Drop the partition.** Remove every object under this tenant's prefix.
    ///
    /// Not a transaction, because an object store has none: objects go one at
    /// a time and a failure part-way through leaves the rest. That is
    /// survivable rather than ignored — the tombstone has already made the
    /// remainder unreadable, and a second run finishes the job.
    pub fn drop_partition(&self) -> Result<Dropped> {
        let keys = self.list()?;
        for key in &keys {
            self.check(key)?;
            self.zone.delete(key)?;
        }
        Ok(Dropped {
            database_id: self.database_id.clone(),
            prefix: self.prefix.clone(),
            keys,
        })
    }

    /// Refuse a key outside the prefix. See the module docs.
    fn check(&self, key: &str) -> Result<()> {
        if key.starts_with(&self.prefix) {
            return Ok(());
        }
        Err(EraseError::OutsideThePrefix {
            key: key.to_string(),
            prefix: self.prefix.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{A, B, dir_zone, seed};

    #[test]
    fn a_drop_removes_one_tenants_holdings_and_valuations_together() {
        let (_tmp, store, config) = dir_zone();
        seed(&store, A, &["holdings", "valuations"]);
        seed(&store, B, &["holdings"]);

        let sweep = Sweep::new(&store, &config, A).unwrap();
        let dropped = sweep.drop_partition().unwrap();

        assert_eq!(dropped.count(), 4, "two datasets, two parts each");
        assert!(
            dropped
                .keys
                .iter()
                .any(|k| k.contains("dataset=valuations")),
            "valuations are tenant data too and are dropped by the same prefix: {:?}",
            dropped.keys
        );
        assert!(sweep.list().unwrap().is_empty());
    }

    /// One tenant's deletion is one tenant's deletion. The prefix is the whole
    /// mechanism, so this is the test that it is the right prefix.
    #[test]
    fn a_drop_touches_no_other_tenant() {
        let (_tmp, store, config) = dir_zone();
        seed(&store, A, &["holdings"]);
        seed(&store, B, &["holdings", "valuations"]);

        Sweep::new(&store, &config, A)
            .unwrap()
            .drop_partition()
            .unwrap();

        let b = Sweep::new(&store, &config, B).unwrap();
        assert_eq!(b.list().unwrap().len(), 4, "B must be untouched");
    }

    /// The neighbour whose id STARTS WITH this one's.
    ///
    /// `ObjectPurge::list` is a raw key-prefix match — it is
    /// `ObjectSource::list_keys`, deliberately, because a sweep enumerating a
    /// prefix differently from the reader that later checks it is how a
    /// deletion leaves an object a reader can still see. That is the right
    /// definition and it cuts both ways: under it the ONLY thing standing
    /// between `database_id=<A>` and `database_id=<A>B` is the trailing slash
    /// on [`dumpster_kit_lake::tenant_prefix`], and `check` cannot catch the
    /// difference either — `tenant/database_id=<A>B/…` genuinely starts with
    /// `tenant/database_id=<A>`.
    ///
    /// So the confinement is asserted here as a *behaviour over a store*
    /// rather than only as a property of a string. Ids are ULIDs in practice
    /// and this pairing will not occur; `validate_id` accepts any alphanumeric
    /// run, so it is not impossible, and the cost of it is another tenant's
    /// data deleted by a run that reported success.
    #[test]
    fn a_drop_does_not_reach_a_tenant_whose_id_extends_this_one() {
        let (_tmp, store, config) = dir_zone();
        let longer = format!("{A}B");
        seed(&store, A, &["holdings", "valuations"]);
        seed(&store, &longer, &["holdings", "valuations"]);

        let dropped = Sweep::new(&store, &config, A).unwrap().drop_partition();
        let dropped = dropped.expect("dropping A must not fail");

        assert_eq!(
            dropped.count(),
            4,
            "only A's four objects belong to this drop: {:?}",
            dropped.keys
        );
        assert!(
            !dropped
                .keys
                .iter()
                .any(|k| k.contains(&format!("database_id={longer}/"))),
            "the neighbour's objects were swept: {:?}",
            dropped.keys
        );
        assert_eq!(
            Sweep::new(&store, &config, &longer)
                .unwrap()
                .list()
                .unwrap()
                .len(),
            4,
            "{longer} is a different tenant and must still hold everything"
        );
    }

    /// The catalog zone shares the bucket. A sweep must not be able to reach
    /// it — here the prefix is what stops it, and in the bucket the tenant
    /// role's explicit `Deny` on `raw/` and `lake/` stops it again.
    #[test]
    fn a_drop_does_not_reach_the_catalog_zone() {
        let (_tmp, store, config) = dir_zone();
        seed(&store, A, &["holdings"]);
        use dumpster_kit_lake::ObjectStore;
        store
            .put(
                "raw/source=tcgcsv/dataset=groups/x/part-0000.json",
                b"c".to_vec(),
            )
            .unwrap();
        store
            .put("lake/warehouse/catalog/prices/x.parquet", b"c".to_vec())
            .unwrap();

        Sweep::new(&store, &config, A)
            .unwrap()
            .drop_partition()
            .unwrap();

        assert_eq!(
            ObjectPurge::list(&store, "raw/").unwrap().len(),
            1,
            "the catalog zone must be untouched"
        );
        assert_eq!(ObjectPurge::list(&store, "lake/").unwrap().len(), 1);
    }

    /// The second run of an interrupted deletion. It has to finish rather than
    /// fail on what it already removed.
    #[test]
    fn dropping_twice_is_the_second_run_of_a_crashed_one() {
        let (_tmp, store, config) = dir_zone();
        seed(&store, A, &["holdings"]);
        let sweep = Sweep::new(&store, &config, A).unwrap();

        assert_eq!(sweep.drop_partition().unwrap().count(), 2);
        let again = sweep.drop_partition().unwrap();
        assert_eq!(again.count(), 0, "nothing left, and that is a success");
        assert!(sweep.list().unwrap().is_empty());
    }

    /// A tenant who never had anything in the zone is dropped successfully.
    /// Deletion must not depend on the tenant having shipped.
    #[test]
    fn dropping_a_tenant_with_no_objects_succeeds() {
        let (_tmp, store, config) = dir_zone();
        let sweep = Sweep::new(&store, &config, A).unwrap();
        assert_eq!(sweep.drop_partition().unwrap().count(), 0);
    }

    /// The confinement, exercised directly — the listing can never produce
    /// such a key, which is exactly why the check is worth having.
    #[test]
    fn a_key_outside_the_prefix_is_fatal() {
        let (_tmp, store, config) = dir_zone();
        let sweep = Sweep::new(&store, &config, A).unwrap();

        for key in [
            "raw/source=tcgcsv/x",
            &format!("tenant/database_id={B}/dataset=holdings/as_of=2026-08-14/part-0000"),
            "tenant/",
            "",
        ] {
            let err = sweep.check(key).unwrap_err();
            assert!(
                matches!(err, EraseError::OutsideThePrefix { .. }),
                "{key:?} should have been refused: {err}"
            );
        }
        // …and its own keys are not refused.
        sweep
            .check(&format!(
                "tenant/database_id={A}/dataset=holdings/as_of=2026-08-14/part-0000"
            ))
            .unwrap();
    }

    /// An id that could address a key outside its own partition is refused
    /// before a prefix is ever built out of it.
    #[test]
    fn an_id_that_is_not_a_partition_component_is_refused() {
        let (_tmp, store, config) = dir_zone();
        for bad in ["", "../../raw", "a/b", "a=b"] {
            assert!(
                Sweep::new(&store, &config, bad).is_err(),
                "{bad:?} should not become a prefix"
            );
        }
    }
}
