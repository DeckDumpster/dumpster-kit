//! **Backup and destruction are different paths.** This file is what makes
//! that a checked property rather than a claim in a doc comment.
//!
//! The bead (`pd-ulds`) asks for the distinction to be demonstrated "not just
//! [as] separate function names calling into shared logic that could conflate
//! the two failure modes", so it is asserted twice over, in two different
//! ways:
//!
//! 1. **Structurally** — the source of `backup.rs` is read and must not name
//!    the registry, and the source of `destroy.rs` is read and must not name
//!    the master key. Comments and doc comments are stripped first, because
//!    both modules talk about each other at length and prose is exactly what
//!    this is not about. A future edit that routes one through the other
//!    fails here, before anybody has to notice the behaviour changed.
//!
//! 2. **Behaviourally** — each path is run with the other one's world
//!    destroyed, and must be unaffected and distinguishable:
//!    * with the master key deleted, tombstoning still works, and a
//!      tombstoned tenant still reports as *revoked* while a live one reports
//!      as *broken*;
//!    * with the registry unreachable, the backup still produces the key, and
//!      failing to reach the registry never reads as a revocation.
//!
//! The failure this guards against is not hypothetical bookkeeping. If the
//! two share a mechanism then either (a) a backup failure starts looking like
//! a legitimate deletion — data loss disguised as compliance — or (b) a
//! deletion is implemented as "the backup is gone", and revokes nothing the
//! moment the backup turns up.

use std::path::Path;

use dumpster_kit_keys::error::KeyError;
use dumpster_kit_keys::master::KeyEnv;
use dumpster_kit_keys::{backup, derive, destroy, master, state};
use dumpster_kit_tenancy::AppPrefix;

fn env() -> KeyEnv {
    KeyEnv::new(&AppPrefix::new("DUMPSTAR").unwrap())
}

const A: &str = "01J0000000000000000000000A";
const B: &str = "01J0000000000000000000000B";

fn registry() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!(
        "../../dumpster-kit-tenancy/src/schema_registry.sql"
    ))
    .unwrap();
    conn
}

/// Point the key file at `path` for the duration. Serialised by the same
/// lock the crate's own tests use? No — integration tests are a separate
/// binary, so this file has its own. Every test here that touches the
/// environment goes through it.
struct EnvGuard {
    previous: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl EnvGuard {
    fn set(path: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var(&env().key_file_var).ok();
        unsafe { std::env::set_var(&env().key_file_var, path) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe { std::env::set_var(&env().key_file_var, v) },
            None => unsafe { std::env::remove_var(&env().key_file_var) },
        }
    }
}

// ── 1. Structural: neither path can reach the other's world ────────────────

const BACKUP_SRC: &str = include_str!("../src/backup.rs");
const DESTROY_SRC: &str = include_str!("../src/destroy.rs");

/// Everything outside a comment, and outside the module's own unit tests.
///
/// `//`-comments (doc comments included) are dropped whole, because both
/// modules explain each other at length and prose is exactly what this rule
/// is not about. `#[cfg(test)]` onwards is dropped too: the rule is about
/// what the two SHIPPED paths can reach, and test scaffolding legitimately
/// stands up both worlds to check they stay apart.
fn code_only(src: &str) -> String {
    let src = match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    };
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The backup path is about a FILE. It must not be able to read, write or
/// ask anything of the key-state registry — a backup that consulted the
/// tombstones could report a revoked tenant as an unbackable key, or worse,
/// treat a registry it could not open as a reason not to back anything up.
#[test]
fn the_backup_path_cannot_reach_the_registry() {
    let code = code_only(BACKUP_SRC);
    for forbidden in [
        "state::",
        "destroy::",
        "tombstone",
        "Connection",
        "rusqlite",
        "KeyState",
        "database_id",
    ] {
        assert!(
            !code.contains(forbidden),
            "backup.rs names {forbidden:?} in code — the backup path must not be able to reach \
             the key-state registry. If a backup can fail for a reason that lives in the \
             tombstone table, then 'we could not back this up' and 'this was revoked' have \
             started to share a failure mode."
        );
    }
}

/// The destruction path is about a ROW. It must not be able to read, write,
/// move or even locate the master key file — the whole hazard is a deletion
/// implemented as "the key went missing", which revokes nothing at all.
#[test]
fn the_destruction_path_cannot_reach_the_master_key() {
    let code = code_only(DESTROY_SRC);
    for forbidden in [
        "master::",
        "MasterKey",
        "backup::",
        "key_path",
        "std::fs",
        "PathBuf",
        "remove_file",
        "DUMPSTAR_MASTER_KEY_FILE",
    ] {
        assert!(
            !code.contains(forbidden),
            "destroy.rs names {forbidden:?} in code — the destruction path must not be able to \
             reach the master key. A revocation that goes anywhere near that file is one \
             refactor away from being 'delete the key and hope', which revokes every tenant at \
             once and revokes none of them permanently."
        );
    }
}

/// The two modules must not share a private helper, which is the subtler way
/// the same coupling arrives: not one calling the other, but both calling a
/// third thing whose failure they would then report identically.
#[test]
fn the_two_paths_share_no_helper_of_their_own() {
    let backup = code_only(BACKUP_SRC);
    let destroy = code_only(DESTROY_SRC);

    let calls = |code: &str| -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for token in code.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')) {
            if let Some(rest) = token.strip_prefix("crate::")
                && let Some(module) = rest.split("::").next()
                && !module.is_empty()
            {
                out.insert(module.to_string());
            }
        }
        out
    };

    let shared: Vec<_> = calls(&backup)
        .intersection(&calls(&destroy))
        .filter(|m| *m != "error")
        .cloned()
        .collect();
    assert!(
        shared.is_empty(),
        "backup.rs and destroy.rs both reach into crate::{shared:?}. Only crate::error may be \
         common ground — it is the module whose entire job is keeping their failures apart. \
         Anything else is logic the two now share, and shared logic is how 'lost' and \
         'destroyed' become one failure mode."
    );
}

// ── 2. Behavioural: each path survives the other's world ending ────────────

/// Destroy the master key entirely. The destruction path must still work,
/// and — the part that matters — a revoked tenant must still read as
/// *revoked* while a live one reads as *broken*.
#[test]
fn destruction_is_unaffected_by_a_backup_that_cannot_run() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("tenant-master.key");
    master::create_at(&env(), &key).unwrap();
    let _guard = EnvGuard::set(&key);

    let conn = registry();
    state::register(&conn, A).unwrap();
    state::register(&conn, B).unwrap();

    // The backup path is healthy to begin with.
    backup::export(&env()).unwrap();

    // Now break it, completely.
    std::fs::remove_file(&key).unwrap();
    let backup_err = backup::export(&env()).unwrap_err();
    assert!(backup_err.is_operational_failure());
    assert!(
        !backup_err.is_deliberate_revocation(),
        "a backup that cannot run must never report as a deletion: {backup_err}"
    );

    // Destruction is untouched by that.
    let row = destroy::tombstone(&conn, A, Some("account deleted")).unwrap();
    assert_eq!(row.state, dumpster_kit_keys::KeyState::Tombstoned);

    // …and the two tenants remain distinguishable, with no key on the box.
    let revoked = derive::tenant_key(&env(), &conn, A).unwrap_err();
    assert!(
        revoked.is_deliberate_revocation(),
        "the revoked tenant must read as revoked: {revoked}"
    );
    let live = derive::tenant_key(&env(), &conn, B).unwrap_err();
    assert!(
        live.is_operational_failure() && !live.is_deliberate_revocation(),
        "the live tenant must read as broken, not deleted: {live}"
    );
    assert!(matches!(live, KeyError::MasterKeyUnavailable { .. }));
}

/// Break the registry instead. The backup path must be unaffected — it does
/// not consult the registry to know what the master key is — and a registry
/// failure must never read as a revocation.
#[test]
fn backup_is_unaffected_by_a_registry_that_cannot_be_read() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("tenant-master.key");
    let (_, fingerprint) = master::create_at(&env(), &key).unwrap();
    let _guard = EnvGuard::set(&key);

    // A registry connection that has been closed under everyone's feet: any
    // statement against it fails. This is what a database mid-restore, on a
    // full disk, or on a volume that failed to mount looks like from here.
    let broken = registry();
    broken.execute_batch("DROP TABLE tenant_key").unwrap();

    let derive_err = derive::tenant_key(&env(), &broken, A).unwrap_err();
    assert!(derive_err.is_operational_failure(), "{derive_err}");
    assert!(
        !derive_err.is_deliberate_revocation(),
        "an unreadable registry must never read as a revocation: {derive_err}"
    );
    let destroy_err = destroy::tombstone(&broken, A, None).unwrap_err();
    assert!(destroy_err.is_operational_failure(), "{destroy_err}");

    // The backup path does not care about any of that.
    let material = backup::export(&env()).unwrap();
    let restored = tmp.path().join("elsewhere").join("tenant-master.key");
    let (_, restored_fp) = backup::restore_to(&env(), &restored, &material).unwrap();
    assert_eq!(
        restored_fp, fingerprint,
        "the backup must still round-trip THE key with the registry broken"
    );
}

/// The end-to-end shape of the property, in one test: back the key up,
/// restore it onto a *different* box, and confirm the tombstone travels with
/// the registry rather than with the key.
///
/// This is the scenario that goes wrong if the two paths are one: restoring
/// a backup must bring the data back, and must NOT bring back access to an
/// account somebody deleted.
#[test]
fn a_restored_master_key_does_not_undo_a_revocation() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("original").join("tenant-master.key");
    master::create_at(&env(), &key).unwrap();

    let conn = registry();
    state::register(&conn, A).unwrap();
    state::register(&conn, B).unwrap();

    let (live_before, material) = {
        let _guard = EnvGuard::set(&key);
        let live = derive::tenant_key(&env(), &conn, B).unwrap().fingerprint();
        destroy::tombstone(&conn, A, Some("account deleted")).unwrap();
        (live, backup::export(&env()).unwrap())
    };

    // A rebuilt box: same registry (restored first, per deploy/RESTORE.md),
    // master key pasted back from the password manager.
    let rebuilt = tmp.path().join("rebuilt").join("tenant-master.key");
    backup::restore_to(&env(), &rebuilt, &material).unwrap();
    let _guard = EnvGuard::set(&rebuilt);

    assert_eq!(
        derive::tenant_key(&env(), &conn, B).unwrap().fingerprint(),
        live_before,
        "a restored key must derive exactly the keys it did before — that is what a backup is"
    );
    let err = derive::tenant_key(&env(), &conn, A).unwrap_err();
    assert!(
        err.is_deliberate_revocation(),
        "…and it must not resurrect a deleted account: {err}"
    );
}

/// The mirror of the test above, and the reason `state::tombstone` has no
/// foreign key: purging the user row must not un-revoke the key.
#[test]
fn deleting_the_user_does_not_undo_the_revocation() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("tenant-master.key");
    master::create_at(&env(), &key).unwrap();
    let _guard = EnvGuard::set(&key);

    let conn = registry();
    conn.execute(
        "INSERT INTO user (database_id, handle, created_at, state) \
         VALUES (?1, 'alice', '2026-08-13T00:00:00Z', 'active')",
        rusqlite::params![A],
    )
    .unwrap();
    state::register(&conn, A).unwrap();
    destroy::tombstone(&conn, A, Some("account deleted")).unwrap();
    conn.execute(
        "DELETE FROM user WHERE database_id = ?1",
        rusqlite::params![A],
    )
    .unwrap();

    let err = derive::tenant_key(&env(), &conn, A).unwrap_err();
    assert!(err.is_deliberate_revocation(), "{err}");
}

/// The path resolution the deploy scripts depend on, asserted here so a
/// change to it fails a test rather than a production box: the environment
/// variable wins, and the default is under `~/.config/dumpstar/`, which is the
/// directory the Litestream credentials already live in.
#[test]
fn the_key_lives_where_the_litestream_credentials_do() {
    let tmp = tempfile::tempdir().unwrap();
    let explicit = tmp.path().join("instance").join("tenant-master.key");
    {
        let _guard = EnvGuard::set(&explicit);
        assert_eq!(env().key_path().unwrap(), explicit);
    }
    assert_eq!(
        env().default_relative_path,
        ".config/dumpstar/tenant-master.key",
        "the default must stay inside the host-config directory deploy/setup.sh already \
         creates and chmods for litestream.env"
    );
}
