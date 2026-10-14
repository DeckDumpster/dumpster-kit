//! The **tenant zone** — where holdings and valuations live offline.
//!
//! A different object from the catalog zone that happens to share a bucket
//! with it. `raw/` and `lake/` are cross-tenant, shared, indefinitely
//! retained and read with broad credentials; `tenant/` is always
//! tenant-keyed, retained 90 days, and read with credentials that reach
//! nothing else. The standing rule — *tenant data never enters the lake* —
//! is about the CATALOG, and it is restated here rather than broken: the
//! tenant zone is governed separately, which is the whole reason it is a
//! separate zone at all.
//!
//! ```text
//! tenant/database_id=<id>/dataset=<holdings|valuations>/as_of=YYYY-MM-DD/part-NNNN.parquet
//! ```
//!
//! Four things about that line are decisions, not implementation:
//!
//! - **`database_id` is the FIRST partition.** Deleting a tenant has to be a
//!   prefix drop, and putting the id above `dataset=` makes ONE prefix cover
//!   that tenant's holdings *and* their valuations. Derived artifacts
//!   inherit the deletion obligation, so they must not need a second sweep
//!   to find. [`tenant_prefix`] is that prefix, and it is the unit item 8
//!   drops.
//! - **Plain partitioned Parquet, not Iceberg.** Iceberg records absolute
//!   paths in its metadata, so moving this zone to its own bucket later
//!   would mean rewriting manifests; plain files keep that a location
//!   change. It also gives up snapshots and time travel deliberately —
//!   holdings want CURRENT STATE per tenant, deletable, not history.
//! - **Hive-style `key=value` components**, matching [`crate::keys`], so a
//!   reader (DuckDB, pyarrow) recovers the partition values from the path
//!   without a side table.
//! - **`as_of=` is a date, and retention is measured on OBJECT AGE.** See
//!   [`RETENTION_DAYS`]: an object written today is gone in 90 days whatever
//!   its `as_of` says, so a tenant whose state is still current has to be
//!   re-materialised inside that window or they age out of the zone. That is
//!   a constraint on the shipper, stated here because this module is what
//!   makes it true.
//!
//! ## What this module is not
//!
//! It is not a writer. Nothing in this crate puts an object under `tenant/`
//! — the shipper is its own item, and until it exists the zone is meant to
//! be empty. What lives here is where the bytes will go and who is allowed
//! to reach them; the policy documents that *enforce* the second half are in
//! `deploy/policies/tenant-zone/`, and `tests/lake/tenant_zone.sh` is what
//! proves they are not merely decorative.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{KEY_CATALOG_PROFILE, LakeEnv, parse_env, read_env_file};
use crate::error::{LakeError, Result};

/// The tenant zone's key prefix, and the boundary every credential policy is
/// written against. Changing it orphans every object already shipped **and**
/// silently widens both policies, which is why it is a constant here and a
/// literal in `deploy/policies/tenant-zone/*.json` with a test across them.
pub const TENANT_ROOT: &str = "tenant/";

/// The catalog zone's prefixes — the ones tenant credentials must NOT reach.
///
/// `raw/` is the landing zone ([`crate::keys`]); `lake/` is the Iceberg
/// warehouse Nessie is configured with. Both are cross-tenant and retained
/// indefinitely. They are listed here because the isolation gate asserts the
/// boundary in both directions, and "the other side" needs a name.
pub const CATALOG_ROOTS: &[&str] = &["raw/", "lake/"];

/// How long an object may live in the tenant zone.
///
/// **A hard product limit, not a tunable.** The catalog's indefinite
/// retention is justified by "we may need to rebuild any historical price";
/// no equivalent argument covers holdings. Two consequences follow and both
/// are wanted: 90 days IS the backfill window — a price correction reaches
/// the last 90 days and no further — and a missed deletion has a BOUNDED
/// blast radius, because anything not explicitly deleted ages out within 90
/// days regardless. That is what makes 90 days safer than "indefinite, with
/// a delete button".
///
/// Enforced mechanically by the lifecycle rule in
/// `deploy/policies/tenant-zone/lifecycle.json`, applied by
/// `deploy/setup-tenant-zone.sh`. Raising it is a decision to be filed, not
/// a default to be edited.
pub const RETENTION_DAYS: u32 = 90;

/// What a tenant partition holds.
///
/// Valuations are tenant data too — same partitioning, same retention, same
/// deletion — which is why they are a dataset in this zone rather than
/// something derived into the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dataset {
    /// What a tenant owns: one row per physical card, shipped from the
    /// outbox.
    Holdings,
    /// What it was worth, computed offline against `catalog.prices`.
    Valuations,
}

impl Dataset {
    /// Every dataset, for a caller that has to do something once per
    /// dataset — enumerating a tenant's partitions, most of all. A dataset
    /// missing from this list is one a sweep would never look at, and the
    /// deletion path is a sweep.
    pub const ALL: &'static [Dataset] = &[Dataset::Holdings, Dataset::Valuations];

    /// The `dataset=` partition value. On-disk layout: a change here
    /// orphans every object already shipped.
    pub fn as_str(self) -> &'static str {
        match self {
            Dataset::Holdings => "holdings",
            Dataset::Valuations => "valuations",
        }
    }
}

impl std::fmt::Display for Dataset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything belonging to one tenant, under a single prefix.
///
/// **This is the deletion unit.** Dropping it removes that tenant's
/// holdings and their valuations together, because `database_id` sits above
/// `dataset=`. Item 8 drops exactly this and nothing else has to be found.
pub fn tenant_prefix(database_id: &str) -> Result<String> {
    validate_id(database_id)?;
    Ok(format!("{TENANT_ROOT}database_id={database_id}/"))
}

/// One tenant's whole dataset, every date of it.
///
/// What a *reader* works from. A writer always knows which day it is writing
/// — `as_of` comes out of the event — but a consumer asking "what does this
/// tenant hold" cannot know which partitions exist, because the answer is
/// however many days the shipper has run over. So the date is the one
/// component this prefix leaves off.
///
/// It is not a deletion unit either — [`tenant_prefix`] is, and one prefix
/// covering both datasets is why `database_id` sits above `dataset=`. The
/// verification (`pd-qbrf`) reads at this level because it checks each
/// dataset **by name** as well as in bulk: [`Dataset::ALL`] is the
/// enumeration a sweep would have to cover, so a dataset added later and
/// partitioned somewhere the tenant prefix does not reach is named rather
/// than averaged into a count that was already zero.
pub fn dataset_prefix(database_id: &str, dataset: Dataset) -> Result<String> {
    Ok(format!(
        "{}dataset={}/",
        tenant_prefix(database_id)?,
        dataset.as_str()
    ))
}

/// One tenant's partition for one dataset on one date.
pub fn partition_prefix(database_id: &str, dataset: Dataset, as_of: &str) -> Result<String> {
    validate_date(as_of)?;
    Ok(format!(
        "{}as_of={}/",
        dataset_prefix(database_id, dataset)?,
        as_of
    ))
}

/// What every object in this zone is: Parquet, sealed.
///
/// The `.enc` is not decoration. Every byte under `tenant/` is AES-256-GCM
/// under that tenant's derived key (`pd-ulds`), because crypto-shredding is
/// the defence in depth beside the partition drop — so a key that said
/// `.parquet` would be describing something no reader could open. The
/// envelope and the reason it wraps the whole file rather than using
/// Parquet's own modular encryption are in `dumpstar_ship::cipher`.
pub const PART_SUFFIX: &str = ".parquet.enc";

/// The key of one part. `part` is the zero-based ordinal within the
/// partition; parts are numbered rather than named so a writer needs no
/// coordination beyond its own counter.
///
/// This is the form for a dataset whose parts have no identity of their own —
/// valuations, recomputed whole for a date. **Holdings do not use it**: they
/// are shipped incrementally and at-least-once, so a part has to be
/// addressable by the rows it carries rather than by its position in a run.
/// See [`range_part_key`].
pub fn part_key(database_id: &str, dataset: Dataset, as_of: &str, part: u32) -> Result<String> {
    Ok(format!(
        "{}part-{part:04}{PART_SUFFIX}",
        partition_prefix(database_id, dataset, as_of)?
    ))
}

/// The key of one part, named for the outbox range it carries.
///
/// The whole of the shipper's idempotence (`pd-dxn3`): delivery is
/// at-least-once, so a part is sometimes written twice, and a part addressed
/// by *what is in it* makes the second write land on the first one rather
/// than beside it. An ordinal cannot do that — two runs disagree about where
/// in the sequence they started, so they disagree about the number.
///
/// Zero-padded to twelve digits so a listing sorts in sequence order, which
/// is what makes "what does this partition hold" answerable from a directory
/// listing alone.
pub fn range_part_key(
    database_id: &str,
    dataset: Dataset,
    as_of: &str,
    from_seq: i64,
    to_seq: i64,
) -> Result<String> {
    if from_seq < 1 || to_seq < from_seq {
        return Err(LakeError::Layout(format!(
            "part range {from_seq}..{to_seq} is not a range of sequence numbers; the outbox \
             numbers from 1 and a part is never empty"
        )));
    }
    Ok(format!(
        "{}part-seq-{from_seq:012}-{to_seq:012}{PART_SUFFIX}",
        partition_prefix(database_id, dataset, as_of)?
    ))
}

/// Does `key` belong to the tenant zone? The one place the question is
/// answered, so a guard and a policy cannot drift into disagreeing.
pub fn is_tenant_key(key: &str) -> bool {
    key.starts_with(TENANT_ROOT)
}

/// Does `key` belong to the catalog zone?
pub fn is_catalog_key(key: &str) -> bool {
    CATALOG_ROOTS.iter().any(|root| key.starts_with(root))
}

/// Refuse an id that could not be a path component.
///
/// This is a **layout** guard, not an identity check: the registry
/// (`dumpstar_db::registry`) is the authority on which ids exist, and the
/// shipper reads them from it, so every id reaching this module is real by
/// construction. What this refuses is the other failure — a string
/// carrying `/`, `=` or `..` would write *outside* its own partition, which
/// would make a tenant's data unreachable by the prefix its deletion drops.
/// Deliberately weaker than `validate_database_id`, and deliberately not a
/// copy of it: two spellings of one rule drift, and this one has a
/// different job.
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(LakeError::Layout("database id is empty".into()));
    }
    if let Some(bad) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(LakeError::Layout(format!(
            "database id {id:?} contains {bad:?}; a partition component may hold only \
             A-Z a-z 0-9 - _, because anything else can address a key outside the \
             prefix this tenant's deletion drops"
        )));
    }
    Ok(())
}

/// `as_of` is a partition value, so a malformed one is a partition nobody
/// will find rather than an error at read time.
fn validate_date(as_of: &str) -> Result<()> {
    let ok = as_of.len() == 10
        && as_of.as_bytes()[4] == b'-'
        && as_of.as_bytes()[7] == b'-'
        && as_of
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    if !ok {
        return Err(LakeError::Layout(format!(
            "as_of {as_of:?} is not YYYY-MM-DD"
        )));
    }
    Ok(())
}

// ── Configuration ───────────────────────────────────────────────────────────

/// Where the tenant zone is, and who may reach it.
///
/// The bucket is **the same bucket as the lake** — decided 2026-08-13, one
/// bucket and a separate prefix, to be revisited once the arrangement is
/// proven — so it is not repeated here: [`crate::LakeConfig`] already
/// resolves it, and a second bucket setting would be a second thing to get
/// out of step. What this adds is the half that differs: the prefix, and the
/// credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantZoneConfig {
    /// The AWS profile whose role reaches `tenant/` and nothing else.
    pub profile: String,
    /// The zone's key prefix — [`TENANT_ROOT`] unless overridden.
    pub prefix: String,
}

impl TenantZoneConfig {
    /// Resolve from the process environment layered over
    /// `~/.config/dumpstar/lake.env`, environment winning — the same file and
    /// the same precedence as [`crate::LakeConfig`], because one file
    /// configuring both halves is what stops a job reading one zone's
    /// settings and another zone's credentials.
    pub fn load(env: &LakeEnv) -> Result<Self> {
        let path = env.config_path();
        let mut settings = match &path {
            Some(p) => read_env_file(p)?,
            None => BTreeMap::new(),
        };
        for key in [
            env.tenant_profile.as_str(),
            KEY_CATALOG_PROFILE,
            env.tenant_prefix.as_str(),
        ] {
            if let Ok(value) = std::env::var(key) {
                settings.insert(key.to_string(), value);
            }
        }
        Self::from_settings(env, &settings, path.as_deref())
    }

    /// Resolve from an explicit settings map. `config_path` only shapes the
    /// error text — it is the file an operator would have to write.
    pub fn from_settings(
        env: &LakeEnv,
        settings: &BTreeMap<String, String>,
        config_path: Option<&Path>,
    ) -> Result<Self> {
        let get = |k: &str| {
            settings
                .get(k)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };

        let tenant_profile_key = &env.tenant_profile;
        let where_from = match config_path {
            Some(p) => p.display().to_string(),
            None => format!("~/{}", env.config_relative_path),
        };

        let Some(profile) = get(&env.tenant_profile) else {
            return Err(LakeError::NotConfigured(format!(
                "the tenant zone has no credentials: {where_from} does not set \
                 {tenant_profile_key}.\n\
                 The tenant zone shares the lake's bucket and is separated from it by a prefix, \
                 and a prefix boundary is only a policy — so the credential that reaches \
                 `{TENANT_ROOT}` is a DIFFERENT one from the catalog's, from day one. There is no \
                 default: falling back to the catalog profile would erase the boundary silently, \
                 which is the one failure this setting exists to prevent.\n\
                 Create the role, then add to {where_from}:\n\
                 \x20 {tenant_profile_key}=<profile>   # reaches {TENANT_ROOT} and nothing else"
            )));
        };

        // The refusal that makes "separate credentials" mechanical rather
        // than aspirational. One profile for both zones is not a
        // misconfiguration that shows up later as a policy that happens to
        // be too wide — it is the boundary not existing at all, and it is
        // cheap to detect exactly here.
        if let Some(catalog) = get(KEY_CATALOG_PROFILE)
            && catalog == profile
        {
            return Err(LakeError::NotConfigured(format!(
                "{tenant_profile_key} and {KEY_CATALOG_PROFILE} are both {profile:?}.\n\
                 The tenant zone and the catalog zone share a bucket, so the ONLY thing \
                 separating them is that they are reached by different credentials. One \
                 profile for both is not a narrow policy — it is no boundary, and a zone \
                 governed by nothing looks exactly like a zone governed correctly until \
                 someone reads a tenant's holdings with the catalog's role.\n\
                 Give the tenant zone its own profile in {where_from}."
            )));
        }

        Ok(Self {
            profile,
            prefix: get(&env.tenant_prefix).unwrap_or_else(|| TENANT_ROOT.to_string()),
        })
    }

    /// A key from the free functions above, relocated under this zone's
    /// configured root.
    ///
    /// A no-op in production, where [`Self::prefix`] *is* [`TENANT_ROOT`].
    /// The free functions stay written against the constant deliberately:
    /// they are what `tests/lake/tenant_zone.sh` §8 reads to hold Rust, the
    /// IAM documents and the deploy script to one prefix, and a value that
    /// arrived from the environment could not be compared with a literal in
    /// a policy file.
    pub fn rooted(&self, key: String) -> String {
        match key.strip_prefix(TENANT_ROOT) {
            Some(rest) if self.prefix != TENANT_ROOT => format!("{}{rest}", self.prefix),
            _ => key,
        }
    }
}

/// Parse a `lake.env`-shaped file's text. Exposed for the deploy scripts'
/// tests, which read the same dotenv subset.
pub fn parse_settings(text: &str) -> BTreeMap<String, String> {
    parse_env(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> LakeEnv {
        LakeEnv::new(&dumpster_kit_tenancy::AppPrefix::new("TESTAPP").unwrap())
    }

    fn settings(pairs: &[(&String, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_key_layout_is_exactly_this() {
        assert_eq!(
            part_key(
                "01K2C7HQ8NZ0XW3V9R5M6D0ABC",
                Dataset::Holdings,
                "2026-08-13",
                0
            )
            .unwrap(),
            "tenant/database_id=01K2C7HQ8NZ0XW3V9R5M6D0ABC/dataset=holdings/\
             as_of=2026-08-13/part-0000.parquet.enc"
        );
        assert_eq!(
            part_key(
                "01K2C7HQ8NZ0XW3V9R5M6D0ABC",
                Dataset::Valuations,
                "2026-08-13",
                17
            )
            .unwrap(),
            "tenant/database_id=01K2C7HQ8NZ0XW3V9R5M6D0ABC/dataset=valuations/\
             as_of=2026-08-13/part-0017.parquet.enc"
        );
    }

    #[test]
    fn a_shipped_part_is_named_for_its_sequence_range() {
        assert_eq!(
            range_part_key(
                "01K2C7HQ8NZ0XW3V9R5M6D0ABC",
                Dataset::Holdings,
                "2026-08-13",
                1,
                4096
            )
            .unwrap(),
            "tenant/database_id=01K2C7HQ8NZ0XW3V9R5M6D0ABC/dataset=holdings/\
             as_of=2026-08-13/part-seq-000000000001-000000004096.parquet.enc"
        );
    }

    /// The property that makes the naming worth its awkwardness: a listing is
    /// in sequence order, so "what does this partition hold" needs no side
    /// table.
    #[test]
    fn range_part_keys_sort_in_sequence_order() {
        let key = |from, to| {
            range_part_key(
                "01K2C7HQ8NZ0XW3V9R5M6D0ABC",
                Dataset::Holdings,
                "2026-08-13",
                from,
                to,
            )
            .unwrap()
        };
        let mut keys = vec![key(1000, 1999), key(1, 9), key(10, 999)];
        keys.sort();
        assert_eq!(keys, [key(1, 9), key(10, 999), key(1000, 1999)]);
    }

    #[test]
    fn a_part_range_that_is_not_a_range_is_refused() {
        for (from, to) in [(0, 5), (-1, 5), (9, 3)] {
            assert!(
                range_part_key(
                    "01K2C7HQ8NZ0XW3V9R5M6D0ABC",
                    Dataset::Holdings,
                    "2026-08-13",
                    from,
                    to
                )
                .is_err(),
                "{from}..{to} should not name a part"
            );
        }
    }

    /// Every object in this zone is sealed, so every key says so — including
    /// the ordinal form the valuations will use. A dataset that started
    /// writing plaintext under a `.parquet.enc` key, or ciphertext under a
    /// `.parquet` one, would be a lie a reader could only discover by
    /// failing.
    #[test]
    fn every_key_form_carries_the_sealed_suffix() {
        let id = "01K2C7HQ8NZ0XW3V9R5M6D0ABC";
        for dataset in Dataset::ALL {
            assert!(
                part_key(id, *dataset, "2026-08-13", 0)
                    .unwrap()
                    .ends_with(PART_SUFFIX)
            );
            assert!(
                range_part_key(id, *dataset, "2026-08-13", 1, 2)
                    .unwrap()
                    .ends_with(PART_SUFFIX)
            );
        }
    }

    #[test]
    fn a_configured_root_relocates_a_key_and_nothing_else() {
        let cfg = TenantZoneConfig::from_settings(
            &env(),
            &settings(&[
                (&env().tenant_profile, "testapp-tenant"),
                (&env().tenant_prefix, "tenant-standin/"),
            ]),
            None,
        )
        .unwrap();
        let key = range_part_key(
            "01K2C7HQ8NZ0XW3V9R5M6D0ABC",
            Dataset::Holdings,
            "2026-08-13",
            1,
            2,
        )
        .unwrap();
        assert_eq!(
            cfg.rooted(key.clone()),
            key.replacen(TENANT_ROOT, "tenant-standin/", 1)
        );

        // …and the production case is the identity.
        let prod = TenantZoneConfig::from_settings(
            &env(),
            &settings(&[(&env().tenant_profile, "testapp-tenant")]),
            None,
        )
        .unwrap();
        assert_eq!(prod.rooted(key.clone()), key);
    }

    #[test]
    fn one_prefix_covers_every_dataset_a_tenant_has() {
        // The property deletion rests on: `database_id` above `dataset=`, so
        // the drop needs one prefix and no sweep can miss a dataset that was
        // added later. If this ever fails, item 8 is deleting half a tenant.
        let id = "01K2C7HQ8NZ0XW3V9R5M6D0ABC";
        let prefix = tenant_prefix(id).unwrap();
        for dataset in Dataset::ALL {
            let key = part_key(id, *dataset, "2026-08-13", 0).unwrap();
            assert!(
                key.starts_with(&prefix),
                "{dataset} lands outside the deletion prefix: {key} !^ {prefix}"
            );
        }
    }

    #[test]
    fn a_tenant_prefix_belongs_to_no_other_tenant() {
        let a = tenant_prefix("01K2C7HQ8NZ0XW3V9R5M6D0ABC").unwrap();
        let b = tenant_prefix("01K2C7HQ8NZ0XW3V9R5M6D0ABD").unwrap();
        assert!(!a.starts_with(&b) && !b.starts_with(&a));
        // The trailing slash is what makes that true — without it,
        // `database_id=01` would prefix-match `database_id=012`.
        assert!(a.ends_with('/'));
    }

    #[test]
    fn the_two_zones_never_overlap() {
        // Both directions of the boundary, as strings. The policies assert
        // the same thing against a real object store; this asserts that the
        // prefixes they are written against cannot collide in the first
        // place.
        assert!(is_tenant_key(
            "tenant/database_id=X/dataset=holdings/as_of=2026-08-13/p.parquet"
        ));
        assert!(!is_catalog_key(
            "tenant/database_id=X/dataset=holdings/as_of=2026-08-13/p.parquet"
        ));
        for root in CATALOG_ROOTS {
            let key = format!("{root}something");
            assert!(is_catalog_key(&key), "{key} should be a catalog key");
            assert!(!is_tenant_key(&key), "{key} must not be a tenant key");
        }
    }

    #[test]
    fn an_id_that_could_escape_its_partition_is_refused() {
        // Not identity checking — layout safety. Each of these would write
        // somewhere the tenant's own deletion prefix does not reach.
        for bad in [
            "",
            "../raw",
            "a/b",
            "a=b",
            "id with space",
            "01K2C7HQ8NZ0XW3V9R5M6D0AB.",
        ] {
            assert!(
                tenant_prefix(bad).is_err(),
                "{bad:?} should not be usable as a partition component"
            );
        }
        // And a real minted id is fine.
        assert!(tenant_prefix("01K2C7HQ8NZ0XW3V9R5M6D0ABC").is_ok());
    }

    #[test]
    fn as_of_must_be_a_date() {
        for bad in [
            "2026-8-13",
            "20260813",
            "2026-08-13T00:00:00Z",
            "",
            "latest",
        ] {
            assert!(
                partition_prefix("01K2C7HQ8NZ0XW3V9R5M6D0ABC", Dataset::Holdings, bad).is_err(),
                "{bad:?} should not be an as_of"
            );
        }
    }

    #[test]
    fn every_dataset_is_in_all() {
        // Exhaustiveness by construction: adding a variant without adding it
        // to ALL fails to compile here, which is what keeps a deletion sweep
        // from silently skipping a dataset.
        for dataset in Dataset::ALL {
            match dataset {
                Dataset::Holdings | Dataset::Valuations => {}
            }
        }
        assert_eq!(Dataset::ALL.len(), 2);
    }

    #[test]
    fn retention_is_ninety_days() {
        // Not a tuning knob. If this changes, the lifecycle document, the
        // backfill window and the bound on a missed deletion's blast radius
        // all change with it — which is a decision to file, and this line is
        // where it gets noticed.
        assert_eq!(RETENTION_DAYS, 90);
    }

    #[test]
    fn unconfigured_credentials_refuse_and_name_the_file() {
        let err = TenantZoneConfig::from_settings(
            &env(),
            &settings(&[]),
            Some(Path::new("/tmp/lake.env")),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("/tmp/lake.env"), "{err}");
        assert!(err.contains(&env().tenant_profile), "{err}");
    }

    #[test]
    fn one_profile_for_both_zones_is_refused() {
        // The boundary made mechanical. Same profile for both is not a
        // narrow policy that happens to be wide — it is no boundary at all.
        let err = TenantZoneConfig::from_settings(
            &env(),
            &settings(&[
                (&env().tenant_profile, "testapp"),
                (&KEY_CATALOG_PROFILE.to_string(), "testapp"),
            ]),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no boundary"), "{err}");
    }

    #[test]
    fn separate_profiles_resolve() {
        let cfg = TenantZoneConfig::from_settings(
            &env(),
            &settings(&[
                (&env().tenant_profile, "testapp-tenant"),
                (&KEY_CATALOG_PROFILE.to_string(), "testapp"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.profile, "testapp-tenant");
        assert_eq!(cfg.prefix, TENANT_ROOT);
    }

    #[test]
    fn the_prefix_override_is_test_tier_only_but_honoured() {
        let cfg = TenantZoneConfig::from_settings(
            &env(),
            &settings(&[
                (&env().tenant_profile, "testapp-tenant"),
                (&env().tenant_prefix, "tenant-standin/"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.prefix, "tenant-standin/");
    }
}
