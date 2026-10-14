//! **THE BACKUP PATH.** One of two, and the other one is [`crate::destroy`].
//!
//! Read this module and `destroy.rs` together — they are deliberately built
//! so that nothing joins them:
//!
//! | | backup (here) | destruction ([`crate::destroy`]) |
//! |---|---|---|
//! | object | the master key **file** | a row in the key-state **registry** |
//! | verb | copy it out / put it back | record a tombstone |
//! | scope | every tenant at once | exactly one `database_id` |
//! | reach | never opens the registry | never opens the key file |
//!
//! That last row is not a description of today's code, it is a rule, and
//! `tests/separation.rs` reads both files and fails if either one grows a
//! reference to the other's world. The reason is the property this whole item
//! turns on: **a lost key is indistinguishable from a deleted tenant.** If
//! backup and destruction share a mechanism, then a backup that failed starts
//! looking like a deletion that succeeded (data loss wearing compliance as a
//! costume), or a deletion gets implemented as "the backup went missing",
//! which revokes nothing the moment the backup turns up. Sharing a helper is
//! how that happens — not sharing a name.
//!
//! ## The mechanism is matched, not invented
//!
//! The master key is backed up **exactly the way the Litestream bootstrap
//! credential already is**: the operator holds a copy in their password
//! manager, and `deploy/RESTORE.md` Scenario C is where it is pasted back in
//! after a total box loss. There is no automated replication of it, on
//! purpose — the obvious place to replicate it to is S3, and S3 is where the
//! data it protects lives. A key stored beside its ciphertext is a key that
//! protects nothing.
//!
//! So this module's whole job is to hand the operator the bytes, in the
//! file's own format, and to take them back. Emitting a secret is an explicit
//! act at the CLI (the site's `keys backup --yes`), never a side effect.

use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::error::{KeyError, Result};
use crate::master::{self, KeyEnv};

/// The master key file's exact contents, for the operator to store.
///
/// Byte-for-byte what [`restore_to`] accepts, so a round trip through a
/// password manager is a copy rather than a re-encoding.
///
/// **This is secret material.** It is returned in a [`Zeroizing`] wrapper so
/// the copy this process made does not linger, and the caller is the one
/// deciding to put it on a screen.
pub fn export(env: &KeyEnv) -> Result<Zeroizing<String>> {
    let path = env.key_path().ok_or_else(|| env.nowhere_to_look())?;
    export_from(env, &path)
}

/// [`export`], from an explicit key file.
pub fn export_from(env: &KeyEnv, path: &Path) -> Result<Zeroizing<String>> {
    Ok(master::load_from(env, path)?.encode(env))
}

/// Write a mode-600 copy of the master key to `dest`.
///
/// For an operator moving a key onto removable media or into a staging file
/// on the way to a password manager. Refuses to overwrite: a backup that
/// silently replaced an older backup would be a way to lose a key while
/// believing you had kept two.
pub fn export_to_file(env: &KeyEnv, dest: &Path) -> Result<PathBuf> {
    if dest.exists() {
        return Err(KeyError::MasterKeyExists {
            path: dest.to_path_buf(),
        });
    }
    let material = export(env)?;
    master::write_600(dest, material.as_bytes())?;
    Ok(dest.to_path_buf())
}

/// Put a backed-up master key back, at [`KeyEnv::key_path`].
///
/// The inverse of [`export`] and the second half of Scenario C. It refuses if
/// a key is already there — restoring over a live key would destroy every
/// tenant's derived key at once, and there is no undo for it.
///
/// Returns the restored key's fingerprint, which is how an operator confirms
/// they pasted the right one.
pub fn restore(env: &KeyEnv, material: &str) -> Result<(PathBuf, String)> {
    let path = env.key_path().ok_or_else(|| env.nowhere_to_look())?;
    restore_to(env, &path, material)
}

/// [`restore`], to an explicit path.
pub fn restore_to(env: &KeyEnv, path: &Path, material: &str) -> Result<(PathBuf, String)> {
    if path.exists() {
        return Err(KeyError::MasterKeyExists {
            path: path.to_path_buf(),
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Written to a temporary neighbour and validated by loading it back
    // BEFORE it becomes the key file: a paste that lost a character must fail
    // as a bad paste, not as a box whose key silently derives the wrong
    // thing for every tenant from now on.
    let staging = path.with_extension("restoring");
    let _ = std::fs::remove_file(&staging);
    master::write_600(&staging, material.as_bytes())?;
    let fingerprint = match master::load_from(env, &staging) {
        Ok(key) => key.fingerprint(),
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            return Err(e);
        }
    };
    std::fs::rename(&staging, path)?;
    Ok((path.to_path_buf(), fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::master::create_at;
    use crate::test_support::env;

    #[test]
    fn a_backup_round_trips_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("tenant-master.key");
        let (_, fingerprint) = create_at(&env(), &original).unwrap();

        let material = export_from(&env(), &original).unwrap();
        assert!(material.contains(&env().format_tag()));
        assert_eq!(
            material.as_str(),
            std::fs::read_to_string(&original).unwrap()
        );

        let restored_path = tmp.path().join("restored").join("tenant-master.key");
        let (written, restored_fp) = restore_to(&env(), &restored_path, &material).unwrap();
        assert_eq!(written, restored_path);
        assert_eq!(restored_fp, fingerprint, "a restore must give back THE key");
        assert_eq!(master::mode_of(&restored_path).unwrap(), 0o600);
    }

    /// A restored key derives the same tenant keys as the original. This is
    /// what a backup is FOR — the fingerprint matching is the check, and this
    /// is the thing the check stands in for.
    #[test]
    fn a_restored_key_derives_the_same_tenant_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("a.key");
        create_at(&env(), &original).unwrap();
        let material = export_from(&env(), &original).unwrap();
        restore_to(&env(), &tmp.path().join("b.key"), &material).unwrap();

        let id = "01J0000000000000000000000A";
        let before = crate::derive::from_master(&master::load_from(&env(), &original).unwrap(), id);
        let after = crate::derive::from_master(
            &master::load_from(&env(), &tmp.path().join("b.key")).unwrap(),
            id,
        );
        assert_eq!(before.as_bytes(), after.as_bytes());
    }

    #[test]
    fn a_restore_never_lands_over_a_live_key() {
        let tmp = tempfile::tempdir().unwrap();
        let live = tmp.path().join("tenant-master.key");
        let (_, fingerprint) = create_at(&env(), &live).unwrap();
        let other = tmp.path().join("other.key");
        create_at(&env(), &other).unwrap();
        let material = export_from(&env(), &other).unwrap();

        let err = restore_to(&env(), &live, &material).unwrap_err();
        assert!(matches!(err, KeyError::MasterKeyExists { .. }), "{err}");
        assert_eq!(
            master::load_from(&env(), &live).unwrap().fingerprint(),
            fingerprint,
            "the live key must be exactly as it was"
        );
    }

    /// A truncated paste fails as a bad paste, and leaves nothing behind
    /// that a later load could mistake for a key.
    #[test]
    fn a_corrupt_backup_is_refused_and_leaves_no_key_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.key");
        create_at(&env(), &src).unwrap();
        let material = export_from(&env(), &src).unwrap();
        let truncated = &material[..material.len() - 5];

        let dest = tmp.path().join("restored.key");
        let err = restore_to(&env(), &dest, truncated).unwrap_err();
        assert!(matches!(err, KeyError::MasterKeyMalformed { .. }), "{err}");
        assert!(!dest.exists(), "a refused restore must leave no key file");
        assert!(
            !dest.with_extension("restoring").exists(),
            "…and no staging file either"
        );
    }

    #[test]
    fn export_to_file_writes_600_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("tenant-master.key");
        create_at(&env(), &key).unwrap();
        let _guard = crate::test_support::EnvGuard::set(&key);

        let dest = tmp.path().join("backup.key");
        export_to_file(&env(), &dest).unwrap();
        assert_eq!(master::mode_of(&dest).unwrap(), 0o600);
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            std::fs::read_to_string(&key).unwrap()
        );

        let err = export_to_file(&env(), &dest).unwrap_err();
        assert!(matches!(err, KeyError::MasterKeyExists { .. }), "{err}");
    }

    /// Backing up a key that is not there is an operational failure and says
    /// nothing about any tenant. The other half of this — that the failure
    /// leaves the destruction path working — is in `tests/separation.rs`.
    #[test]
    fn backing_up_a_missing_key_is_operational_not_a_revocation() {
        let tmp = tempfile::tempdir().unwrap();
        let err = export_from(&env(), &tmp.path().join("absent.key")).unwrap_err();
        assert!(err.is_operational_failure());
        assert!(!err.is_deliberate_revocation());
    }
}
