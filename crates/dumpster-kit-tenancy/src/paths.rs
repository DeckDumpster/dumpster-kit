//! Filesystem paths and the two identifier validators.
//!
//! Layout — one shared catalog, one collection database per tenant:
//!
//! ```text
//! $<PREFIX>_HOME/                   # default ~/.<prefix>
//!   shared.sqlite                   # the catalog: one copy, ATTACHed read-only by every tenant
//!   registry.sqlite                 # handle → database_id (see [`crate::registry`])
//!   tenants/
//!     <database_id>.sqlite          # one file per registered user, named by ULID
//! ```
//!
//! The path functions split by what they are handed. [`tenant_db_file`] takes a *`database_id`*
//! — an opaque ULID the registry minted — so no caller-supplied string reaches a path
//! constructor. [`tenant_db_path`] takes a *handle*, the pre-registry naming, and belongs only to
//! the migrations that move a data directory off it.
//!
//! The catalog sits at the root, never under `tenants/`: "every `*.sqlite` under `tenants/` is
//! one tenant" is the statement a replication glob rests on, and the catalog is rebuildable and
//! deliberately not replicated.

use std::path::{Path, PathBuf};

use crate::app::AppPrefix;
use crate::error::{Result, TenancyError};

const DEFAULT_USER: &str = "collection";

/// Directory under the data dir holding one database per tenant.
pub const TENANTS_DIR: &str = "tenants";

/// A tenant name becomes a filename and an S3 replica-path component; 32 keeps both comfortable.
const MAX_TENANT_NAME: usize = 32;

/// The data directory: `$<PREFIX>_HOME` if set, else `$HOME/.<prefix>`.
pub fn home(app: &AppPrefix) -> Result<PathBuf> {
    let var = app.var("HOME");
    if let Ok(dir) = std::env::var(&var) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME")
        .map_err(|_| TenancyError::Env(format!("neither {var} nor HOME is set")))?;
    Ok(PathBuf::from(home).join(format!(".{}", app.cli())))
}

/// Path to the shared catalog database. One copy, shared by every tenant.
pub fn shared_db_path(app: &AppPrefix) -> Result<PathBuf> {
    Ok(home(app)?.join("shared.sqlite"))
}

/// The directory holding every tenant's collection database.
pub fn tenants_dir(app: &AppPrefix) -> Result<PathBuf> {
    Ok(home(app)?.join(TENANTS_DIR))
}

/// Path to the user registry database — the handle → `database_id` map.
pub fn registry_db_path(app: &AppPrefix) -> Result<PathBuf> {
    Ok(home(app)?.join("registry.sqlite"))
}

/// The active tenant: `$<PREFIX>_USER` if set, else `collection`.
pub fn current_user(app: &AppPrefix) -> String {
    std::env::var(app.var("USER")).unwrap_or_else(|_| DEFAULT_USER.to_string())
}

/// What a handle may contain, in one sentence, for telling a caller what they got wrong.
///
/// The rule is enforced twice — [`validate_tenant_name`] and the `handle` `CHECK` in
/// `schema_registry.sql` — and held to one corpus, `conformance/handles/cases.json`, which the
/// tests run through both. Change the rule in one place only and a test fails.
pub const HANDLE_RULE: &str =
    "1-32 characters of a-z, 0-9, `-` or `_`, starting with a letter or digit";

/// Reject anything that would not be safe as both a filename and an S3 replica-path component:
/// `[a-z0-9][a-z0-9_-]{0,31}`.
///
/// * Lowercase only: on a case-insensitive filesystem `Alice` and `alice` are one file but two
///   S3 prefixes.
/// * No `.`, `/` or `\`: traversal, and `foo.sqlite.sqlite`.
/// * No leading `-`: reads as a flag to every CLI it is passed to.
///
/// This is where a *claim* is checked, so a malformed request is refused as malformed rather than
/// answered "no such tenant". It is not what keeps a handle from becoming a path: nothing
/// concatenates one (see [`validate_database_id`]).
pub fn validate_tenant_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(TenancyError::Env("tenant name is empty".into()));
    }
    if name.len() > MAX_TENANT_NAME {
        return Err(TenancyError::Env(format!(
            "tenant name {name:?} is longer than {MAX_TENANT_NAME} characters"
        )));
    }
    let first = name.as_bytes()[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(TenancyError::Env(format!(
            "tenant name {name:?} must start with a lowercase letter or digit"
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
    {
        return Err(TenancyError::Env(format!(
            "tenant name {name:?} contains {bad:?}; allowed: a-z 0-9 - _"
        )));
    }
    Ok(())
}

/// Path to a tenant's collection database, named by *handle*. Validates the name; does not touch
/// the filesystem. The pre-registry addressing: only the migrations between the two namings have
/// business here, and none takes its name from a request.
pub fn tenant_db_path(app: &AppPrefix, name: &str) -> Result<PathBuf> {
    validate_tenant_name(name)?;
    Ok(tenants_dir(app)?.join(format!("{name}.sqlite")))
}

/// A canonical ULID is 26 characters of Crockford base32.
const DATABASE_ID_LEN: usize = 26;

/// Crockford base32, the alphabet a ULID renders in: the digits and the uppercase letters minus
/// `I`, `L`, `O` and `U`.
const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Reject anything that is not a canonical ULID as the registry mints them.
///
/// A tenant name is a *claim*, checked for what it must not contain. A `database_id` is *issued*:
/// the check is "is this one of ours". It is the one string that becomes a filename, and its
/// alphabet contains no `.`, `/` or `\`, so a value that passes can name only a sibling inside
/// `tenants/`.
///
/// Checked explicitly rather than with `Ulid::from_string`, which accepts lowercase: `01j…` and
/// `01J…` are one file on a case-insensitive filesystem but two S3 prefixes. Uppercase is the
/// only spelling, and every accepted id round-trips through a path unchanged.
pub fn validate_database_id(id: &str) -> Result<()> {
    if id.len() != DATABASE_ID_LEN {
        return Err(TenancyError::Env(format!(
            "database id {id:?} is not {DATABASE_ID_LEN} characters"
        )));
    }
    if let Some(bad) = id.chars().find(|c| !CROCKFORD.contains(&(*c as u8))) {
        return Err(TenancyError::Env(format!(
            "database id {id:?} contains {bad:?}; expected Crockford base32"
        )));
    }
    Ok(())
}

/// A tenant directory + an issued `database_id` → that user's collection database.
///
/// The **only** way a tenant database path is built at request time, and it takes a
/// `database_id` rather than a name: a handle read off a header is a lookup key in
/// [`crate::registry`], never a path component.
pub fn tenant_db_file(dir: &Path, database_id: &str) -> Result<PathBuf> {
    validate_database_id(database_id)?;
    Ok(dir.join(format!("{database_id}.sqlite")))
}

/// The `database_id` a tenant database path names, if it is one. The inverse of
/// [`tenant_db_file`]: `database_id_of(&tenant_db_file(d, id)?) == Some(id)` for every valid id.
pub fn database_id_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".sqlite")?;
    validate_database_id(stem).ok()?;
    Some(stem.to_string())
}

/// Path to the collection database named by an opaque `database_id`, under the [`tenants_dir`]
/// of the current data directory.
pub fn tenant_db_path_for_id(app: &AppPrefix, database_id: &str) -> Result<PathBuf> {
    tenant_db_file(&tenants_dir(app)?, database_id)
}

/// Where a tenant's collection database lived before the `tenants/` layout:
/// `<home>/<name>.sqlite`, beside the catalog.
pub fn legacy_user_db_path(app: &AppPrefix, name: &str) -> Result<PathBuf> {
    validate_tenant_name(name)?;
    Ok(home(app)?.join(format!("{name}.sqlite")))
}

/// Run `f` against a throwaway data directory for `app`.
///
/// The home variable is process-global and `cargo test` is threaded, so every test that sets one
/// shares ONE lock. Two locks would be no lock at all.
#[cfg(any(test, feature = "testing"))]
pub fn with_home<T>(app: &AppPrefix, f: impl FnOnce(&Path) -> T) -> T {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let var = app.var("HOME");
    // SAFETY: serialised by LOCK, which every test that sets a home variable goes through.
    unsafe { std::env::set_var(&var, dir.path()) };
    let out = f(dir.path());
    unsafe { std::env::remove_var(&var) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumpster_kit_conformance::{handle_cases, ulid_cases};

    fn app() -> AppPrefix {
        AppPrefix::new("KITTEST").unwrap()
    }

    /// The Rust half of the handle corpus. The SQL half is
    /// `registry::tests::the_check_and_the_validator_agree`, over the same file.
    #[test]
    fn tenant_names_are_validated() {
        for c in handle_cases() {
            assert_eq!(
                validate_tenant_name(&c.value).is_ok(),
                c.valid,
                "{:?} should be {}",
                c.value,
                if c.valid { "accepted" } else { "rejected" }
            );
        }
    }

    #[test]
    fn database_ids_are_validated_and_round_trip_through_a_path() {
        let dir = Path::new("/data/tenants");
        for c in ulid_cases() {
            assert_eq!(
                validate_database_id(&c.value).is_ok(),
                c.valid,
                "{:?}",
                c.value
            );
            assert_eq!(
                tenant_db_file(dir, &c.value).is_ok(),
                c.valid,
                "{:?}",
                c.value
            );
            if c.valid {
                let path = tenant_db_file(dir, &c.value).unwrap();
                assert_eq!(path, dir.join(format!("{}.sqlite", c.value)));
                assert_eq!(database_id_of(&path).as_deref(), Some(c.value.as_str()));
            }
        }
    }

    #[test]
    fn a_minted_uppercase_id_round_trips() {
        let id = crate::registry::mint_database_id();
        assert_eq!(id, id.to_ascii_uppercase());
        let path = tenant_db_file(Path::new("/t"), &id).unwrap();
        assert_eq!(database_id_of(&path), Some(id.clone()));
        assert!(tenant_db_file(Path::new("/t"), &id.to_lowercase()).is_err());
        assert_eq!(
            database_id_of(Path::new(&format!("/t/{}.sqlite", id.to_lowercase()))),
            None
        );
    }

    #[test]
    fn paths_derive_from_the_prefix() {
        with_home(&app(), |home| {
            assert_eq!(
                tenant_db_path(&app(), "alice").unwrap(),
                home.join("tenants").join("alice.sqlite")
            );
            assert_eq!(shared_db_path(&app()).unwrap(), home.join("shared.sqlite"));
            assert_eq!(
                registry_db_path(&app()).unwrap(),
                home.join("registry.sqlite")
            );
        });
    }

    #[test]
    fn traversal_cannot_escape_the_tenants_dir() {
        with_home(&app(), |_| {
            assert!(tenant_db_path(&app(), "../../etc/passwd").is_err());
            assert!(legacy_user_db_path(&app(), "../../etc/passwd").is_err());
            assert!(tenant_db_path_for_id(&app(), "../../etc/passwd").is_err());
        });
    }
}
