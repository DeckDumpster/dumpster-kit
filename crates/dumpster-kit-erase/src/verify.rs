//! **The proof.** "Proven, not asserted" is the literal bar this item was
//! written to, and this module is where the bar is met.
//!
//! A deletion that ran without error is not a deletion that worked. What is
//! checked here is the opposite claim, from the reader's side: for a deleted
//! `database_id`, every path by which that tenant's holdings or valuations
//! could still be reached is closed.
//!
//! ```text
//!   ┌── machinery ──── the box can still derive SOMEBODY's key
//!   │                  (without this, every check below is vacuous)
//!   ├── derivation ─── this tenant's key refuses, DELIBERATELY
//!   ├── partition ──── the tenant prefix lists nothing
//!   ├── dataset=… ──── and neither does any single dataset under it
//!   └── stray copy ─── a byte-for-byte copy taken BEFORE the deletion does
//!                      not open, because there is no key left to open it
//! ```
//!
//! ## The two ways a proof can be worthless, and what stops each
//!
//! **Vacuity from a broken box.** A box with no master key derives nothing
//! for anybody, so "no key could be derived" would be true of every tenant
//! alive. [`Check::Machinery`] runs first and establishes that the key
//! machinery works *at all*; the stray-copy check refuses to conclude
//! anything if it did not. The derivation check is immune for a different
//! reason and it is item 3's: [`dumpster_kit_keys::tenant_key`] consults the
//! registry BEFORE the master key, so `Tombstoned` is a fact about a row and
//! can never be produced by a missing file. This module insists on that
//! specific error rather than on any error, which is the whole of the
//! difference between "we destroyed this" and "we lost everything".
//!
//! **Vacuity from a meaningless subject.** A stray copy that is not a sealed
//! tenant-zone object proves nothing by failing to open — a text file does
//! not open either. So the copy is checked for the envelope magic first, and
//! a copy that is not one makes the check *fail* rather than pass.
//!
//! ## It answers the other way too
//!
//! Run against a tenant who has NOT been deleted, [`verify`] reports NOT
//! PROVEN and says which paths are still open — including opening the stray
//! copy and saying so. That is deliberate: a check that can only ever report
//! success is not a check, and this one is run in the failing direction by
//! both gates before it is trusted in the passing one.

use dumpster_kit_keys::master::KeyEnv;
use dumpster_kit_lake::{TenantDataset, TenantZoneConfig};
use rusqlite::Connection;

use crate::error::{EraseError, Result};
use crate::sweep::Sweep;

/// A copy of one zone object, taken before a deletion, to be proven
/// unreadable after it.
///
/// Both halves are needed and neither has a default. The bytes are obvious;
/// the key is not, and it is required because the object key is the sealed
/// envelope's associated data (`dumpster_kit_ship::cipher`). An object "failing to
/// open" because the wrong AAD was guessed would be a proof of nothing at
/// all, so the caller has to say which key the copy was taken from.
pub struct StrayCopy {
    /// The object key the copy was taken from.
    pub object_key: String,
    /// The bytes, exactly as they were in the zone.
    pub bytes: Vec<u8>,
}

/// Ciphertext, and named as such — but a `Debug` that dumped a whole part
/// into a test failure would be unreadable rather than unsafe, so it prints
/// the key and the size.
impl std::fmt::Debug for StrayCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrayCopy")
            .field("object_key", &self.object_key)
            .field("bytes", &format_args!("{} bytes", self.bytes.len()))
            .finish()
    }
}

impl StrayCopy {
    /// Read a copy off disk.
    pub fn read(object_key: &str, path: &std::path::Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|source| EraseError::NoStrayCopy {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            object_key: object_key.to_string(),
            bytes,
        })
    }
}

/// Which read path a proof is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Not a read path — the precondition that makes the others mean
    /// anything. See the module docs.
    Machinery,
    /// Deriving this tenant's key.
    Derivation,
    /// Listing the tenant's whole prefix.
    Partition,
    /// Listing one dataset under it, by name, so a dataset a sweep failed to
    /// cover is named rather than averaged away.
    Dataset(&'static str),
    /// Opening a copy taken before the deletion.
    StrayCopy,
}

impl std::fmt::Display for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Check::Machinery => f.write_str("machinery"),
            Check::Derivation => f.write_str("derivation"),
            Check::Partition => f.write_str("partition"),
            Check::Dataset(d) => write!(f, "dataset={d}"),
            Check::StrayCopy => f.write_str("stray copy"),
        }
    }
}

/// One check, and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    /// Which path.
    pub check: Check,
    /// Did the claim hold?
    ///
    /// For every check but [`Check::Machinery`] the claim is "this path is
    /// closed". A check that could not be *run* is `false` here, never `true`
    /// — an unestablished proof is not a passing one.
    pub held: bool,
    /// What was actually observed, in an operator's words.
    pub detail: String,
}

/// Every check, for one tenant.
#[derive(Debug, Clone)]
pub struct Verdict {
    /// Whose deletion this is about.
    pub database_id: String,
    /// The checks, in the order they ran.
    pub proofs: Vec<Proof>,
}

impl Verdict {
    /// Did every check hold?
    pub fn proven(&self) -> bool {
        self.proofs.iter().all(|p| p.held)
    }

    /// The checks that did not.
    pub fn failures(&self) -> Vec<&Proof> {
        self.proofs.iter().filter(|p| !p.held).collect()
    }

    /// The verdict, or [`EraseError::NotProven`].
    ///
    /// The conversion exists so a caller cannot get a `Verdict` and forget to
    /// look at it — which is the shape of every deletion that was reported
    /// done and was not.
    pub fn into_result(self) -> Result<Self> {
        if self.proven() {
            return Ok(self);
        }
        Err(EraseError::NotProven {
            database_id: self.database_id.clone(),
            checks: self.proofs.len(),
            failures: self.failures().len(),
        })
    }
}

/// Attempt every read path there is, and report which are closed.
///
/// Reads. Deletes nothing, tombstones nothing — so it is safe to run against
/// a live tenant, which is exactly how it is seen in the failing direction.
pub fn verify(
    env: &KeyEnv,
    zone: &dyn dumpster_kit_lake::ObjectPurge,
    config: &TenantZoneConfig,
    registry: &Connection,
    database_id: &str,
    stray: Option<&StrayCopy>,
) -> Result<Verdict> {
    let mut proofs = vec![machinery(env, registry, database_id)];
    let machinery_ok = proofs[0].held;

    proofs.push(derivation(env, registry, database_id));

    let sweep = Sweep::new(zone, config, database_id)?;
    let live = sweep.list()?;
    proofs.push(Proof {
        check: Check::Partition,
        held: live.is_empty(),
        detail: if live.is_empty() {
            format!("{} holds no objects", sweep.prefix())
        } else {
            format!(
                "{} still holds {} object(s), the first being {}",
                sweep.prefix(),
                live.len(),
                live[0]
            )
        },
    });

    // Per dataset as well as in bulk. The bulk listing is the one that
    // matters, and this is what makes a miss legible: `Dataset::ALL` is the
    // enumeration a sweep would have to cover, so a dataset added later and
    // partitioned somewhere the tenant prefix does not reach is named here
    // instead of vanishing into a count that was already zero.
    for dataset in TenantDataset::ALL {
        let prefix = config.rooted(dumpster_kit_lake::dataset_prefix(database_id, *dataset)?);
        let held = zone.list(&prefix)?;
        proofs.push(Proof {
            check: Check::Dataset(dataset.as_str()),
            held: held.is_empty(),
            detail: if held.is_empty() {
                format!("{prefix} holds no objects")
            } else {
                format!("{prefix} still holds {} object(s)", held.len())
            },
        });
    }

    if let Some(stray) = stray {
        proofs.push(stray_copy(env, registry, database_id, stray, machinery_ok));
    }

    Ok(Verdict {
        database_id: database_id.to_string(),
        proofs,
    })
}

/// Can this box still derive anybody's key?
///
/// Not a read path — the precondition the stray-copy check needs, and a
/// statement worth making out loud whatever the answer: a verification run on
/// a box whose master key is gone would otherwise find every tenant on it
/// beautifully unreadable.
fn machinery(env: &KeyEnv, registry: &Connection, subject: &str) -> Proof {
    let fingerprint = match dumpster_kit_keys::derive::master_fingerprint(env) {
        Ok(f) => f,
        Err(e) => {
            return Proof {
                check: Check::Machinery,
                held: false,
                detail: format!(
                    "the master key is not usable on this box ({e}), so NOTHING here can be \
                     derived and 'unreadable' would be true of every tenant alive. This is not \
                     a statement about {subject} — see deploy/KEYS.md."
                ),
            };
        }
    };

    // And a live tenant still derives, when there is one. The master key
    // loading proves the file is there; this proves the whole path still
    // produces a key for somebody who has not been deleted.
    let control = dumpster_kit_keys::state::list(registry)
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.state == dumpster_kit_keys::KeyState::Active && r.database_id != subject);
    match control {
        None => Proof {
            check: Check::Machinery,
            held: true,
            detail: format!(
                "master key {fingerprint} is present and usable; no other active tenant on \
                 this box to derive as a control"
            ),
        },
        Some(row) => match dumpster_kit_keys::tenant_key(env, registry, &row.database_id) {
            Ok(key) => Proof {
                check: Check::Machinery,
                held: true,
                detail: format!(
                    "master key {fingerprint} is present, and {} still derives to {} — so a \
                     refusal below is about {subject} and not about this box",
                    row.database_id,
                    key.fingerprint()
                ),
            },
            Err(e) => Proof {
                check: Check::Machinery,
                held: false,
                detail: format!(
                    "the master key is present but {} — an active tenant — does not derive \
                     ({e}). Something is wrong with this box, and no conclusion about \
                     {subject} can be drawn from it.",
                    row.database_id
                ),
            },
        },
    }
}

/// The key path: derivation must refuse, and refuse *deliberately*.
fn derivation(env: &KeyEnv, registry: &Connection, database_id: &str) -> Proof {
    match dumpster_kit_keys::tenant_key(env, registry, database_id) {
        Ok(key) => Proof {
            check: Check::Derivation,
            held: false,
            detail: format!(
                "this tenant's key STILL DERIVES ({}). Nothing is crypto-shredded: any copy \
                 of any object of theirs, anywhere, is readable.",
                key.fingerprint()
            ),
        },
        Err(e) if e.is_deliberate_revocation() => Proof {
            check: Check::Derivation,
            held: true,
            detail: format!("refused as a deliberate revocation: {}", first_line(&e)),
        },
        // The distinction the whole of item 3 is built to preserve. An id
        // nobody registered, or a box with no key, refuses too — and neither
        // is a tombstone. Accepting them here would let "we never heard of
        // them" be filed as "we destroyed their data".
        Err(e) => Proof {
            check: Check::Derivation,
            held: false,
            detail: format!(
                "derivation failed, but NOT as a revocation: {}. A tombstone is what makes a \
                 deletion durable — it survives a restore, and it is the same answer on every \
                 box. This is not that; record one with `{} keys tombstone {database_id} \
                 --yes`.",
                first_line(&e),
                env.label()
            ),
        },
    }
}

/// A copy taken before the deletion, proven unopenable now.
fn stray_copy(
    env: &KeyEnv,
    registry: &Connection,
    database_id: &str,
    stray: &StrayCopy,
    machinery_ok: bool,
) -> Proof {
    let fail = |detail: String| Proof {
        check: Check::StrayCopy,
        held: false,
        detail,
    };

    // A copy that is not a sealed object proves nothing by failing to open.
    if !stray.bytes.starts_with(dumpster_kit_ship::cipher::MAGIC) {
        return fail(format!(
            "{} is not a sealed tenant-zone object — it does not carry the envelope magic. A \
             file that was never encrypted does not open either, so nothing can be concluded \
             from this one.",
            stray.object_key
        ));
    }
    if !machinery_ok {
        return fail(format!(
            "not established: this box cannot derive anybody's key, so 'no key opens {}' is \
             true of every tenant and says nothing about {database_id}.",
            stray.object_key
        ));
    }

    match dumpster_kit_keys::tenant_key(env, registry, database_id) {
        // The red direction, and it is a real attempt rather than an
        // inference: the key exists, so the copy is opened, and if it opens
        // then this tenant's old bytes are readable wherever they survived.
        Ok(key) => match dumpster_kit_ship::cipher::open(&key, &stray.object_key, &stray.bytes) {
            Ok(plaintext) => fail(format!(
                "the copy of {} OPENED, to {} bytes of Parquet. A copy that survived the \
                 partition drop is readable, which is exactly what crypto-shredding exists to \
                 prevent.",
                stray.object_key,
                plaintext.len()
            )),
            Err(e) => fail(format!(
                "this tenant's key still derives, so the copy is not shredded — though this \
                 particular copy did not open ({e}). Deletion is not proven while a key \
                 exists.",
            )),
        },
        Err(e) if e.is_deliberate_revocation() => Proof {
            check: Check::StrayCopy,
            held: true,
            detail: format!(
                "no key can be derived for {database_id}, so the {} bytes of {} cannot be \
                 opened by this system — wherever else that object survived, it survived as \
                 ciphertext nobody holds a key for.",
                stray.bytes.len(),
                stray.object_key
            ),
        },
        Err(e) => fail(format!(
            "not established: derivation failed without being a revocation ({}), so the copy \
             is unopened for the wrong reason.",
            first_line(&e)
        )),
    }
}

/// This crate's errors are long on purpose; a proof line is not the place for
/// all of it.
fn first_line(e: &impl std::fmt::Display) -> String {
    let text = e.to_string();
    text.lines().next().unwrap_or(&text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{A, B, dir_zone, env, registry, seal_into, seed};

    /// The green direction, whole: tombstoned and swept, every path closed.
    #[test]
    fn a_deleted_tenant_proves_unreadable_on_every_path() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings", "valuations"]);
        let stray = seal_into(&world, A, &store, &config);

        Sweep::new(&store, &config, A)
            .unwrap()
            .drop_partition()
            .unwrap();
        dumpster_kit_keys::destroy::tombstone(&world, A, Some("account deleted")).unwrap();

        let verdict = verify(&env(), &store, &config, &world, A, Some(&stray)).unwrap();
        assert!(
            verdict.proven(),
            "not proven: {:?}",
            verdict
                .failures()
                .iter()
                .map(|p| &p.detail)
                .collect::<Vec<_>>()
        );
        // Every path the bead names, by name.
        for want in [
            Check::Machinery,
            Check::Derivation,
            Check::Partition,
            Check::Dataset("holdings"),
            Check::Dataset("valuations"),
            Check::StrayCopy,
        ] {
            assert!(
                verdict.proofs.iter().any(|p| p.check == want),
                "{want} was never checked"
            );
        }
    }

    /// SEEN RED. The same call against a live tenant must report every path
    /// OPEN — and must actually open the stray copy rather than infer it.
    #[test]
    fn a_live_tenant_is_reported_not_proven_and_the_stray_copy_opens() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings"]);
        let stray = seal_into(&world, A, &store, &config);

        let verdict = verify(&env(), &store, &config, &world, A, Some(&stray)).unwrap();
        assert!(
            !verdict.proven(),
            "a live tenant must not verify as deleted"
        );

        let by = |c: Check| {
            verdict
                .proofs
                .iter()
                .find(|p| p.check == c)
                .unwrap_or_else(|| panic!("{c} was not checked"))
        };
        assert!(by(Check::Machinery).held, "the box itself is fine");
        assert!(!by(Check::Derivation).held);
        assert!(by(Check::Derivation).detail.contains("STILL DERIVES"));
        assert!(!by(Check::Partition).held);
        assert!(!by(Check::StrayCopy).held);
        assert!(
            by(Check::StrayCopy).detail.contains("OPENED"),
            "the red direction must be an attempt, not an inference: {}",
            by(Check::StrayCopy).detail
        );
        assert!(verdict.into_result().is_err());
    }

    /// The tombstone alone is not the deletion. A swept-but-not-tombstoned
    /// tenant fails on derivation and on the stray copy, and the message says
    /// what to do about it.
    #[test]
    fn dropping_the_partition_without_a_tombstone_is_not_proven() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings", "valuations"]);
        let stray = seal_into(&world, A, &store, &config);

        Sweep::new(&store, &config, A)
            .unwrap()
            .drop_partition()
            .unwrap();

        let verdict = verify(&env(), &store, &config, &world, A, Some(&stray)).unwrap();
        assert!(!verdict.proven());
        let derivation = verdict
            .proofs
            .iter()
            .find(|p| p.check == Check::Derivation)
            .unwrap();
        assert!(!derivation.held);
        assert!(
            derivation.detail.contains("STILL DERIVES"),
            "{derivation:?}"
        );
    }

    /// …and the mirror: tombstoned but not swept. The bytes are ciphertext
    /// nobody can open, and it is STILL not a deletion, because the objects
    /// are there and the design says the drop is the erasure.
    #[test]
    fn a_tombstone_without_the_drop_is_not_proven_either() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings"]);

        dumpster_kit_keys::destroy::tombstone(&world, A, None).unwrap();

        let verdict = verify(&env(), &store, &config, &world, A, None).unwrap();
        assert!(!verdict.proven());
        let partition = verdict
            .proofs
            .iter()
            .find(|p| p.check == Check::Partition)
            .unwrap();
        assert!(!partition.held);
        assert!(partition.detail.contains("still holds 2 object(s)"));
    }

    /// The vacuity guard. With no master key on the box, nothing derives for
    /// anybody — and the verification must say so rather than congratulate
    /// itself. `Derivation` still holds, because a tombstone is a row and
    /// answers without the key; `Machinery` and `StrayCopy` do not.
    #[test]
    fn a_box_with_no_master_key_cannot_prove_a_stray_copy_unreadable() {
        let (tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        seed(&store, A, &["holdings"]);
        let stray = seal_into(&world, A, &store, &config);

        Sweep::new(&store, &config, A)
            .unwrap()
            .drop_partition()
            .unwrap();
        dumpster_kit_keys::destroy::tombstone(&world, A, None).unwrap();
        std::fs::remove_file(tmp.path().join("tenant-master.key")).unwrap();

        let verdict = verify(&env(), &store, &config, &world, A, Some(&stray)).unwrap();
        let by = |c: Check| verdict.proofs.iter().find(|p| p.check == c).unwrap();

        assert!(
            by(Check::Derivation).held,
            "a tombstone is a row and answers with the key gone — that is item 3's whole point"
        );
        assert!(!by(Check::Machinery).held);
        assert!(
            !by(Check::StrayCopy).held,
            "'no key opens it' is true of everybody on a keyless box and proves nothing"
        );
        assert!(by(Check::StrayCopy).detail.contains("not established"));
        assert!(!verdict.proven());
    }

    /// An unregistered id refuses derivation too, and it is NOT a revocation.
    /// Accepting it would let "never heard of them" be filed as "destroyed".
    #[test]
    fn an_unregistered_id_does_not_count_as_a_revocation() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[B]); // A was never registered
        let verdict = verify(&env(), &store, &config, &world, A, None).unwrap();

        let derivation = verdict
            .proofs
            .iter()
            .find(|p| p.check == Check::Derivation)
            .unwrap();
        assert!(!derivation.held, "absence is not a deletion");
        assert!(derivation.detail.contains("NOT as a revocation"));
        assert!(derivation.detail.contains("dumpstar keys tombstone"));
    }

    /// A "stray copy" that was never encrypted must not be able to prove
    /// anything by failing to open.
    #[test]
    fn something_that_is_not_a_sealed_object_proves_nothing() {
        let (_tmp, store, config) = dir_zone();
        let world = registry(&[A, B]);
        dumpster_kit_keys::destroy::tombstone(&world, A, None).unwrap();

        let stray = StrayCopy {
            object_key: format!(
                "tenant/database_id={A}/dataset=holdings/as_of=2026-08-14/part-0000.parquet.enc"
            ),
            bytes: b"PAR1 a bare parquet file".to_vec(),
        };
        let verdict = verify(&env(), &store, &config, &world, A, Some(&stray)).unwrap();
        let copy = verdict
            .proofs
            .iter()
            .find(|p| p.check == Check::StrayCopy)
            .unwrap();
        assert!(!copy.held);
        assert!(copy.detail.contains("envelope magic"), "{copy:?}");
    }

    /// A copy that is not there is not a copy that failed to decrypt.
    #[test]
    fn a_missing_stray_copy_is_an_error_not_a_pass() {
        let err =
            StrayCopy::read("tenant/x", std::path::Path::new("/nonexistent/copy.enc")).unwrap_err();
        assert!(matches!(err, EraseError::NoStrayCopy { .. }), "{err}");
    }
}
