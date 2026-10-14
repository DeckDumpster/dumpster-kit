//! Shared scaffolding for this crate's unit tests. Compiled out of any real build.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use dumpster_kit_keys::master::KeyEnv;
use dumpster_kit_tenancy::AppPrefix;

/// The fixture catalog: two holdings tables and their outbox triggers. A site's own schema
/// plays this part in production.
pub const FIXTURE_SQL: &str = include_str!("../../../fixtures/lake/holdings.sql");

/// The names the tests' app derives.
pub fn env() -> KeyEnv {
    KeyEnv::new(&AppPrefix::new("DUMPSTAR").unwrap())
}

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Points the app's master-key variable at `path` for the life of the guard, then puts the
/// previous value back.
pub struct EnvGuard {
    previous: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// Take the lock and set the variable.
    pub fn set(path: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = env().key_file_var;
        let previous = std::env::var(&var).ok();
        unsafe { std::env::set_var(&var, path) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let var = env().key_file_var;
        match &self.previous {
            Some(v) => unsafe { std::env::set_var(&var, v) },
            None => unsafe { std::env::remove_var(&var) },
        }
    }
}

/// A database on disk carrying the kit's transport tables and the fixture catalog. The
/// directory is returned because dropping it deletes the file.
pub fn collection_db() -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("collection.sqlite")).unwrap();
    conn.execute_batch(crate::outbox::SCHEMA_SQL).unwrap();
    conn.execute_batch(FIXTURE_SQL).unwrap();
    (dir, conn)
}

/// A registry holding the real schema, with `ids` registered as live tenants.
pub fn registry(ids: &[&str]) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!(
        "../../dumpster-kit-tenancy/src/schema_registry.sql"
    ))
    .unwrap();
    for id in ids {
        dumpster_kit_keys::state::register(&conn, id).unwrap();
    }
    conn
}
