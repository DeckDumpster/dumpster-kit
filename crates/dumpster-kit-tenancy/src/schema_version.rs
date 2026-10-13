//! Every database states its own schema version, and the binary refuses to operate on one it
//! does not understand.
//!
//! The mechanism is SQLite's own `PRAGMA user_version`: one integer in the file header, **per
//! file**. There is one collection database per tenant, so "which of my N databases are behind"
//! is read off each file rather than assumed.
//!
//! This is not a migration runner. Additive change still travels by re-applying
//! `CREATE … IF NOT EXISTS` on every open; `user_version` gates what that cannot express.
//!
//! | file vs binary | outcome |
//! |----------------|---------|
//! | equal          | proceed; [`stamp`] writes nothing |
//! | lower (incl. 0)| apply the schema idempotently, then [`stamp`] |
//! | higher         | **refuse to open** — [`TenancyError::SchemaVersion`] |
//!
//! The refusal is what makes rollback safe: an older binary stops rather than quietly operating
//! on a schema it does not know.
//!
//! The kernel owns the registry's spec ([`REGISTRY`]); each application declares its own for its
//! catalog and collection.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::error::{Result, TenancyError};

/// A database kind and the schema version this build understands for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SchemaSpec {
    /// How the database is named in an operator-facing error.
    pub label: &'static str,
    /// Bump when a change cannot be expressed as `CREATE … IF NOT EXISTS` — when an older binary
    /// reading the new file would be wrong rather than merely missing something.
    pub version: i64,
}

/// The user registry, `registry.sqlite` at the data root.
pub const REGISTRY: SchemaSpec = SchemaSpec {
    label: "tenant registry",
    version: 1,
};

impl SchemaSpec {
    /// Where a file carrying `found` stands relative to this build.
    pub fn state_of(self, found: i64) -> SchemaState {
        match found.cmp(&self.version) {
            std::cmp::Ordering::Less => SchemaState::Behind,
            std::cmp::Ordering::Equal => SchemaState::Current,
            std::cmp::Ordering::Greater => SchemaState::Ahead,
        }
    }
}

/// Where a database file's schema version stands relative to this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemaState {
    /// Older than this build — adopted (schema re-applied, then stamped) on its next open.
    Behind,
    /// Exactly this build's version. Opening it writes no version at all.
    Current,
    /// Newer than this build understands. Refused, not opened.
    Ahead,
}

impl SchemaState {
    pub fn label(self) -> &'static str {
        match self {
            SchemaState::Behind => "behind",
            SchemaState::Current => "current",
            SchemaState::Ahead => "ahead",
        }
    }
}

impl fmt::Display for SchemaState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl fmt::Display for SchemaSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label)
    }
}

/// The version recorded in an attached schema's file header (`main` for the connection's own).
pub fn version_of(conn: &Connection, schema: &str) -> Result<i64> {
    // `PRAGMA` takes no bind parameters, so the schema name is formatted in. It is never
    // caller-supplied data: only `main` and the kernel's own `shared`.
    Ok(conn.query_row(&format!("PRAGMA {schema}.user_version"), [], |r| r.get(0))?)
}

/// The version recorded in the connection's own database.
pub fn version(conn: &Connection) -> Result<i64> {
    version_of(conn, "main")
}

/// The version recorded in the database file at `path`, read without applying any schema,
/// without stamping and — deliberately — without the gate. The reporting path: a file from the
/// future is exactly the one an operator most needs named. Every failure names the file.
pub fn version_of_file(path: &Path) -> Result<i64> {
    read_file_version(path).map_err(|e| {
        TenancyError::Env(format!(
            "reading the schema version of {}: {e}",
            path.display()
        ))
    })
}

fn read_file_version(path: &Path) -> Result<i64> {
    // Read-write so a WAL database whose `-shm` is gone still opens; `CREATE` is left off so a
    // vanished path is an error rather than an empty database reporting a confident 0.
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    version(&conn)
}

/// The gate, for the connection's own database. Call it *before* applying the schema: a file
/// from the future must not be written to at all. Returns the version found.
pub fn gate(conn: &Connection, spec: SchemaSpec) -> Result<i64> {
    gate_schema(conn, spec, "main")
}

/// The gate for a database attached under `schema` — the read-only catalog on a tenant
/// connection. Only the refusal applies: the catalog's schema is not applied through this
/// connection, so a *lower* version is the next read-write open's to stamp, not a reason to
/// take the site down.
pub fn gate_attached(conn: &Connection, spec: SchemaSpec, schema: &str) -> Result<i64> {
    gate_schema(conn, spec, schema)
}

fn gate_schema(conn: &Connection, spec: SchemaSpec, schema: &str) -> Result<i64> {
    let found = version_of(conn, schema)?;
    if spec.state_of(found) == SchemaState::Ahead {
        return Err(TenancyError::SchemaVersion(format!(
            "the {} database at {} is schema version {found}, but this build understands \
             version {} — refusing to open it. It was written by a newer build; run that \
             build, or restore this database from a replica taken before the upgrade.",
            spec.label,
            file_of(conn, schema),
            spec.version,
        )));
    }
    Ok(found)
}

/// Record this build's version on the connection's own database. Call it *after* the schema has
/// been applied. A no-op when the file already carries the version: re-opening an up-to-date
/// database writes nothing at all.
pub fn stamp(conn: &Connection, spec: SchemaSpec) -> Result<()> {
    if version(conn)? != spec.version {
        conn.execute_batch(&format!("PRAGMA user_version = {}", spec.version))?;
    }
    Ok(())
}

/// The file backing `schema`, for the refusal message. Best-effort: an error path is no place to
/// fail.
fn file_of(conn: &Connection, schema: &str) -> String {
    conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name = ?1",
        [schema],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|f| !f.is_empty())
    .unwrap_or_else(|| format!("<{schema}>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: SchemaSpec = SchemaSpec {
        label: "collection",
        version: 2,
    };

    fn change_counter(path: &Path) -> u32 {
        let bytes = std::fs::read(path).unwrap();
        u32::from_be_bytes(bytes[24..28].try_into().unwrap())
    }

    #[test]
    fn stamp_records_the_version_and_re_stamping_is_a_no_op() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(version(&conn).unwrap(), 0);
        stamp(&conn, USER).unwrap();
        assert_eq!(version(&conn).unwrap(), 2);
        stamp(&conn, USER).unwrap();
        assert_eq!(version(&conn).unwrap(), 2);
    }

    /// "Re-opening an up-to-date database is a no-op" as a property of the bytes: SQLite bumps
    /// the file change counter on every write, so an unchanged counter proves nothing was
    /// written — and a write on every start is replicated off-box each time.
    #[test]
    fn stamping_an_already_stamped_file_writes_nothing_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE t (x)").unwrap();
            stamp(&conn, USER).unwrap();
        }
        let before = change_counter(&path);
        {
            let conn = Connection::open(&path).unwrap();
            stamp(&conn, USER).unwrap();
        }
        assert_eq!(change_counter(&path), before);
    }

    #[test]
    fn a_file_from_the_future_is_refused_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 9").unwrap();
        let err = gate(&conn, USER).unwrap_err();
        assert!(matches!(err, TenancyError::SchemaVersion(_)), "{err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("version 9") && msg.contains("future.sqlite"),
            "{msg}"
        );
    }

    #[test]
    fn behind_and_current_pass_the_gate() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(gate(&conn, USER).unwrap(), 0);
        conn.execute_batch("PRAGMA user_version = 2").unwrap();
        assert_eq!(gate(&conn, USER).unwrap(), 2);
        assert_eq!(USER.state_of(1), SchemaState::Behind);
        assert_eq!(USER.state_of(3), SchemaState::Ahead);
    }

    #[test]
    fn the_version_is_read_per_file_without_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.sqlite");
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 9")
            .unwrap();
        assert_eq!(version_of_file(&path).unwrap(), 9);
        let err = version_of_file(&dir.path().join("missing.sqlite")).unwrap_err();
        assert!(err.to_string().contains("missing.sqlite"), "{err}");
    }
}
