//! The master key: one file, on the box, mode 600.
//!
//! Everything the tenant zone encrypts is encrypted under a key derived from
//! this one. There is exactly one of them, and it is not a service — it is a
//! file, sitting in the same host-config directory the Litestream
//! credentials already live in, backed up by the same mechanism they are
//! (the operator's password manager; see `deploy/KEYS.md`). No key service
//! to run, no per-tenant secret to rotate or lose.
//!
//! The trade is stated rather than hidden: **one master key means destroying
//! it destroys everything.** That is why this module refuses to overwrite an
//! existing key, why creation is an explicit act (`dumpstar keys init`) and not
//! a side effect of `deploy/setup.sh`, and why nothing here is reachable from
//! the destruction path — revoking one tenant must never be able to touch the
//! file every other tenant depends on. See [`crate::destroy`].
//!
//! ## Where it lives
//!
//! `$DUMPSTAR_MASTER_KEY_FILE`, else `$HOME/.config/dumpstar/tenant-master.key`.
//!
//! The default is box-wide; the deploy scripts point the environment variable
//! at the **per-instance** file, `~/.config/dumpstar/<instance>/tenant-master.key`,
//! beside that instance's `litestream.env`. Per-instance because the key
//! *state* is per-instance — the tombstones live in that instance's
//! `registry.sqlite` — and a master key paired with the wrong registry is a
//! key that derives happily for a `database_id` somebody else revoked.
//!
//! ## The file format
//!
//! ```text
//! # Dumpstar tenant master key. Mode 600. Back this up — see deploy/KEYS.md.
//! dumpstar-master-key-v1:<64 lowercase hex characters>
//! ```
//!
//! Text, tagged and versioned, because the backup mechanism is a human
//! copy-pasting it into a password manager. A version tag now is what lets a
//! later rotation be *detected* rather than guessed at.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use dumpster_kit_tenancy::AppPrefix;

use crate::error::{KeyError, Result};

/// Length of the master key in bytes.
pub const MASTER_KEY_LEN: usize = 32;

/// The names and domain labels this crate uses, derived from one
/// [`AppPrefix`]. `DUMPSTAR` gives `DUMPSTAR_MASTER_KEY_FILE`,
/// `~/.config/dumpstar/tenant-master.key`, the `dumpstar-master-key-v1:` file
/// tag and the `dumpstar/…` HKDF labels. The label is the app's, so two sites
/// sharing a master key file by accident still derive different tenant keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEnv {
    label: String,
    /// Redirects the key file, so a test never reads — or writes — the
    /// operator's real one, and so the deploy scripts can point at the
    /// per-instance path.
    pub key_file_var: String,
    /// Default location of the key file, relative to `$HOME`.
    pub default_relative_path: String,
}

impl KeyEnv {
    /// The names for `app`.
    pub fn new(app: &AppPrefix) -> Self {
        let label = app.cli();
        Self {
            key_file_var: app.var("MASTER_KEY_FILE"),
            default_relative_path: format!(".config/{label}/tenant-master.key"),
            label,
        }
    }

    /// The app's lower-case name, the root of every HKDF and fingerprint label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The line prefix that tags the encoding. Versioned so a future rotation
    /// is a new tag rather than an ambiguous file.
    pub fn format_tag(&self) -> String {
        format!("{}-master-key-v1:", self.label)
    }

    /// The master key file this process would read.
    ///
    /// `None` only when there is no `$HOME` to resolve the default against and
    /// nothing set [`Self::key_file_var`].
    pub fn key_path(&self) -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var(&self.key_file_var) {
            return Some(PathBuf::from(explicit));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(&self.default_relative_path))
    }

    /// The same, or the error a caller should report if there is nowhere to look.
    pub(crate) fn key_path_or_err(&self) -> Result<PathBuf> {
        self.key_path().ok_or_else(|| self.nowhere_to_look())
    }

    pub(crate) fn nowhere_to_look(&self) -> KeyError {
        KeyError::MasterKeyUnavailable {
            path: PathBuf::from(format!("$HOME/{}", self.default_relative_path)),
            detail: format!("neither {} nor HOME is set", self.key_file_var),
        }
    }
}

/// 32 bytes of master key, held in memory and wiped on drop.
///
/// No `Display`, no `Serialize`, and a `Debug` that prints the fingerprint
/// instead of the key — a struct like this reaches a log through
/// `{:?}` on something that merely contains it, and that is not a mistake
/// worth leaving available. The only things that come out of it are a
/// [`MasterKey::fingerprint`] (non-invertible, safe to print) and — for the
/// two callers that genuinely need the bytes, derivation and backup —
/// [`MasterKey::as_bytes`].
pub struct MasterKey {
    label: String,
    bytes: Zeroizing<[u8; MASTER_KEY_LEN]>,
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MasterKey")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

impl MasterKey {
    /// The app label this key derives under.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The raw key material.
    pub fn as_bytes(&self) -> &[u8; MASTER_KEY_LEN] {
        &self.bytes
    }

    /// A short, non-invertible identifier for this key.
    ///
    /// Safe to print, log and compare. It is what makes "is the key on this
    /// box the same one as in the password manager" answerable without ever
    /// putting the key beside it on a screen.
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.label, self.bytes.as_slice())
    }

    /// The file's exact contents, for the backup path and nothing else.
    ///
    /// Kept here rather than in [`crate::backup`] because it is the file
    /// *format*, which belongs with the file. What [`crate::backup`] adds is
    /// the deliberate act of emitting it.
    pub(crate) fn encode(&self, env: &KeyEnv) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "# {} tenant master key. Mode 600. Back this up — see deploy/KEYS.md.\n\
             # Losing it is NOT the same as deleting a tenant: it makes every tenant's data\n\
             # unreadable at once, with nothing revoked and nobody deleted.\n\
             {}{}\n",
            capitalised(&self.label),
            env.format_tag(),
            hex::encode(self.bytes.as_slice())
        ))
    }
}

/// A short, non-invertible identifier for arbitrary key material.
///
/// Domain-separated so a tenant key's fingerprint can never collide with the
/// master key's by construction, and truncated to 16 hex characters — enough
/// to compare by eye, not enough to be anything else.
pub fn fingerprint_of(label: &str, material: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(format!("{label}/key-fingerprint/v1").as_bytes());
    h.update(material);
    hex::encode(&h.finalize()[..8])
}

fn capitalised(label: &str) -> String {
    let mut chars = label.chars();
    chars
        .next()
        .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

/// Generate a master key and write it, mode 600, at [`KeyEnv::key_path`].
///
/// **Refuses to overwrite.** Overwriting is the one irreversible act here —
/// it destroys every tenant's derived key at once — so it is never implicit
/// and there is no `--force`. Moving the old file aside is a decision an
/// operator makes with their own hands.
pub fn create(env: &KeyEnv) -> Result<(PathBuf, String)> {
    create_at(env, &env.key_path_or_err()?)
}

/// [`create`], at an explicit path.
pub fn create_at(env: &KeyEnv, path: &Path) -> Result<(PathBuf, String)> {
    if path.exists() {
        return Err(KeyError::MasterKeyExists {
            path: path.to_path_buf(),
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        restrict_dir(parent)?;
    }

    let mut bytes = Zeroizing::new([0u8; MASTER_KEY_LEN]);
    getrandom::fill(bytes.as_mut_slice()).map_err(|e| KeyError::MasterKeyUnavailable {
        path: path.to_path_buf(),
        detail: format!("the system CSPRNG refused: {e}"),
    })?;
    let key = MasterKey {
        label: env.label.clone(),
        bytes,
    };

    write_600(path, key.encode(env).as_bytes())?;
    Ok((path.to_path_buf(), key.fingerprint()))
}

/// Load the master key from [`KeyEnv::key_path`].
///
/// Every refusal here is an [`KeyError::is_operational_failure`], never a
/// revocation: a key that cannot be read says nothing whatsoever about
/// whether any tenant was deleted. See [`crate::error`].
pub fn load(env: &KeyEnv) -> Result<MasterKey> {
    load_from(env, &env.key_path_or_err()?)
}

/// [`load`], from an explicit path.
pub fn load_from(env: &KeyEnv, path: &Path) -> Result<MasterKey> {
    let text = std::fs::read_to_string(path).map_err(|e| KeyError::MasterKeyUnavailable {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    check_permissions(path)?;
    decode(env, path, &text)
}

/// Whether a key file is present at [`key_path`], without reading it.
pub fn exists(env: &KeyEnv) -> bool {
    env.key_path().map(|p| p.exists()).unwrap_or(false)
}

/// The file's permission bits (`0o600` and friends), for reporting.
pub fn mode_of(path: &Path) -> Result<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(0o600)
    }
}

/// Parse the file format. Comments and blank lines are skipped; exactly one
/// tagged key line must remain.
fn decode(env: &KeyEnv, path: &Path, text: &str) -> Result<MasterKey> {
    let tag = env.format_tag();
    let mut found: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(hexpart) = line.strip_prefix(tag.as_str()) else {
            return Err(KeyError::MasterKeyMalformed {
                path: path.to_path_buf(),
                detail: format!("expected a line starting {tag:?}"),
            });
        };
        if found.is_some() {
            return Err(KeyError::MasterKeyMalformed {
                path: path.to_path_buf(),
                detail: "more than one key line — which one is the key?".into(),
            });
        }
        found = Some(hexpart);
    }

    let Some(hexpart) = found else {
        return Err(KeyError::MasterKeyMalformed {
            path: path.to_path_buf(),
            detail: "the file holds no key line at all".into(),
        });
    };

    let raw = Zeroizing::new(
        hex::decode(hexpart).map_err(|e| KeyError::MasterKeyMalformed {
            path: path.to_path_buf(),
            detail: format!("the key is not hex: {e}"),
        })?,
    );
    if raw.len() != MASTER_KEY_LEN {
        return Err(KeyError::MasterKeyMalformed {
            path: path.to_path_buf(),
            detail: format!(
                "the key is {} bytes; a master key is {MASTER_KEY_LEN}",
                raw.len()
            ),
        });
    }
    let mut bytes = Zeroizing::new([0u8; MASTER_KEY_LEN]);
    bytes.copy_from_slice(&raw);
    Ok(MasterKey {
        label: env.label.clone(),
        bytes,
    })
}

/// Refuse a key file anyone but its owner can read.
///
/// Checked on every load, not only at creation: `chmod 644` after the fact is
/// exactly the mistake that leaves a key readable for months, and the code
/// that wrote the file is long gone by then. The container gate
/// (`tests/keys/run.sh`) asserts the mode on the *deployed* file for the same
/// reason — "the code sets 600" and "the file is 600" are different claims.
fn check_permissions(path: &Path) -> Result<()> {
    let mode = mode_of(path)?;
    if mode & 0o077 != 0 {
        return Err(KeyError::MasterKeyPermissions {
            path: path.to_path_buf(),
            found: mode,
        });
    }
    Ok(())
}

/// Write bytes to a brand-new file at mode 600, never widening an existing
/// one and never leaving a world-readable moment in between.
pub(crate) fn write_600(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Hold the containing directory to 700 — the mode `deploy/setup.sh` already
/// puts `~/.config/dumpstar/<instance>/` at for the Litestream credentials.
fn restrict_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dir)?.permissions();
        if perms.mode() & 0o077 != 0 {
            perms.set_mode(0o700);
            std::fs::set_permissions(dir, perms)?;
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env;

    #[test]
    fn a_created_key_round_trips_and_is_mode_600() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub").join("tenant-master.key");

        let (written, fingerprint) = create_at(&env(), &path).unwrap();
        assert_eq!(written, path);
        assert_eq!(mode_of(&path).unwrap(), 0o600, "the key file must be 600");
        assert_eq!(mode_of(path.parent().unwrap()).unwrap(), 0o700);

        let key = load_from(&env(), &path).unwrap();
        assert_eq!(key.fingerprint(), fingerprint);
        assert_eq!(key.as_bytes().len(), MASTER_KEY_LEN);
    }

    #[test]
    fn two_created_keys_differ() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, a) = create_at(&env(), &tmp.path().join("a.key")).unwrap();
        let (_, b) = create_at(&env(), &tmp.path().join("b.key")).unwrap();
        assert_ne!(a, b, "the CSPRNG must not be handing out one key");
    }

    /// The one irreversible act in the crate is never implicit.
    #[test]
    fn creation_refuses_to_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tenant-master.key");
        let (_, first) = create_at(&env(), &path).unwrap();

        let err = create_at(&env(), &path).unwrap_err();
        assert!(matches!(err, KeyError::MasterKeyExists { .. }), "{err}");
        assert!(err.to_string().contains("destroys EVERY tenant"), "{err}");
        assert_eq!(
            load_from(&env(), &path).unwrap().fingerprint(),
            first,
            "a refused create must leave the existing key exactly as it was"
        );
    }

    #[test]
    fn a_widened_key_file_is_refused_on_load() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tenant-master.key");
        create_at(&env(), &path).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = load_from(&env(), &path).unwrap_err();
        assert!(
            matches!(err, KeyError::MasterKeyPermissions { found: 0o644, .. }),
            "{err}"
        );
        // Wrong permissions are an operational problem, not a revocation.
        assert!(!err.is_deliberate_revocation());
        assert!(err.is_operational_failure());
    }

    #[test]
    fn a_missing_key_is_operational_not_a_revocation() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load_from(&env(), &tmp.path().join("absent.key")).unwrap_err();
        assert!(
            matches!(err, KeyError::MasterKeyUnavailable { .. }),
            "{err}"
        );
        assert!(!err.is_deliberate_revocation());
    }

    #[test]
    fn malformed_files_are_refused_specifically() {
        let tmp = tempfile::tempdir().unwrap();

        let cases = [
            ("# only a comment\n", "no key line"),
            ("deadbeef\n", "expected a line"),
            (
                &format!("{}not-hex-at-all\n", env().format_tag()),
                "is not hex",
            ),
            (
                &format!("{}{}\n", env().format_tag(), hex::encode([0u8; 16])),
                "bytes",
            ),
            (
                &format!(
                    "{tag}{a}\n{tag}{a}\n",
                    tag = env().format_tag(),
                    a = hex::encode([7u8; 32])
                ),
                "more than one key line",
            ),
        ];
        for (i, (text, expect)) in cases.iter().enumerate() {
            let path = tmp.path().join(format!("bad{i}.key"));
            write_600(&path, text.as_bytes()).unwrap();
            let err = load_from(&env(), &path).unwrap_err();
            assert!(
                matches!(err, KeyError::MasterKeyMalformed { .. }),
                "case {i}: {err}"
            );
            assert!(err.to_string().contains(expect), "case {i}: {err}");
            assert!(!err.is_deliberate_revocation(), "case {i}");
        }
    }

    #[test]
    fn the_env_var_beats_the_default() {
        // Not run in parallel with anything else touching this var: the
        // whole test is one set/read/unset, and `key_path` reads it once.
        let previous = std::env::var(&env().key_file_var).ok();
        unsafe { std::env::set_var(&env().key_file_var, "/tmp/somewhere/else.key") };
        assert_eq!(
            env().key_path().unwrap(),
            PathBuf::from("/tmp/somewhere/else.key")
        );
        match previous {
            Some(v) => unsafe { std::env::set_var(&env().key_file_var, v) },
            None => unsafe { std::env::remove_var(&env().key_file_var) },
        }
    }

    /// The fingerprint is an identifier, not an encoding of the key.
    #[test]
    fn a_fingerprint_reveals_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("k.key");
        create_at(&env(), &path).unwrap();
        let key = load_from(&env(), &path).unwrap();

        let fp = key.fingerprint();
        assert_eq!(fp.len(), 16);
        assert!(
            !hex::encode(key.as_bytes()).contains(&fp),
            "the fingerprint must not be a substring of the key"
        );
        assert_eq!(fp, fingerprint_of("dumpstar", key.as_bytes()));
        let bare = hex::encode(&Sha256::digest(key.as_bytes())[..8]);
        assert_ne!(
            fp, bare,
            "…and it must be domain-separated from a bare digest of the same bytes"
        );
    }
}
