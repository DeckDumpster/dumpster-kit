//! The one connection opener.
//!
//! A tenant connection is the tenant's own collection database with the shared catalog
//! `ATTACH`ed **read-only** as `shared`, and its tables exposed as `TEMP VIEW`s so queries join
//! user and catalog data unqualified. [`connect_tenant`] is the only way the kernel hands one
//! out, and it ends in [`assert_isolated`]: a connection whose `database_list` is not exactly
//! `{main = this tenant's file, shared = the catalog, read-only}` is refused, not returned.
//!
//! Schema management is the application's, expressed as a [`TenantDbSpec`]: its collection SQL
//! (re-applied idempotently on every open), its schema versions, and an optional `prepare` hook
//! for what is application-specific (add-missing-column fixups, seeds). Every file is gated on
//! its own `PRAGMA user_version` before anything is written and stamped after.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, DatabaseName};

use crate::error::{Result, TenancyError};
use crate::schema_version::{self, SchemaSpec};

/// How long an open waits for a lock before giving up.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA_REGISTRY: &str = include_str!("schema_registry.sql");

/// What the application contributes to a tenant connection.
#[derive(Clone, Copy)]
pub struct TenantDbSpec {
    /// The collection schema, applied with `CREATE … IF NOT EXISTS` on every open.
    pub schema_sql: &'static str,
    /// The collection's schema version.
    pub collection: SchemaSpec,
    /// The shared catalog's schema version. Only the refusal applies (see
    /// [`schema_version::gate_attached`]).
    pub shared: SchemaSpec,
    /// Application-specific work on a freshly schema'd collection, run before it is stamped.
    pub prepare: fn(&Connection) -> Result<()>,
}

impl TenantDbSpec {
    /// A spec with no application-specific preparation.
    pub const fn new(schema_sql: &'static str, collection: SchemaSpec, shared: SchemaSpec) -> Self {
        TenantDbSpec {
            schema_sql,
            collection,
            shared,
            prepare: |_| Ok(()),
        }
    }
}

fn create_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
}

/// Open `path` with the busy timeout set *before* the WAL pragma: `journal_mode = WAL` needs a
/// brief write lock even when WAL is already set, and two connections opening the same file
/// together would fail immediately without it.
fn open_wal(path: &Path) -> Result<Connection> {
    create_parent(path);
    let conn = Connection::open(path)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

/// Open the user registry, creating it if absent, and apply the registry schema.
///
/// Never attaches the catalog: the registry answers one question — which database file belongs
/// to this handle or email — and joins nothing. Gated and stamped like every database, for a
/// sharper reason than most: this is the file that says whose database is whose, and an
/// unattributable collection is the failure this layout exists to prevent.
pub fn open_registry(path: &Path) -> Result<Connection> {
    let conn = open_wal(path)?;
    schema_version::gate(&conn, schema_version::REGISTRY)?;
    conn.execute_batch(SCHEMA_REGISTRY)?;
    schema_version::stamp(&conn, schema_version::REGISTRY)?;
    Ok(conn)
}

/// Apply the collection schema to any connection: gate, schema, `prepare`, stamp. For tests that
/// open an in-memory collection; [`open_tenant`] is this on a file.
pub fn init_tenant_schema(conn: &Connection, spec: &TenantDbSpec) -> Result<()> {
    schema_version::gate(conn, spec.collection)?;
    conn.execute_batch(spec.schema_sql)?;
    (spec.prepare)(conn)?;
    schema_version::stamp(conn, spec.collection)?;
    Ok(())
}

/// Open a tenant's collection database — applying its schema — **without** the catalog. For
/// work that touches only collection tables (a JSON backup). Requests never come through here:
/// [`connect_tenant`] is the request path.
pub fn open_tenant(path: &Path, spec: &TenantDbSpec) -> Result<Connection> {
    let conn = open_wal(path)?;
    init_tenant_schema(&conn, spec)?;
    Ok(conn)
}

/// A catalog path as a SQLite `file:` URI. `%`, `?` and `#` are the three characters that would
/// change which file the URI names.
fn read_only_uri(path: &Path) -> String {
    let p = path
        .display()
        .to_string()
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23");
    format!("file:{p}?mode=ro")
}

/// `ATTACH` the shared catalog read-only as `shared`, then create a `TEMP VIEW` for every
/// catalog table and view so they are queryable unqualified alongside the collection's own.
///
/// The catalog is version-gated here: this is the path a server reaches it by, and without a
/// check a catalog from a newer build would be joined against silently on every request.
///
/// A catalog name the collection itself declares gets **no view**. SQLite resolves an unqualified
/// name in `temp` before `main`, so a TEMP VIEW would shade the collection's own table and every
/// join would silently read the catalog's rows. The collection's own tables win.
pub fn attach_shared_readonly(
    conn: &Connection,
    shared_path: &Path,
    shared: SchemaSpec,
) -> Result<()> {
    conn.execute("ATTACH DATABASE ?1 AS shared", [read_only_uri(shared_path)])?;
    schema_version::gate_attached(conn, shared, "shared")?;

    let names: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM shared.sqlite_master \
             WHERE type IN ('table', 'view') \
               AND name NOT LIKE 'sqlite_%' \
               AND name NOT IN (SELECT name FROM main.sqlite_master \
                                WHERE type IN ('table', 'view'))",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for name in names {
        let quoted = name.replace('"', "\"\"");
        conn.execute_batch(&format!(
            "CREATE TEMP VIEW IF NOT EXISTS \"{quoted}\" AS SELECT * FROM shared.\"{quoted}\";"
        ))?;
    }
    Ok(())
}

/// Open a tenant's collection database with the shared catalog attached read-only, and refuse
/// the result unless [`assert_isolated`] holds. The one way the kernel hands out a connection a
/// request is served from.
pub fn connect_tenant(
    user_path: &Path,
    shared_path: &Path,
    spec: &TenantDbSpec,
) -> Result<Connection> {
    let conn = open_tenant(user_path, spec)?;
    attach_shared_readonly(&conn, shared_path, spec.shared)?;
    assert_isolated(&conn, user_path, shared_path)?;
    Ok(conn)
}

/// Refuse a connection wired to anything but this tenant's own database and the one shared
/// catalog, read-only.
///
/// Isolation is a property of what a connection can see, and what it can see is
/// `pragma_database_list`. `main` must be this tenant's file and `shared` the catalog, opened
/// read-only; a third entry is another database in query scope, which is precisely what must not
/// exist. (`temp` is SQLite's own scratch database, holding the catalog TEMP VIEWs, and is
/// skipped.)
pub fn assert_isolated(conn: &Connection, user_db: &Path, shared_db: &Path) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT name, file FROM pragma_database_list WHERE name <> 'temp' ORDER BY name",
    )?;
    let attached: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let expected = [("main", user_db), ("shared", shared_db)];
    let wrong = attached.len() != expected.len()
        || attached
            .iter()
            .zip(expected)
            .any(|((name, file), (want, path))| name != want || !same_file(file, path));
    let shared_writable = !wrong && !conn.is_readonly(DatabaseName::Attached("shared"))?;
    if wrong || shared_writable {
        let seen: Vec<String> = attached.iter().map(|(n, f)| format!("{n}={f}")).collect();
        return Err(TenancyError::Env(format!(
            "refusing a tenant connection that is not isolated: expected main={} and \
             read-only shared={}, got [{}]{}",
            user_db.display(),
            shared_db.display(),
            seen.join(", "),
            if shared_writable {
                " (shared is writable)"
            } else {
                ""
            }
        )));
    }
    Ok(())
}

/// Whether the path SQLite resolved an attachment to is the file we meant. Both sides are
/// canonicalised: a deployment may mount the data directory through a symlink, where a textual
/// comparison would not hold.
fn same_file(sqlite_path: &str, expected: &Path) -> bool {
    match (
        std::fs::canonicalize(sqlite_path),
        std::fs::canonicalize(expected),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARED: SchemaSpec = SchemaSpec {
        label: "shared catalog",
        version: 1,
    };
    const COLLECTION: SchemaSpec = SchemaSpec {
        label: "collection",
        version: 1,
    };
    const SPEC: TenantDbSpec = TenantDbSpec::new(
        "CREATE TABLE IF NOT EXISTS binders (id INTEGER PRIMARY KEY, name TEXT);",
        COLLECTION,
        SHARED,
    );

    struct Fixture {
        _dir: tempfile::TempDir,
        user: std::path::PathBuf,
        shared: std::path::PathBuf,
        other: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        let c = Connection::open(&shared).unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, name TEXT); \
             INSERT INTO cards VALUES (1, 'Luke'); PRAGMA user_version = 1;",
        )
        .unwrap();
        let other = dir.path().join("other.sqlite");
        Connection::open(&other)
            .unwrap()
            .execute_batch("CREATE TABLE secrets (v TEXT); INSERT INTO secrets VALUES ('bob');")
            .unwrap();
        Fixture {
            user: dir
                .path()
                .join("tenants")
                .join("01ARZ3NDEKTSV4RRFFQ69G5FAV.sqlite"),
            shared,
            other,
            _dir: dir,
        }
    }

    #[test]
    fn a_tenant_connection_sees_its_collection_and_the_catalog() {
        let f = fixture();
        let conn = connect_tenant(&f.user, &f.shared, &SPEC).unwrap();
        conn.execute("INSERT INTO binders (name) VALUES ('mine')", [])
            .unwrap();
        let joined: String = conn
            .query_row("SELECT name FROM cards", [], |r| r.get(0))
            .unwrap();
        assert_eq!(joined, "Luke");
    }

    #[test]
    fn the_catalog_is_read_only_through_a_tenant_connection() {
        let f = fixture();
        let conn = connect_tenant(&f.user, &f.shared, &SPEC).unwrap();
        assert!(
            conn.execute("INSERT INTO shared.cards VALUES (2, 'x')", [])
                .is_err()
        );
    }

    #[test]
    fn a_catalog_path_with_uri_metacharacters_still_names_that_file() {
        let f = fixture();
        let odd = f.shared.parent().unwrap().join("sh#ar?ed%.sqlite");
        std::fs::copy(&f.shared, &odd).unwrap();
        let conn = connect_tenant(&f.user, &odd, &SPEC).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM cards", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_collection_table_is_never_shaded_by_the_catalogs() {
        let f = fixture();
        Connection::open(&f.shared)
            .unwrap()
            .execute_batch("CREATE TABLE binders (id INTEGER, name TEXT); INSERT INTO binders VALUES (9, 'catalog');")
            .unwrap();
        let conn = connect_tenant(&f.user, &f.shared, &SPEC).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM binders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "the catalog's binders shaded the collection's");
    }

    fn bare(f: &Fixture) -> Connection {
        open_tenant(&f.user, &SPEC).unwrap()
    }

    #[test]
    fn a_third_attached_database_is_refused() {
        let f = fixture();
        let conn = bare(&f);
        attach_shared_readonly(&conn, &f.shared, SHARED).unwrap();
        conn.execute("ATTACH DATABASE ?1 AS bob", [f.other.to_str().unwrap()])
            .unwrap();
        let err = assert_isolated(&conn, &f.user, &f.shared).unwrap_err();
        assert!(err.to_string().contains("not isolated"), "{err}");
        assert!(err.to_string().contains("bob="), "{err}");
    }

    #[test]
    fn a_missing_catalog_is_refused() {
        let f = fixture();
        let conn = bare(&f);
        assert!(assert_isolated(&conn, &f.user, &f.shared).is_err());
    }

    #[test]
    fn a_writable_catalog_is_refused() {
        let f = fixture();
        let conn = bare(&f);
        conn.execute("ATTACH DATABASE ?1 AS shared", [f.shared.to_str().unwrap()])
            .unwrap();
        let err = assert_isolated(&conn, &f.user, &f.shared).unwrap_err();
        assert!(err.to_string().contains("writable"), "{err}");
    }

    #[test]
    fn a_shared_alias_for_another_file_is_refused() {
        let f = fixture();
        let conn = bare(&f);
        conn.execute("ATTACH DATABASE ?1 AS shared", [read_only_uri(&f.other)])
            .unwrap();
        assert!(assert_isolated(&conn, &f.user, &f.shared).is_err());
    }

    #[test]
    fn a_connection_to_another_tenants_file_is_refused() {
        let f = fixture();
        let conn = connect_tenant(&f.user, &f.shared, &SPEC).unwrap();
        let err = assert_isolated(&conn, &f.other, &f.shared).unwrap_err();
        assert!(err.to_string().contains("not isolated"), "{err}");
    }

    #[test]
    fn a_catalog_from_the_future_is_refused() {
        let f = fixture();
        Connection::open(&f.shared)
            .unwrap()
            .execute_batch("PRAGMA user_version = 7")
            .unwrap();
        let err = connect_tenant(&f.user, &f.shared, &SPEC).unwrap_err();
        assert!(matches!(err, TenancyError::SchemaVersion(_)), "{err:?}");
    }

    #[test]
    fn a_collection_from_the_future_is_refused_before_anything_is_written() {
        let f = fixture();
        std::fs::create_dir_all(f.user.parent().unwrap()).unwrap();
        Connection::open(&f.user)
            .unwrap()
            .execute_batch("PRAGMA user_version = 7")
            .unwrap();
        let err = open_tenant(&f.user, &SPEC).unwrap_err();
        assert!(matches!(err, TenancyError::SchemaVersion(_)), "{err:?}");
        let c = Connection::open(&f.user).unwrap();
        let tables: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='binders'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "schema was applied to a refused file");
    }

    #[test]
    fn the_prepare_hook_runs_and_every_file_is_stamped() {
        fn seed(conn: &Connection) -> Result<()> {
            conn.execute(
                "INSERT INTO binders (name) SELECT 'seed' WHERE NOT EXISTS (SELECT 1 FROM binders)",
                [],
            )?;
            Ok(())
        }
        let f = fixture();
        let spec = TenantDbSpec {
            prepare: seed,
            ..SPEC
        };
        let conn = open_tenant(&f.user, &spec).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM binders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(schema_version::version(&conn).unwrap(), 1);
        let reg = open_registry(&f.user.with_file_name("registry.sqlite")).unwrap();
        assert_eq!(
            schema_version::version(&reg).unwrap(),
            schema_version::REGISTRY.version
        );
    }

    #[test]
    fn a_registry_from_the_future_is_refused() {
        let f = fixture();
        std::fs::create_dir_all(f.user.parent().unwrap()).unwrap();
        let path = f.user.with_file_name("registry.sqlite");
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 5")
            .unwrap();
        assert!(matches!(
            open_registry(&path).unwrap_err(),
            TenancyError::SchemaVersion(_)
        ));
    }
}
