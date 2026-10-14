//! Per-tenant keys, **derived rather than stored**.
//!
//! ```text
//! tenant key = HKDF-SHA256(
//!     salt = "dumpstar/tenant-zone/v1",
//!     ikm  = <the master key>,
//!     info = "dumpstar/tenant-key/v1/<database_id>",
//!     len  = 32,
//! )
//! ```
//!
//! Nothing per-tenant is written down, so there is no key service to run, no
//! per-tenant secret to rotate, and nothing to lose one tenant at a time. The
//! `database_id` is the whole of the per-tenant input, which is what makes a
//! key reproducible from the master key and a directory listing.
//!
//! ## The order of the two checks is the design
//!
//! [`tenant_key`] consults the **key-state registry first** and the master
//! key second. Four outcomes, and they are four:
//!
//! | key state    | master key | result                                     |
//! |--------------|-----------|---------------------------------------------|
//! | tombstoned   | present   | [`KeyError::Tombstoned`]                     |
//! | tombstoned   | MISSING   | [`KeyError::Tombstoned`] — still             |
//! | active       | missing   | [`KeyError::MasterKeyUnavailable`]           |
//! | unregistered | either    | [`KeyError::NotRegistered`]                  |
//!
//! Row two is the one that matters. A deliberate revocation is answerable
//! **without the master key**, so a box that has lost its key still reports a
//! deleted tenant as deleted rather than as broken. Row three is its mirror:
//! a live tenant whose key is missing reports as broken and never as deleted.
//! Reverse the order and the two collapse into each other exactly when
//! somebody is trying to tell them apart.
//!
//! ## Future option, deliberately not built
//!
//! Stored per-tenant random keys are stronger against a master-key leak, and
//! moving to them costs a re-encrypt of at most 90 days of data — the tenant
//! zone's whole retention window. It is a real option, not a regret; it is
//! written down in `deploy/KEYS.md` and is not this item's work.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{KeyError, Result};
use crate::master::{self, KeyEnv, MASTER_KEY_LEN, MasterKey};
use crate::state::{self, KeyState};

/// Length of a derived per-tenant key, in bytes.
pub const TENANT_KEY_LEN: usize = 32;

/// HKDF's salt. A fixed, versioned domain separator — not a secret, and not
/// per-tenant (that is what `info` is for). Rooted at the app's label, so one
/// app's tenant keys are never another's even under a shared master key.
fn salt(label: &str) -> Vec<u8> {
    format!("{label}/tenant-zone/v1").into_bytes()
}

/// One tenant's key, wiped on drop.
///
/// No `Display`, and a `Debug` that redacts — see [`crate::master::MasterKey`].
/// What comes out of it is the material (for the thing doing the encrypting)
/// or a [`TenantKey::fingerprint`], which is what a log line, a CLI and a
/// test get to see.
pub struct TenantKey {
    label: String,
    database_id: String,
    bytes: Zeroizing<[u8; TENANT_KEY_LEN]>,
}

impl std::fmt::Debug for TenantKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantKey")
            .field("database_id", &self.database_id)
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

impl TenantKey {
    /// The app label the key was derived under.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The database this key belongs to.
    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    /// The raw key material.
    pub fn as_bytes(&self) -> &[u8; TENANT_KEY_LEN] {
        &self.bytes
    }

    /// A short, non-invertible identifier for this key.
    ///
    /// Determinism is a claim about this value: same master key and same
    /// `database_id` give the same fingerprint, and two `database_id`s do
    /// not. It is safe to print, which is what lets the claim be checked on a
    /// real box without the key ever reaching a terminal.
    pub fn fingerprint(&self) -> String {
        master::fingerprint_of(&self.label, self.bytes.as_slice())
    }
}

/// Derive `database_id`'s key, subject to the key-state registry.
///
/// The registry is consulted **first** — see the module docs for why that
/// ordering is load-bearing rather than incidental.
pub fn tenant_key(
    env: &KeyEnv,
    conn: &rusqlite::Connection,
    database_id: &str,
) -> Result<TenantKey> {
    match state::find(conn, database_id)? {
        None => Err(KeyError::NotRegistered {
            database_id: database_id.to_string(),
        }),
        Some(row) if row.state == KeyState::Tombstoned => Err(KeyError::Tombstoned {
            database_id: row.database_id,
            tombstoned_at: row
                .tombstoned_at
                .unwrap_or_else(|| "an unrecorded time".to_string()),
            reason: row.reason,
        }),
        Some(_) => {
            let master = master::load(env)?;
            Ok(from_master(&master, database_id))
        }
    }
}

/// The derivation itself, with no registry in the way.
///
/// Pure: a master key and a `database_id` in, 32 bytes out, the same 32 bytes
/// every time. Separated from [`tenant_key`] so the *maths* can be tested
/// without a database and the *policy* can be tested without a key file — and
/// so it is obvious that the registry is what refuses, never the arithmetic.
///
/// Deliberately **not public**: a caller that could reach this could derive a
/// key for a tombstoned tenant, which is the one thing the tombstone exists
/// to stop. Everything outside this crate goes through [`tenant_key`].
pub(crate) fn from_master(master: &MasterKey, database_id: &str) -> TenantKey {
    let hk = Hkdf::<Sha256>::new(Some(&salt(master.label())), master.as_bytes());
    let info = format!("{}/tenant-key/v1/{database_id}", master.label());
    let mut bytes = Zeroizing::new([0u8; TENANT_KEY_LEN]);
    hk.expand(info.as_bytes(), bytes.as_mut_slice())
        .expect("32 bytes is far below HKDF-SHA256's 255*32 output limit");
    TenantKey {
        label: master.label().to_string(),
        database_id: database_id.to_string(),
        bytes,
    }
}

/// The master key's own fingerprint, for `dumpstar keys status`.
///
/// Here rather than in [`crate::backup`] on purpose: asking "which key is on
/// this box" is not backing it up, and must not travel the path that emits
/// key material.
pub fn master_fingerprint(env: &KeyEnv) -> Result<String> {
    Ok(master::load(env)?.fingerprint())
}

const _: () = assert!(MASTER_KEY_LEN == 32, "HKDF ikm length assumption");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::master::create_at;
    use crate::state::tests::registry;
    use crate::test_support::EnvGuard;
    use crate::test_support::env;

    const A: &str = "01J0000000000000000000000A";
    const B: &str = "01J0000000000000000000000B";

    fn a_master(dir: &std::path::Path, name: &str) -> MasterKey {
        let path = dir.join(name);
        create_at(&env(), &path).unwrap();
        master::load_from(&env(), &path).unwrap()
    }

    /// The cutover guarantee: a site named `DUMPSTAR` derives exactly the bytes
    /// the crate's HKDF labels spelled before the labels were parameterised, so
    /// moving a site onto the kit makes no already-shipped part unreadable.
    #[test]
    fn the_dumpstar_labels_derive_the_pre_extraction_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let m = a_master(tmp.path(), "m.key");
        let mut expected = [0u8; TENANT_KEY_LEN];
        Hkdf::<Sha256>::new(Some(b"dumpstar/tenant-zone/v1"), m.as_bytes())
            .expand(
                format!("dumpstar/tenant-key/v1/{A}").as_bytes(),
                &mut expected,
            )
            .unwrap();
        assert_eq!(from_master(&m, A).as_bytes(), &expected);
    }

    /// Two apps that share a master key file by accident still do not share
    /// tenant keys.
    #[test]
    fn two_apps_derive_different_keys_from_one_master() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m.key");
        create_at(&env(), &path).unwrap();
        let other = KeyEnv::new(&dumpster_kit_tenancy::AppPrefix::new("PKDUMP").unwrap());
        let mine = master::load_from(&env(), &path).unwrap();
        let theirs = {
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("dumpstar-", "pkdump-");
            let p2 = tmp.path().join("m2.key");
            crate::master::write_600(&p2, text.as_bytes()).unwrap();
            master::load_from(&other, &p2).unwrap()
        };
        assert_eq!(mine.as_bytes(), theirs.as_bytes());
        assert_ne!(
            from_master(&mine, A).as_bytes(),
            from_master(&theirs, A).as_bytes()
        );
    }

    /// Same master key, same id — the same key, always.
    #[test]
    fn derivation_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let m = a_master(tmp.path(), "m.key");
        let first = from_master(&m, A);
        for _ in 0..8 {
            assert_eq!(
                from_master(&m, A).as_bytes(),
                first.as_bytes(),
                "the same master key and database_id must always give the same key"
            );
        }
        // …and reloading the master key from disk changes nothing.
        let reloaded = master::load_from(&env(), &tmp.path().join("m.key")).unwrap();
        assert_eq!(from_master(&reloaded, A).as_bytes(), first.as_bytes());
    }

    /// Different ids, different keys — checked over a set big enough that a
    /// collision would be a bug rather than luck.
    #[test]
    fn different_database_ids_give_different_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let m = a_master(tmp.path(), "m.key");

        let alphabet: Vec<char> = "0123456789ABCDEFGHJKMNPQRSTVWXYZ".chars().collect();
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        for (i, c) in alphabet.iter().enumerate() {
            for (j, d) in alphabet.iter().enumerate() {
                if (i + j) % 7 != 0 {
                    continue;
                }
                let id = format!("01J00000000000000000000{c}{d}0");
                dumpster_kit_tenancy::validate_database_id(&id).unwrap();
                ids.push(id);
            }
        }
        assert!(ids.len() > 100, "want a real set, got {}", ids.len());

        for id in &ids {
            let key = from_master(&m, id);
            assert!(
                seen.insert(*key.as_bytes()),
                "two database_ids derived the same key — {id}"
            );
        }
        assert_eq!(seen.len(), ids.len());
    }

    /// A different master key gives different tenant keys for the same id.
    /// This is what makes the master key load-bearing rather than decorative.
    #[test]
    fn a_different_master_gives_different_tenant_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let one = a_master(tmp.path(), "one.key");
        let two = a_master(tmp.path(), "two.key");
        assert_ne!(
            from_master(&one, A).as_bytes(),
            from_master(&two, A).as_bytes()
        );
    }

    /// The tenant key is not the master key, and is not a slice of it.
    #[test]
    fn a_tenant_key_is_not_the_master_key() {
        let tmp = tempfile::tempdir().unwrap();
        let m = a_master(tmp.path(), "m.key");
        let k = from_master(&m, A);
        assert_ne!(k.as_bytes().as_slice(), m.as_bytes().as_slice());
        assert_ne!(k.fingerprint(), m.fingerprint());
        assert_eq!(k.database_id(), A);
    }

    /// RFC 5869 test case 1, so "we used HKDF-SHA256" is a checked claim
    /// about this build's dependency rather than a sentence in a doc comment.
    #[test]
    fn hkdf_sha256_matches_rfc_5869() {
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0x00u8..=0x0c).collect();
        let info: Vec<u8> = (0xf0u8..=0xf9).collect();
        let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
        let mut okm = [0u8; 42];
        hk.expand(&info, &mut okm).unwrap();
        assert_eq!(
            hex::encode(okm),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
    }

    // ── the ordering that keeps "lost" and "destroyed" apart ────────────────

    #[test]
    fn an_unregistered_id_refuses_before_anything_is_derived() {
        let conn = registry();
        let err = tenant_key(&env(), &conn, A).unwrap_err();
        assert!(matches!(err, KeyError::NotRegistered { .. }), "{err}");
        assert!(!err.is_deliberate_revocation());
    }

    #[test]
    fn a_tombstoned_id_refuses_even_with_a_healthy_master_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m.key");
        create_at(&env(), &path).unwrap();
        let _guard = EnvGuard::set(&path);

        let conn = registry();
        state::register(&conn, A).unwrap();
        // A live tenant derives fine…
        let before = tenant_key(&env(), &conn, A).unwrap().fingerprint();
        assert!(!before.is_empty());

        crate::destroy::tombstone(&conn, A, Some("account deleted")).unwrap();

        let err = tenant_key(&env(), &conn, A).unwrap_err();
        assert!(err.is_deliberate_revocation(), "{err}");
        assert!(err.to_string().contains("account deleted"), "{err}");
        // The master key is untouched — the tombstone is what refuses.
        assert!(path.exists());
        assert!(
            !tenant_key(&env(), &conn, B)
                .unwrap_err()
                .is_deliberate_revocation()
        );
    }

    /// Row two of the table: revocation is answerable with no master key at
    /// all. This is the property that keeps a broken box from reporting live
    /// tenants as deleted, and deleted ones as merely broken.
    #[test]
    fn a_tombstone_answers_even_when_the_master_key_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m.key");
        create_at(&env(), &path).unwrap();
        let _guard = EnvGuard::set(&path);

        let conn = registry();
        state::register(&conn, A).unwrap();
        state::register(&conn, B).unwrap();
        crate::destroy::tombstone(&conn, A, Some("account deleted")).unwrap();

        std::fs::remove_file(&path).unwrap();

        let revoked = tenant_key(&env(), &conn, A).unwrap_err();
        assert!(
            revoked.is_deliberate_revocation(),
            "a revoked tenant must still read as revoked with the key gone: {revoked}"
        );

        let broken = tenant_key(&env(), &conn, B).unwrap_err();
        assert!(
            broken.is_operational_failure() && !broken.is_deliberate_revocation(),
            "a live tenant whose key is missing must read as broken, never deleted: {broken}"
        );
        assert!(matches!(broken, KeyError::MasterKeyUnavailable { .. }));
    }
}
