//! The shipper, end to end over the fixture catalog (`fixtures/lake/holdings.sql`) and a real data directory and a real object
//! store — the four claims `pd-dxn3` asks to see proven, plus the two the
//! design rests on.
//!
//! | claim                         | test                                          |
//! |-------------------------------|-----------------------------------------------|
//! | gap detection                 | `a_dropped_sequence_number_is_detected…`      |
//! | idempotence                   | `shipping_the_same_rows_twice_leaves…`        |
//! | resumability                  | `a_crash_after_a_part_lands_but_before…`      |
//! | encrypted, under the RIGHT key| `each_tenant_s_parts_open_only_under…`        |
//! | a tombstone stops shipping    | `a_tombstoned_tenant_is_not_shipped…`         |
//! | absence is not permission     | `an_unregistered_tenant_is_skipped…`          |
//! | a backfill is an ordinary run | `a_backfilled_collection_ships_as_ordinary…`  |
//! | the zone IS the collection    | `a_shipped_part_projects_back_to_the_live…`   |
//!
//! The object store is a [`DirStore`], which is the same `ObjectStore` the
//! real thing is behind and holds exactly the keys and bytes S3 would — so
//! "the zone is unchanged" can be a byte-for-byte comparison of what is on
//! disk rather than a claim about an API. `tests/lake/shipper.sh` is the
//! other half: the same binary against a real MinIO, under the real tenant
//! credential policy, with a real `SIGKILL`.
//!
//! Every fixture here is treated as if it were real tenant data — which is to
//! say there is none of it. The collections hold invented printing ids.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use dumpster_kit_lake::{DirStore, ObjectStore, TenantZoneConfig};
use dumpster_kit_ship::run::{Outcome, Status};

const SOURCES: dumpster_kit_ship::zone::Sources<'static> = &[
    ("collection", "zone_holdings"),
    ("sealed_collection", "zone_sealed_holdings"),
];
use dumpster_kit_keys::master::KeyEnv;
use dumpster_kit_ship::outbox::{self, Event};
use dumpster_kit_ship::{Gap, ShipContext, Tenant};
use dumpster_kit_ship::{cipher, cursor, encode};
use dumpster_kit_tenancy::AppPrefix;
use rusqlite::Connection;

const ALICE: &str = "01J0000000000000000000000A";
const BOB: &str = "01J0000000000000000000000B";

// ── the world one test runs in ──────────────────────────────────────────────

/// The environment this crate's jobs read is process-wide, so tests that set
/// it cannot overlap. Same shape as `dumpstar-keys`' own guard.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn app() -> AppPrefix {
    AppPrefix::new("DUMPSTAR").unwrap()
}

fn env() -> KeyEnv {
    KeyEnv::new(&app())
}

/// The site's opener, as the fixture catalog defines it: the kit's transport tables and the
/// fixture holdings tables, on every open.
fn open_fixture(path: &Path) -> dumpster_kit_ship::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(outbox::SCHEMA_SQL)?;
    conn.execute_batch(include_str!("../../../fixtures/lake/holdings.sql"))?;
    Ok(conn)
}

struct World {
    _dir: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
    home: std::path::PathBuf,
    zone_root: std::path::PathBuf,
    previous_home: Option<String>,
    previous_key: Option<String>,
}

impl World {
    /// A data directory with a registry, a master key, and an empty zone.
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("data");
        let zone_root = dir.path().join("zone");
        std::fs::create_dir_all(home.join("tenants")).unwrap();
        std::fs::create_dir_all(&zone_root).unwrap();

        let previous_home = std::env::var("DUMPSTAR_HOME").ok();
        let previous_key = std::env::var(&env().key_file_var).ok();
        unsafe {
            std::env::set_var("DUMPSTAR_HOME", &home);
            std::env::set_var(&env().key_file_var, dir.path().join("tenant-master.key"));
        }
        dumpster_kit_keys::master::create(&env()).unwrap();

        World {
            _dir: dir,
            _lock: lock,
            home,
            zone_root,
            previous_home,
            previous_key,
        }
    }

    fn db_path(&self, database_id: &str) -> std::path::PathBuf {
        self.home
            .join("tenants")
            .join(format!("{database_id}.sqlite"))
    }

    /// Register `handle` with a chosen `database_id`, create their collection
    /// database, and register their key.
    fn tenant(&self, handle: &str, database_id: &str) -> Tenant {
        let registry = self.registry();
        registry
            .execute(
                "INSERT INTO user (database_id, handle, created_at, state) \
                 VALUES (?1, ?2, '2026-08-14T00:00:00Z', 'active')",
                rusqlite::params![database_id, handle],
            )
            .unwrap();
        dumpster_kit_keys::state::register(&registry, database_id).unwrap();
        let path = self.db_path(database_id);
        open_fixture(&path).unwrap();
        self.tenants()
            .into_iter()
            .find(|t| t.user.database_id == database_id)
            .expect("just registered")
    }

    /// Every registered tenant, in registry order.
    fn tenants(&self) -> Vec<Tenant> {
        dumpster_kit_tenancy::registry::list(&self.registry())
            .unwrap()
            .into_iter()
            .map(|user| Tenant {
                path: self.db_path(&user.database_id),
                user,
            })
            .collect()
    }

    fn collection(&self, database_id: &str) -> Connection {
        open_fixture(&self.db_path(database_id)).unwrap()
    }

    fn registry(&self) -> Connection {
        dumpster_kit_tenancy::registry::open(&app()).unwrap()
    }

    fn zone(&self) -> DirStore {
        DirStore::new(&self.zone_root)
    }

    /// Every object in the zone, key -> bytes. The unit "the zone is
    /// unchanged" is measured in.
    fn objects(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        walk(&self.zone_root, &self.zone_root, &mut out);
        out
    }
}

impl Drop for World {
    fn drop(&mut self) {
        unsafe {
            match &self.previous_home {
                Some(v) => std::env::set_var("DUMPSTAR_HOME", v),
                None => std::env::remove_var("DUMPSTAR_HOME"),
            }
            match &self.previous_key {
                Some(v) => std::env::set_var(&env().key_file_var, v),
                None => std::env::remove_var(&env().key_file_var),
            }
        }
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let key = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            out.insert(key, std::fs::read(&path).unwrap());
        }
    }
}

fn lake_env() -> dumpster_kit_lake::LakeEnv {
    dumpster_kit_lake::LakeEnv::new(&app())
}

fn config() -> TenantZoneConfig {
    TenantZoneConfig::from_settings(
        &lake_env(),
        &[(
            lake_env().tenant_profile,
            "dumpstar-tenant-test".to_string(),
        )]
        .into_iter()
        .collect(),
        None,
    )
    .unwrap()
}

/// Append `n` holdings to a collection, dated `date`. The triggers write the
/// outbox; nothing here mentions it, which is the point of pd-5m54.
fn add_holdings(conn: &Connection, n: usize, date: &str) {
    let before = high_water(conn);
    for i in 0..n {
        conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source) VALUES (?1, ?2, 'test')",
            rusqlite::params![format!("fixture-{date}-{i}"), format!("{date}T00:00:00Z")],
        )
        .unwrap();
    }
    redate(conn, before, date);
}

/// `n` sealed lots, dated onto `date` the same way. Sealed product is a
/// holding like any other (pd-4gop) and, since pd-bbv7, one the read-back
/// stages rather than declines.
fn add_sealed(conn: &Connection, n: usize, date: &str) {
    let before = high_water(conn);
    for i in 0..n {
        conn.execute(
            "INSERT INTO sealed_collection (product_id, quantity, added_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![9000 + i as i64, 2 + i as i64, format!("{date}T00:00:00Z")],
        )
        .unwrap();
    }
    redate(conn, before, date);
}

fn high_water(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT coalesce(max(seq), 0) FROM ownership_outbox",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// Move every event after `after` onto `date`, keeping its time of day.
///
/// The trigger reads the clock, and these tests are about partitioning by
/// date — so the fixture says which day each batch of events belongs to,
/// exactly as a run on that day would have found it. Bounded by `after` so a
/// later batch cannot re-date an earlier one.
fn redate(conn: &Connection, after: i64, date: &str) {
    conn.execute(
        "UPDATE ownership_outbox SET occurred_at = ?1 || substr(occurred_at, 11) \
         WHERE seq > ?2",
        rusqlite::params![date, after],
    )
    .unwrap();
}

/// A context over `zone` and the world's registry, handed to `f`.
fn with_ctx<R>(world: &World, zone: &dyn ObjectStore, f: impl FnOnce(&ShipContext<'_>) -> R) -> R {
    let registry = world.registry();
    let config = config();
    let keys = env();
    let open = |p: &Path| open_fixture(p);
    let ctx = ShipContext {
        keys: &keys,
        zone,
        config: &config,
        registry: &registry,
        open_tenant: &open,
        crash_var: app().var("SHIP_CRASH_AFTER_PARTS"),
        cli: app().cli(),
    };
    f(&ctx)
}

fn ship(world: &World, tenants: &[Tenant], max_rows: usize) -> dumpster_kit_ship::Report {
    let zone = world.zone();
    with_ctx(world, &zone, |ctx| {
        dumpster_kit_ship::ship_all(ctx, tenants, max_rows)
    })
}

// ── the run ─────────────────────────────────────────────────────────────────

#[test]
fn a_first_run_ships_every_event_under_the_tenant_s_own_prefix() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 5, "2026-08-14");

    let report = ship(&world, &[alice], 100);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert_eq!(report.events(), 5);
    assert_eq!(report.parts(), 1);

    let objects = world.objects();
    assert_eq!(objects.len(), 1);
    let key = objects.keys().next().unwrap();
    assert_eq!(
        key,
        &format!(
            "tenant/database_id={ALICE}/dataset=holdings/as_of=2026-08-14/\
             part-seq-000000000001-000000000005.parquet.enc"
        ),
        "the key names the range it carries, under this tenant's prefix"
    );

    // …and the cursor now says so.
    assert_eq!(cursor::shipped_thru(&world.collection(ALICE)).unwrap(), 5);
    assert_eq!(cursor::pending(&world.collection(ALICE)).unwrap(), 0);
}

#[test]
fn a_second_run_ships_only_what_arrived_since() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 3, "2026-08-14");
    ship(&world, std::slice::from_ref(&alice), 100);

    add_holdings(&world.collection(ALICE), 2, "2026-08-15");
    let report = ship(&world, &[alice], 100);

    assert_eq!(report.events(), 2, "only the new rows");
    let keys: Vec<_> = world.objects().into_keys().collect();
    assert_eq!(keys.len(), 2);
    assert!(keys[1].contains("as_of=2026-08-15"));
    assert!(keys[1].ends_with("part-seq-000000000004-000000000005.parquet.enc"));
}

#[test]
fn a_part_never_spans_two_days_and_a_long_run_is_split() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-08-13");
    add_holdings(&conn, 5, "2026-08-14");

    ship(&world, &[alice], 3);
    let keys: Vec<_> = world.objects().into_keys().collect();
    assert_eq!(
        keys.iter()
            .map(|k| k.rsplit('/').next().unwrap())
            .collect::<Vec<_>>(),
        [
            "part-seq-000000000001-000000000003.parquet.enc",
            "part-seq-000000000004-000000000006.parquet.enc",
            "part-seq-000000000007-000000000008.parquet.enc",
        ]
    );
    assert!(keys[0].contains("as_of=2026-08-13"));
    assert!(keys[1].contains("as_of=2026-08-14"));
}

// ── idempotence ─────────────────────────────────────────────────────────────

/// The bead's claim, in its own words: ship the same outbox range twice, the
/// tenant zone is UNCHANGED by the second run.
#[test]
fn shipping_the_same_rows_twice_leaves_the_zone_byte_identical() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 9, "2026-08-14");

    ship(&world, std::slice::from_ref(&alice), 4);
    let after_first = world.objects();
    assert_eq!(after_first.len(), 3);

    // Wind the cursor back, which is what a redrive of the same range is.
    world
        .collection(ALICE)
        .execute("DELETE FROM ownership_outbox_cursor", [])
        .unwrap();

    let report = ship(&world, &[alice], 4);
    assert_eq!(report.events(), 9, "it really did ship them all again");
    assert_eq!(
        world.objects(),
        after_first,
        "the second run wrote different bytes, or extra objects"
    );
}

// ── resumability ────────────────────────────────────────────────────────────

/// A store that writes the object and then dies, which is the one instant
/// at-least-once delivery is about: the part is in the zone and the cursor
/// does not know it. The container gate does this with a real `SIGKILL` on a
/// real process; here it is a panic, so it lands on the same part every time.
struct DiesAfter {
    inner: DirStore,
    after: usize,
    puts: std::sync::atomic::AtomicUsize,
}

impl ObjectStore for DiesAfter {
    fn put(&self, key: &str, body: Vec<u8>) -> dumpster_kit_lake::Result<()> {
        self.inner.put(key, body)?;
        let done = self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if done >= self.after {
            panic!("the shipper was killed after {done} part(s)");
        }
        Ok(())
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

#[test]
fn a_crash_after_a_part_lands_but_before_the_cursor_moves_re_ships_only_that_part() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 12, "2026-08-14");

    // Killed the instant the second part lands.
    let dying = DiesAfter {
        inner: DirStore::new(&world.zone_root),
        after: 2,
        puts: std::sync::atomic::AtomicUsize::new(0),
    };
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_ctx(&world, &dying, |ctx| {
            dumpster_kit_ship::ship_one(ctx, &alice, 4)
        })
    }));
    assert!(killed.is_err(), "the fixture did not actually kill the run");

    let after_crash = world.objects();
    assert_eq!(after_crash.len(), 2, "two parts landed before the crash");
    assert_eq!(
        cursor::shipped_thru(&world.collection(ALICE)).unwrap(),
        4,
        "…and the cursor only knows about the first — the second is unrecorded, \
         which is exactly the state a re-ship has to handle"
    );

    // Restart.
    let report = ship(&world, &[alice], 4);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert_eq!(
        report.events(),
        8,
        "the restart re-ships the part the crash did not record, and the rest"
    );

    let objects = world.objects();
    assert_eq!(
        objects.len(),
        3,
        "no duplicate object beside the re-shipped one"
    );
    for (key, bytes) in &after_crash {
        assert_eq!(
            objects.get(key),
            Some(bytes),
            "{key} changed when it was shipped again"
        );
    }
    assert_eq!(cursor::shipped_thru(&world.collection(ALICE)).unwrap(), 12);

    // Nothing was lost: every event is in the zone exactly once.
    let mut seqs: Vec<i64> = Vec::new();
    for (key, bytes) in &objects {
        let tenant_key = dumpster_kit_keys::tenant_key(&env(), &world.registry(), ALICE).unwrap();
        let parquet = cipher::open(&tenant_key, key, bytes).unwrap();
        seqs.extend(encode::decode(parquet).unwrap().into_iter().map(|e| e.seq));
    }
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=12).collect::<Vec<_>>());
}

// ── gap detection ───────────────────────────────────────────────────────────

/// Deliberately drop a sequence number in transit, exactly as the bead asks.
/// Deleting an unshipped outbox row is what a lost event looks like from
/// here — the number is gone and, because `AUTOINCREMENT` never reissues one,
/// it is never coming back.
#[test]
fn a_dropped_sequence_number_is_detected_recorded_and_alarmed() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 6, "2026-08-14");
    conn.execute("DELETE FROM ownership_outbox WHERE seq IN (3, 4)", [])
        .unwrap();

    let report = ship(&world, &[alice], 100);

    assert_eq!(
        report.outcome(),
        Outcome::Gap,
        "a lost event must not come out as a clean run"
    );
    assert_eq!(report.outcome().code(), 3);
    assert_eq!(
        report.gaps(),
        [(
            ALICE,
            Gap {
                from_seq: 3,
                to_seq: 4
            }
        )]
    );

    // Durable, not just printed: the journal rotates, this does not.
    let recorded = cursor::gaps(&conn).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!((recorded[0].from_seq, recorded[0].to_seq), (3, 4));
    assert!(!recorded[0].detected_at.is_empty());
}

/// The other half of the decision: a gap is alarmed, not obeyed. The rows
/// that are still there are already lost if the shipper refuses to move.
#[test]
fn a_gap_does_not_stop_the_events_that_are_still_there() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 6, "2026-08-14");
    conn.execute("DELETE FROM ownership_outbox WHERE seq = 3", [])
        .unwrap();

    let report = ship(&world, std::slice::from_ref(&alice), 100);
    assert_eq!(report.events(), 5, "everything that survived was shipped");
    assert_eq!(cursor::shipped_thru(&conn).unwrap(), 6);

    // A second run neither re-detects the gap (the cursor is past it) nor
    // records a second copy of it — the ledger is the durable answer.
    let again = ship(&world, &[alice], 100);
    assert!(again.gaps().is_empty());
    assert_eq!(cursor::gaps(&conn).unwrap().len(), 1);
}

#[test]
fn a_gap_at_the_head_of_the_batch_is_seen_too() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 4, "2026-08-14");
    // The first two events never reach the shipper at all.
    conn.execute("DELETE FROM ownership_outbox WHERE seq <= 2", [])
        .unwrap();

    let report = ship(&world, &[alice], 100);
    assert_eq!(
        report.gaps(),
        [(
            ALICE,
            Gap {
                from_seq: 1,
                to_seq: 2
            }
        )],
        "a hole before the first surviving row is the one an implementation forgets"
    );
}

// ── the key, and whose it is ────────────────────────────────────────────────

#[test]
fn each_tenant_s_parts_open_only_under_that_tenant_s_own_key() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    add_holdings(&world.collection(ALICE), 2, "2026-08-14");
    add_holdings(&world.collection(BOB), 2, "2026-08-14");

    let report = ship(&world, &[alice, bob], 100);
    assert_eq!(report.outcome(), Outcome::Clean);

    let objects = world.objects();
    assert_eq!(objects.len(), 2);
    let registry = world.registry();
    let alice_key = dumpster_kit_keys::tenant_key(&env(), &registry, ALICE).unwrap();
    let bob_key = dumpster_kit_keys::tenant_key(&env(), &registry, BOB).unwrap();

    for (key, bytes) in &objects {
        let (mine, theirs) = if key.contains(ALICE) {
            (&alice_key, &bob_key)
        } else {
            (&bob_key, &alice_key)
        };
        // Not plaintext…
        assert!(
            !bytes.windows(4).any(|w| w == b"PAR1"),
            "{key} looks like a bare Parquet file — it is not encrypted"
        );
        assert!(
            !bytes.windows(7).any(|w| w == b"fixture"),
            "{key} carries a readable printing id"
        );
        // …opens under its own key…
        let parquet = cipher::open(mine, key, bytes).expect("its own key must open it");
        assert_eq!(&parquet[..4], b"PAR1");
        assert_eq!(encode::decode(parquet).unwrap().len(), 2);
        // …and not under the other tenant's.
        assert!(
            cipher::open(theirs, key, bytes).is_err(),
            "{key} opened under the WRONG tenant's key"
        );
    }
}

#[test]
fn what_lands_in_the_zone_is_what_the_outbox_held() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-08-14");
    conn.execute("UPDATE collection SET notes = 'traded' WHERE id = 2", [])
        .unwrap();
    conn.execute("DELETE FROM collection WHERE id = 1", [])
        .unwrap();
    redate(&conn, 3, "2026-08-14");

    ship(&world, &[alice], 100);
    let (key, bytes) = world.objects().into_iter().next().unwrap();
    let tenant_key = dumpster_kit_keys::tenant_key(&env(), &world.registry(), ALICE).unwrap();
    let events = encode::decode(cipher::open(&tenant_key, &key, &bytes).unwrap()).unwrap();

    assert_eq!(
        events.iter().map(|e| e.op.as_str()).collect::<Vec<_>>(),
        ["insert", "insert", "insert", "update", "delete"]
    );
    assert!(events.iter().all(|e| e.source_table == "collection"));
    let payload: serde_json::Value = serde_json::from_str(&events[3].payload).unwrap();
    assert_eq!(payload["notes"], "traded");
    let deleted: serde_json::Value = serde_json::from_str(&events[4].payload).unwrap();
    assert_eq!(
        deleted["printing_id"], "fixture-2026-08-14-0",
        "a delete carries the pre-image, all the way to the zone"
    );
}

// ── who is shipped, and who is not ──────────────────────────────────────────

#[test]
fn a_tombstoned_tenant_is_not_shipped_and_is_not_an_anomaly() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    add_holdings(&world.collection(ALICE), 3, "2026-08-14");
    add_holdings(&world.collection(BOB), 3, "2026-08-14");

    dumpster_kit_keys::destroy::tombstone(&world.registry(), BOB, Some("account deleted")).unwrap();

    let report = ship(&world, &[alice, bob], 100);

    assert_eq!(
        report.outcome(),
        Outcome::Clean,
        "a revoked tenant is the system working, not a partial run"
    );
    assert_eq!(report.events(), 3, "only alice's");
    assert_eq!(report.tenants[1].status, Status::Revoked);
    assert!(
        world.objects().keys().all(|k| k.contains(ALICE)),
        "a tombstoned tenant's data reached the zone: {:?}",
        world.objects().keys().collect::<Vec<_>>()
    );
    // …and their outbox is untouched, so nothing was silently consumed.
    assert_eq!(cursor::shipped_thru(&world.collection(BOB)).unwrap(), 0);
}

#[test]
fn an_unregistered_tenant_is_skipped_and_named_rather_than_shipped() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    // Absence is not permission: bob exists, but nothing is recorded about
    // his key. This is what a registry restored without its rows looks like.
    world
        .registry()
        .execute(
            "DELETE FROM tenant_key WHERE database_id = ?1",
            rusqlite::params![BOB],
        )
        .unwrap();
    add_holdings(&world.collection(ALICE), 2, "2026-08-14");
    add_holdings(&world.collection(BOB), 2, "2026-08-14");

    let report = ship(&world, &[alice, bob], 100);

    assert_eq!(report.outcome(), Outcome::Partial);
    assert_eq!(report.outcome().code(), 2);
    let skipped = report.skipped();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, BOB);
    assert!(
        skipped[0].1.contains("dumpstar keys register"),
        "the skip must say what to do about it: {}",
        skipped[0].1
    );
    assert!(world.objects().keys().all(|k| k.contains(ALICE)));
}

#[test]
fn a_run_that_ships_nobody_at_all_is_a_failure_not_a_warning() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 2, "2026-08-14");
    // The systemic failure: the master key is gone, so no tenant can ship.
    std::fs::remove_file(env().key_path().unwrap()).unwrap();

    let report = ship(&world, &[alice], 100);
    assert_eq!(report.outcome(), Outcome::Failed);
    assert_eq!(report.outcome().code(), 1);
    assert!(
        report.skipped()[0].1.contains("OPERATIONAL FAILURE"),
        "a missing key must never read as a deletion: {}",
        report.skipped()[0].1
    );
    assert!(world.objects().is_empty());
}

#[test]
fn a_registered_tenant_with_no_database_is_skipped_rather_than_invented() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    add_holdings(&world.collection(ALICE), 2, "2026-08-14");
    std::fs::remove_file(&bob.path).unwrap();

    let report = ship(&world, &[alice, bob], 100);
    assert_eq!(report.outcome(), Outcome::Partial);
    assert!(report.skipped()[0].1.contains("no database at"));
}

#[test]
fn a_collection_with_an_empty_outbox_ships_nothing_and_is_clean() {
    // pd-whsw's shape: an existing collection whose outbox starts empty. The
    // shipper has nothing to say about it — the backfill (item 5) is what
    // puts those holdings through the outbox, and it does so as ordinary
    // events this code cannot tell apart from any other.
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-08-14");
    conn.execute("DELETE FROM ownership_outbox", []).unwrap();

    let report = ship(&world, &[alice], 100);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert_eq!(report.events(), 0);
    assert!(
        report.gaps().is_empty(),
        "an outbox that was never written is not a gap — nothing was lost, \
         it was never there. A gap needs a cursor that has already passed \
         numbers these rows should have followed."
    );
    assert!(world.objects().is_empty());
}

/// ...and the other half of that shape: once the backfill HAS run, those
/// holdings ship, as ordinary events.
///
/// This is the seam between item 5 (`pd-385w`) and item 4 — the two halves of
/// arming the shipper on a box that already holds cards. Both crates state
/// the claim ("the shipper must NOT branch on provenance"; "every row is
/// shipped identically") and until they lived on one branch neither could
/// show it: `dumpstar-db`'s own gate compares a backfill against a stand-in
/// projection, never against the zone the shipper writes.
///
/// The fixture is a collection whose rows PREDATE the triggers, which is
/// every existing box: the events are absent and so is the sequence they
/// would have burned. Reset one without the other and the backfill's first
/// event arrives above a cursor that never passed anything, which is a real
/// gap and would be reported as one.
#[test]
fn a_backfilled_collection_ships_as_ordinary_events() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-03-04");
    conn.execute("DELETE FROM ownership_outbox", []).unwrap();
    conn.execute(
        "DELETE FROM sqlite_sequence WHERE name = 'ownership_outbox'",
        [],
    )
    .unwrap();
    // What a site's backfill writes: one `insert` event per existing row, dated from the
    // row's own timestamp, marked `backfill`. The emitter is the site's; the shipper only
    // has to treat the result as ordinary events.
    let emitted = conn
        .execute(
            "INSERT INTO ownership_outbox (occurred_at, source_table, op, row_id, payload, source) \
             SELECT acquired_at, 'collection', 'insert', id, \
                    json_object('id', id, 'printing_id', printing_id, 'condition', condition, \
                                'acquired_at', acquired_at, 'source', source, 'notes', notes, \
                                'purchase_price', purchase_price, 'sale_price', sale_price, \
                                'graded', graded, 'status', status), \
                    'backfill' \
             FROM collection ORDER BY id",
            [],
        )
        .unwrap();
    assert_eq!(emitted, 3);

    let report = ship(&world, &[alice], 100);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert_eq!(report.events(), 3);
    assert!(
        report.gaps().is_empty(),
        "the backfill numbered from 1 and nothing was lost"
    );

    let (key, bytes) = world.objects().into_iter().next().unwrap();
    assert!(
        key.contains("as_of=2026-03-04"),
        "a backfilled event is dated from the row's own acquired_at, not from \
         the day the backfill ran — otherwise re-running it would move the \
         partition. Key was {key}"
    );
    let tenant_key = dumpster_kit_keys::tenant_key(&env(), &world.registry(), ALICE).unwrap();
    let events = encode::decode(cipher::open(&tenant_key, &key, &bytes).unwrap()).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| (e.source_table.as_str(), e.op.as_str()))
            .collect::<Vec<_>>(),
        [("collection", "insert"); 3],
        "the zone holds the holdings, described exactly as a trigger would \
         have described them"
    );
    let shipped: Vec<String> = events
        .iter()
        .map(|e| {
            serde_json::from_str::<serde_json::Value>(&e.payload).unwrap()["printing_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        shipped,
        (0..3)
            .map(|i| format!("fixture-2026-03-04-{i}"))
            .collect::<Vec<_>>()
    );

    // And it is over: a second run has nothing to add, exactly as it would
    // have after an ordinary night's shipping.
    let before = world.objects();
    let again = ship(&world, &world.tenants(), 100);
    assert_eq!(again.events(), 0);
    assert_eq!(world.objects(), before);
}

// ── the zone reduces to the collection ──────────────────────────────────────

/// **What is in the zone, reduced by the resolution rule, IS the tenant's
/// collection** — pd-mixm's third step, and the headline claim of the whole
/// inbound leg.
///
/// It is only assertable now that a shipped part decodes into
/// [`Event`] rather than a second spelling of one: the
/// reduction is [`outbox::project`], the ONE implementation of
/// rule 3, and a reader that had to rebuild an event to call it would be
/// proving something about the rebuild instead.
///
/// The fixture is a collection with a history rather than a list of inserts:
/// an update whose newer content must win, and a delete that must leave
/// nothing behind. A transport that dropped an event would still produce a
/// plausible projection — it is the *contents* that would be quietly stale,
/// which is exactly the failure a row count cannot see.
#[test]
fn a_shipped_part_projects_back_to_the_live_collection() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 4, "2026-08-13");
    conn.execute(
        "UPDATE collection SET notes = 'traded away' WHERE id = 2",
        [],
    )
    .unwrap();
    conn.execute("DELETE FROM collection WHERE id = 3", [])
        .unwrap();
    // A second day, so the zone is more than one object and the projection
    // has to be read across parts.
    add_holdings(&conn, 2, "2026-08-14");
    redate(&conn, 4, "2026-08-14");

    let report = ship(&world, &[alice], 3);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert!(world.objects().len() > 1, "the fixture spans parts");

    // Decode every part in the zone and reduce it with the collection's own
    // resolution rule. Nothing here reimplements that rule.
    let tenant_key = dumpster_kit_keys::tenant_key(&env(), &world.registry(), ALICE).unwrap();
    let mut shipped: Vec<Event> = world
        .objects()
        .into_iter()
        .flat_map(|(key, bytes)| {
            encode::decode(cipher::open(&tenant_key, &key, &bytes).unwrap()).unwrap()
        })
        .collect();
    shipped.sort_by_key(|e| e.seq);
    let zone = outbox::project(&shipped);

    // 1. The transport lost nothing: the zone reduces to what the collection's
    //    own outbox reduces to.
    assert_eq!(
        zone,
        outbox::project(&outbox::events(&conn).unwrap()),
        "the zone's projection and the outbox's are the same projection"
    );

    // 2. …and that projection is the collection, row for row.
    let mut live: Vec<(i64, Option<String>)> = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id, notes FROM collection ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .unwrap();
    for row in rows {
        live.push(row.unwrap());
    }
    assert_eq!(
        zone.keys().cloned().collect::<Vec<_>>(),
        live.iter()
            .map(|(id, _)| ("collection".to_string(), *id))
            .collect::<Vec<_>>(),
        "every holding the tenant has, and nothing they deleted"
    );
    for (id, notes) in &live {
        let held = &zone[&("collection".to_string(), *id)];
        assert_eq!(held["id"].as_i64(), Some(*id));
        assert_eq!(
            held["notes"].as_str().map(str::to_string),
            *notes,
            "row {id}: the zone holds the row's CURRENT content — an update \
             that did not reach it would leave this stale and the count right"
        );
    }
}

// ── reading it back: Phase 3's holdings source (pd-szh2) ────────────────────
//
// The tests above prove the zone HOLDS the collection. These prove the
// reader gets it back out — through the same `ObjectSource` a real job
// holds, without being told which parts exist — and stages it where a
// valuation can join it.

/// The whole round trip, as Phase 3 runs it: ship, then read the zone with
/// nothing but the tenant's id, and materialise. `zone_holdings` must be
/// `collection`, row for row and column for column.
///
/// This is the claim the equivalence proof rests on. If the two tables differ
/// here, the two valuations differ downstream for a reason that has nothing
/// to do with arithmetic.
#[test]
fn the_zone_read_back_materialises_the_collection_row_for_row() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);

    // A collection with something in every awkward column: a NULL price, a
    // real price, a note with a quote in it, and a row that was updated and
    // one that was deleted after being shipped once.
    add_holdings(&conn, 4, "2026-08-13");
    conn.execute(
        "UPDATE collection SET purchase_price = 12.5, notes = 'a \"quoted\" note', \
         condition = 'Lightly Played' WHERE id = 1",
        [],
    )
    .unwrap();
    conn.execute("DELETE FROM collection WHERE id = 3", [])
        .unwrap();
    add_holdings(&conn, 2, "2026-08-14");
    redate(&conn, 4, "2026-08-14");

    // Several parts, so the reader has to find them all rather than one.
    let report = ship(&world, &[alice], 3);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert!(world.objects().len() > 1, "the fixture spans parts");

    let key = dumpster_kit_keys::tenant_key(&env(), &world.registry(), ALICE).unwrap();
    let holdings =
        dumpster_kit_ship::zone::read(&world.zone(), &config(), &key, ALICE, SOURCES).unwrap();
    assert_eq!(
        holdings.partitions,
        vec!["2026-08-13".to_string(), "2026-08-14".to_string()],
        "both days were read — a reader that stopped at one partition would \
         value a collection at whichever day it happened to find"
    );
    assert!(holdings.declined.is_empty(), "{:?}", holdings.declined);

    dumpster_kit_ship::zone::materialize(&conn, &holdings, SOURCES, "2026-08-14T09:00:00Z")
        .unwrap();

    assert_eq!(
        dump(&conn, "zone_holdings"),
        dump(&conn, "collection"),
        "the staging table IS the collection"
    );
    let staged: i64 = conn
        .query_row("SELECT count(*) FROM zone_holdings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 5, "four inserted, one deleted, two more");
}

/// pd-bbv7. Sealed product ships exactly as singles do and now comes back the
/// same way, into its OWN staging table. Both halves in one round trip, with
/// a single and a sealed lot deliberately sharing `row_id = 1` — the ordinary
/// shape of a collection, and the shape a reduction keyed on `row_id` alone
/// silently merges.
#[test]
fn the_zone_read_back_materialises_the_sealed_lots_too() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);

    add_holdings(&conn, 3, "2026-08-13");
    add_sealed(&conn, 3, "2026-08-13");
    // Every op on the sealed side: a lot resized, a lot disposed of.
    conn.execute("UPDATE sealed_collection SET quantity = 7 WHERE id = 1", [])
        .unwrap();
    conn.execute("DELETE FROM sealed_collection WHERE id = 3", [])
        .unwrap();
    add_sealed(&conn, 1, "2026-08-14");

    let report = ship(&world, &[alice], 3);
    assert_eq!(report.outcome(), Outcome::Clean);

    let key = dumpster_kit_keys::tenant_key(&env(), &world.registry(), ALICE).unwrap();
    let holdings =
        dumpster_kit_ship::zone::read(&world.zone(), &config(), &key, ALICE, SOURCES).unwrap();
    assert!(
        holdings.declined.is_empty(),
        "sealed is materialised now, not declined: {:?}",
        holdings.declined
    );
    dumpster_kit_ship::zone::materialize(&conn, &holdings, SOURCES, "2026-08-14T09:00:00Z")
        .unwrap();

    assert_eq!(
        dump(&conn, "zone_sealed_holdings"),
        dump(&conn, "sealed_collection"),
        "the sealed staging table IS the sealed collection"
    );
    assert_eq!(
        dump(&conn, "zone_holdings"),
        dump(&conn, "collection"),
        "and the singles are still themselves — the two reductions did not mix"
    );

    let per_source: String = conn
        .query_row(
            "SELECT per_source FROM zone_holdings_run WHERE dataset = 'holdings'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let per_source: serde_json::Value = serde_json::from_str(&per_source).unwrap();
    let (cards, sealed) = (
        per_source["collection"].as_i64().unwrap(),
        per_source["sealed_collection"].as_i64().unwrap(),
    );
    assert_eq!(
        (cards, sealed),
        (3, 3),
        "the run row counts both halves: three singles, three sealed lots \
         (four added, one deleted)"
    );

    // The pair rule, stated over real shipped bytes: id 1 exists on both
    // sides and they are different holdings.
    let quantity: i64 = conn
        .query_row(
            "SELECT quantity FROM zone_sealed_holdings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(quantity, 7, "the update won, in the sealed table");
}

/// The red direction, and the only thing that proves the valuation reads the
/// ZONE rather than the collection beside it: change the collection without
/// shipping, and the staged holdings must NOT follow.
///
/// A reader that quietly fell back to `collection` — or a transform that
/// joined the wrong table — passes every test above and fails this one.
#[test]
fn a_change_that_has_not_shipped_is_not_in_the_staged_holdings() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-08-13");
    ship(&world, std::slice::from_ref(&alice), 100);

    let key = dumpster_kit_keys::tenant_key(&env(), &world.registry(), ALICE).unwrap();
    let read = |conn: &Connection| {
        let holdings =
            dumpster_kit_ship::zone::read(&world.zone(), &config(), &key, ALICE, SOURCES).unwrap();
        dumpster_kit_ship::zone::materialize(conn, &holdings, SOURCES, "2026-08-14T09:00:00Z")
            .unwrap()
    };
    assert_eq!(read(&conn), 3);
    assert_eq!(dump(&conn, "zone_holdings"), dump(&conn, "collection"));

    // Sold one and bought two, and told nobody.
    conn.execute("DELETE FROM collection WHERE id = 1", [])
        .unwrap();
    add_holdings(&conn, 2, "2026-08-14");
    redate(&conn, 3, "2026-08-14");

    assert_eq!(
        read(&conn),
        3,
        "the zone still holds three; it was not told"
    );
    assert_ne!(
        dump(&conn, "zone_holdings"),
        dump(&conn, "collection"),
        "the staged holdings track the ZONE. If these agree, the reader is \
         reading the collection and the whole Phase 3 claim is unfounded."
    );

    // …and shipping is what reconciles them, not re-reading.
    assert_eq!(ship(&world, &[alice], 100).outcome(), Outcome::Clean);
    assert_eq!(read(&conn), 4);
    assert_eq!(dump(&conn, "zone_holdings"), dump(&conn, "collection"));
}

/// A tenant reads their own partition and nobody else's, which the prefix
/// gives — and could not read another's anyway, which the key gives.
#[test]
fn a_tenant_reads_only_their_own_holdings() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    add_holdings(&world.collection(ALICE), 4, "2026-08-14");
    add_holdings(&world.collection(BOB), 2, "2026-08-14");
    assert_eq!(ship(&world, &[alice, bob], 100).outcome(), Outcome::Clean);

    let registry = world.registry();
    let alice_key = dumpster_kit_keys::tenant_key(&env(), &registry, ALICE).unwrap();
    let bob_key = dumpster_kit_keys::tenant_key(&env(), &registry, BOB).unwrap();

    assert_eq!(
        dumpster_kit_ship::zone::read(&world.zone(), &config(), &alice_key, ALICE, SOURCES)
            .unwrap()
            .rows
            .len(),
        4
    );
    assert_eq!(
        dumpster_kit_ship::zone::read(&world.zone(), &config(), &bob_key, BOB, SOURCES)
            .unwrap()
            .rows
            .len(),
        2
    );

    // Alice's key against Bob's prefix: the objects are found and none of
    // them opens. Not an empty read — a refusal.
    let crossed = dumpster_kit_ship::zone::read(&world.zone(), &config(), &alice_key, BOB, SOURCES);
    assert!(
        crossed.is_err(),
        "alice's key opened bob's partition: {crossed:?}"
    );
}

/// A tenant who has shipped nothing stages an EMPTY table, and says so in the
/// provenance row. Not "no table": the transform's refusal to value a
/// collection whose staging table is absent is what stops it valuing today
/// from a materialisation someone took last week.
#[test]
fn a_tenant_with_nothing_in_the_zone_stages_an_empty_table() {
    let world = World::new();
    world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 2, "2026-08-14"); // in the outbox, never shipped

    let key = dumpster_kit_keys::tenant_key(&env(), &world.registry(), ALICE).unwrap();
    let holdings =
        dumpster_kit_ship::zone::read(&world.zone(), &config(), &key, ALICE, SOURCES).unwrap();
    assert_eq!(holdings.parts, 0);
    assert_eq!(
        dumpster_kit_ship::zone::materialize(&conn, &holdings, SOURCES, "x").unwrap(),
        0
    );

    let (parts, rows, max_seq): (i64, i64, i64) = conn
        .query_row(
            "SELECT parts, rows, max_seq FROM zone_holdings_run WHERE dataset = 'holdings'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((parts, rows, max_seq), (0, 0, 0));
    let staged: i64 = conn
        .query_row("SELECT count(*) FROM zone_holdings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 0, "the table exists and is empty");
}

/// Every column of a table, as text, ordered — the unit "the staging table IS
/// the collection" is measured in. Text rather than typed reads so a column
/// added to `collection` is compared without this helper being touched.
fn dump(conn: &Connection, table: &str) -> Vec<String> {
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info(?1)")
        .unwrap()
        .query_map([table], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let selected = columns
        .iter()
        .map(|c| format!("quote(\"{c}\")"))
        .collect::<Vec<_>>()
        .join(" || '|' || ");
    conn.prepare(&format!("SELECT {selected} FROM \"{table}\" ORDER BY id"))
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
