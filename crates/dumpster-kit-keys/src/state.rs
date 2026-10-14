//! The key-state registry: `database_id` → may this key still be derived?
//!
//! It is a table in `registry.sqlite`, the file that already answers "who
//! exists on this box". Three reasons it lives there rather than in a file
//! of its own:
//!
//! * it is keyed on `database_id`, which is that file's primary key;
//! * it is already replicated off-box by the Litestream sidecar and is the
//!   **first** thing restored in the total-loss procedure (`deploy/RESTORE.md`
//!   Scenario C) — so a tombstone survives a restore, which is the whole
//!   point of recording one;
//! * a second database would be a second thing to replicate, a second thing
//!   to restore, and a second chance to restore it in the wrong order.
//!
//! The table's own rules (no foreign key, absence is not permission, a
//! tombstone is terminal) live in `schema_registry.sql` where the reasoning
//! belongs. This module is the accessor.
//!
//! ## What this module deliberately cannot do
//!
//! Touch the master key. Nothing here opens, reads, writes or knows the path
//! of `tenant-master.key`. That is not an accident of the current code — it
//! is asserted by `tests/separation.rs`, because the destruction path runs
//! through this module and the backup path runs through the key file, and the
//! two staying apart is what keeps "we lost it" from ever being recorded as
//! "we destroyed it".

use dumpster_kit_tenancy::AppPrefix;
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{KeyError, Result};

/// Whether a tenant's key may still be derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    /// The key may be derived.
    Active,
    /// It was destroyed on purpose. Derivation refuses, permanently.
    Tombstoned,
}

impl KeyState {
    /// The value stored in `tenant_key.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyState::Active => "active",
            KeyState::Tombstoned => "tombstoned",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(KeyState::Active),
            "tombstoned" => Ok(KeyState::Tombstoned),
            other => Err(KeyError::InvalidDatabaseId(format!(
                "key state registry: unknown key state {other:?}"
            ))),
        }
    }
}

/// One row: what is known about one database's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantKeyState {
    /// The database this is about.
    pub database_id: String,
    /// Derivable, or revoked.
    pub state: KeyState,
    /// When it was first registered (RFC 3339).
    pub created_at: String,
    /// When it was revoked. `None` while active.
    pub tombstoned_at: Option<String>,
    /// Whatever the operator said at the time.
    pub reason: Option<String>,
}

const COLS: &str = "database_id, state, created_at, tombstoned_at, reason";

/// The columns as [`COLS`] names them, still as SQLite handed them over.
type Row = (String, String, String, Option<String>, Option<String>);

fn into_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn into_state(row: Row) -> Result<TenantKeyState> {
    Ok(TenantKeyState {
        database_id: row.0,
        state: KeyState::parse(&row.1)?,
        created_at: row.2,
        tombstoned_at: row.3,
        reason: row.4,
    })
}

/// Open the registry this box's data directory holds, creating it if absent.
///
/// The same file, opened the same way, as the site's `tenant list` — through
/// the site's database crate, so the schema gate applies here too and this crate never
/// becomes a second, laxer way into the registry.
pub fn open(app: &AppPrefix) -> Result<Connection> {
    Ok(dumpster_kit_tenancy::registry::open(app)?)
}

/// Reject anything that is not a `database_id`, before it reaches a query.
///
/// Borrowed wholesale from the tenancy kernel's `paths` rather than restated: one rule
/// for what a `database_id` is, and this crate is not where a second one
/// gets invented.
fn check_id(database_id: &str) -> Result<()> {
    dumpster_kit_tenancy::validate_database_id(database_id)
        .map_err(|e| KeyError::InvalidDatabaseId(e.to_string()))
}

/// What is recorded about `database_id`, if anything.
pub fn find(conn: &Connection, database_id: &str) -> Result<Option<TenantKeyState>> {
    check_id(database_id)?;
    let row = conn
        .query_row(
            &format!("SELECT {COLS} FROM tenant_key WHERE database_id = ?1"),
            params![database_id],
            into_row,
        )
        .optional()?;
    row.map(into_state).transpose()
}

/// Every row, oldest first.
pub fn list(conn: &Connection) -> Result<Vec<TenantKeyState>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM tenant_key ORDER BY created_at, database_id"
    ))?;
    let rows: Vec<_> = stmt
        .query_map([], into_row)?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter().map(into_state).collect()
}

/// Record that `database_id`'s key may be derived.
///
/// Idempotent for an already-active id — provisioning the same tenant twice
/// is not an error. **Not** idempotent for a tombstoned one: that is
/// [`KeyError::TombstoneIsTerminal`], because "register" quietly reactivating
/// a revoked tenant is precisely the accident this registry exists to make
/// impossible.
pub fn register(conn: &Connection, database_id: &str) -> Result<TenantKeyState> {
    check_id(database_id)?;
    if let Some(existing) = find(conn, database_id)? {
        return match existing.state {
            KeyState::Active => Ok(existing),
            KeyState::Tombstoned => Err(KeyError::TombstoneIsTerminal {
                database_id: database_id.to_string(),
            }),
        };
    }
    conn.execute(
        "INSERT INTO tenant_key (database_id, state, created_at, tombstoned_at, reason) \
         VALUES (?1, 'active', ?2, NULL, NULL)",
        params![database_id, now()],
    )?;
    Ok(find(conn, database_id)?.expect("just inserted"))
}

/// Record that `database_id`'s key was **destroyed on purpose**.
///
/// This is the one write that revokes anything, and [`crate::destroy`] is the
/// only caller — the path is deliberately a single narrow door rather than a
/// verb available anywhere a `Connection` is. Idempotent: tombstoning an
/// already-tombstoned id keeps the FIRST tombstone, timestamp and reason
/// intact, because when it happened is part of the record.
pub(crate) fn tombstone(
    conn: &Connection,
    database_id: &str,
    reason: Option<&str>,
) -> Result<TenantKeyState> {
    check_id(database_id)?;
    if let Some(existing) = find(conn, database_id)?
        && existing.state == KeyState::Tombstoned
    {
        return Ok(existing);
    }

    // An id nobody registered can still be tombstoned, and that is deliberate:
    // deletion must not depend on provisioning having been tidy. The row is
    // created straight into the revoked state.
    let stamp = now();
    conn.execute(
        "INSERT INTO tenant_key (database_id, state, created_at, tombstoned_at, reason) \
              VALUES (?1, 'tombstoned', ?2, ?2, ?3) \
         ON CONFLICT(database_id) DO UPDATE SET \
              state = 'tombstoned', tombstoned_at = ?2, reason = ?3",
        params![database_id, stamp, reason],
    )?;
    Ok(find(conn, database_id)?.expect("just written"))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An in-memory registry carrying the real schema — the same file
    /// the tenancy kernel applies, so this crate is never testing a schema of its
    /// own invention.
    pub(crate) fn registry() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!(
            "../../dumpster-kit-tenancy/src/schema_registry.sql"
        ))
        .unwrap();
        conn
    }

    const A: &str = "01J0000000000000000000000A";
    const B: &str = "01J0000000000000000000000B";

    #[test]
    fn an_unregistered_id_is_absent_not_active() {
        let conn = registry();
        assert_eq!(find(&conn, A).unwrap(), None);
    }

    #[test]
    fn registering_is_idempotent() {
        let conn = registry();
        let first = register(&conn, A).unwrap();
        let second = register(&conn, A).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.state, KeyState::Active);
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn a_tombstone_is_terminal() {
        let conn = registry();
        register(&conn, A).unwrap();
        tombstone(&conn, A, Some("account closed")).unwrap();

        let err = register(&conn, A).unwrap_err();
        assert!(matches!(err, KeyError::TombstoneIsTerminal { .. }), "{err}");
        assert_eq!(find(&conn, A).unwrap().unwrap().state, KeyState::Tombstoned);
    }

    /// Tombstoning twice keeps the first record. When a key was destroyed is
    /// part of what the tombstone is for.
    #[test]
    fn tombstoning_is_idempotent_and_keeps_the_first_record() {
        let conn = registry();
        register(&conn, A).unwrap();
        let first = tombstone(&conn, A, Some("account closed")).unwrap();
        let again = tombstone(&conn, A, Some("something else entirely")).unwrap();
        assert_eq!(first, again);
        assert_eq!(again.reason.as_deref(), Some("account closed"));
    }

    /// Deletion must not depend on provisioning having been tidy.
    #[test]
    fn an_unregistered_id_can_still_be_tombstoned() {
        let conn = registry();
        let row = tombstone(&conn, B, None).unwrap();
        assert_eq!(row.state, KeyState::Tombstoned);
        assert!(row.tombstoned_at.is_some());
    }

    #[test]
    fn one_tombstone_touches_no_other_tenant() {
        let conn = registry();
        register(&conn, A).unwrap();
        register(&conn, B).unwrap();
        tombstone(&conn, A, None).unwrap();
        assert_eq!(find(&conn, B).unwrap().unwrap().state, KeyState::Active);
    }

    #[test]
    fn a_non_database_id_never_reaches_a_query() {
        let conn = registry();
        for bad in [
            "",
            "alice",
            "../../etc/passwd",
            "01j0000000000000000000000a",
        ] {
            assert!(matches!(
                find(&conn, bad).unwrap_err(),
                KeyError::InvalidDatabaseId(_)
            ));
            assert!(matches!(
                register(&conn, bad).unwrap_err(),
                KeyError::InvalidDatabaseId(_)
            ));
            assert!(matches!(
                tombstone(&conn, bad, None).unwrap_err(),
                KeyError::InvalidDatabaseId(_)
            ));
        }
    }

    /// Rule 3 in the schema, held against a writer that never enters this
    /// crate: an operator with `sqlite3` open on the file cannot half-lift a
    /// tombstone either.
    #[test]
    fn the_check_constraint_holds_a_raw_writer_to_the_same_rule() {
        let conn = registry();
        register(&conn, A).unwrap();
        tombstone(&conn, A, None).unwrap();

        // state cleared, tombstoned_at left behind
        assert!(
            conn.execute(
                "UPDATE tenant_key SET state = 'active' WHERE database_id = ?1",
                params![A]
            )
            .is_err(),
            "the CHECK must refuse a half-lifted tombstone"
        );
        // …and the reverse.
        assert!(
            conn.execute(
                "UPDATE tenant_key SET tombstoned_at = NULL WHERE database_id = ?1",
                params![A]
            )
            .is_err()
        );
        // A state that is neither is not a state.
        assert!(
            conn.execute(
                "INSERT INTO tenant_key (database_id, state, created_at) VALUES (?1, 'maybe', 'x')",
                params![B]
            )
            .is_err()
        );
    }

    /// Rule 1 in the schema: the tombstone outlives the user row. Deleting a
    /// user must not un-revoke their key — if it did, `tenant purge` would
    /// undo the revocation it is part of.
    #[test]
    fn deleting_the_user_row_leaves_the_tombstone_standing() {
        let conn = registry();
        conn.execute(
            "INSERT INTO user (database_id, handle, created_at, state) \
             VALUES (?1, 'alice', '2026-08-13T00:00:00Z', 'active')",
            params![A],
        )
        .unwrap();
        register(&conn, A).unwrap();
        tombstone(&conn, A, Some("account deleted")).unwrap();

        conn.execute("DELETE FROM user WHERE database_id = ?1", params![A])
            .unwrap();

        let row = find(&conn, A).unwrap().expect("the tombstone must survive");
        assert_eq!(row.state, KeyState::Tombstoned);
    }
}
