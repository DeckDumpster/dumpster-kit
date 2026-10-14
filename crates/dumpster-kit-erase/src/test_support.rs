//! Shared scaffolding for this crate's unit tests. Compiled out of any real
//! build.
//!
//! The master key's location is an environment variable, so every test that
//! needs a key mutates process-wide state and they cannot be allowed to
//! overlap — the same guard `dumpstar-keys` and `dumpstar-ship` carry, for the
//! same reason.
//!
//! Nothing here is anybody's data. The "holdings" are the string `not real
//! holdings` and the printing ids are invented; the tenant zone is the
//! SUBJECT of this design, so its fixtures are treated as if they were real.

use std::sync::{Mutex, MutexGuard};

use dumpster_kit_keys::master::KeyEnv;
use dumpster_kit_lake::{DirStore, ObjectStore, TenantZoneConfig};
use dumpster_kit_tenancy::AppPrefix;
use rusqlite::Connection;

/// The names the tests' app derives.
pub fn env() -> KeyEnv {
    KeyEnv::new(&AppPrefix::new("DUMPSTAR").unwrap())
}

pub const A: &str = "01J0000000000000000000000A";
pub const B: &str = "01J0000000000000000000000B";

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A temp directory holding a master key that this process will find, plus a
/// `DirStore` standing in for the zone.
///
/// The guard rides in the `TempDir`'s tuple slot so a test binding it as
/// `_tmp` keeps both alive for the whole test — dropping the lock early would
/// let the next test move the key file out from under this one.
pub struct Fixture {
    _dir: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
    previous: Option<String>,
    path: std::path::PathBuf,
}

impl Fixture {
    /// The directory the master key is in, so a test can remove it.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe { std::env::set_var(&env().key_file_var, v) },
            None => unsafe { std::env::remove_var(&env().key_file_var) },
        }
    }
}

/// A zone on disk, with a master key minted for this test.
///
/// The `TenantZoneConfig` names the real [`dumpster_kit_lake::TENANT_ROOT`], so
/// every key a test builds is the key production would build.
pub fn dir_zone() -> (Fixture, DirStore, TenantZoneConfig) {
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let previous = std::env::var(&env().key_file_var).ok();
    unsafe { std::env::set_var(&env().key_file_var, path.join("tenant-master.key")) };
    dumpster_kit_keys::master::create(&env()).unwrap();

    let store = DirStore::new(path.join("zone"));
    let config = TenantZoneConfig {
        profile: "dumpstar-tenant-test".to_string(),
        prefix: dumpster_kit_lake::TENANT_ROOT.to_string(),
    };
    (
        Fixture {
            _dir: dir,
            _lock: lock,
            previous,
            path,
        },
        store,
        config,
    )
}

/// A registry carrying the real schema, with `ids` registered as live tenants.
pub fn registry(ids: &[&str]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!(
        "../../dumpster-kit-tenancy/src/schema_registry.sql"
    ))
    .unwrap();
    for id in ids {
        dumpster_kit_keys::state::register(&conn, id).unwrap();
    }
    conn
}

/// Two objects per named dataset, under `database_id`'s prefix, over two days
/// — so a test that only cleared one date would be visible.
///
/// The bytes are not sealed and do not need to be: what this seeds is the
/// SHAPE of a tenant's partition, which is what the sweep addresses.
/// [`seal_into`] is what puts a real sealed object there.
pub fn seed(store: &DirStore, database_id: &str, datasets: &[&str]) {
    for dataset in datasets {
        for as_of in ["2026-08-13", "2026-08-14"] {
            store
                .put(
                    &format!(
                        "{}database_id={database_id}/dataset={dataset}/as_of={as_of}/\
                         part-0000{}",
                        dumpster_kit_lake::TENANT_ROOT,
                        dumpster_kit_lake::PART_SUFFIX
                    ),
                    b"not real holdings".to_vec(),
                )
                .unwrap();
        }
    }
}

/// Put one genuinely sealed object in the zone and hand back a copy of it —
/// the "stray copy taken before the deletion" every proof needs to be about
/// something real.
pub fn seal_into(
    registry: &Connection,
    database_id: &str,
    store: &DirStore,
    config: &TenantZoneConfig,
) -> crate::verify::StrayCopy {
    let object_key = config.rooted(
        dumpster_kit_lake::range_part_key(
            database_id,
            dumpster_kit_lake::TenantDataset::Holdings,
            "2026-08-14",
            1,
            9,
        )
        .unwrap(),
    );
    let key = dumpster_kit_keys::tenant_key(&env(), registry, database_id)
        .expect("seal_into needs a tenant whose key still derives");
    // Deliberately Parquet-shaped: the stray-copy check refuses to conclude
    // anything from a file that was never encrypted, so the plaintext here
    // has to be the thing that would otherwise be readable.
    let sealed =
        dumpster_kit_ship::cipher::seal(&key, &object_key, b"PAR1 not real holdings").unwrap();
    store.put(&object_key, sealed.clone()).unwrap();
    crate::verify::StrayCopy {
        object_key,
        bytes: sealed,
    }
}
