//! The user registry — the table that joins a *handle* to a *database*.
//!
//! Before this, a tenant's name was three things at once: the value in an
//! unauthenticated header, the filename on disk, and the S3 replica prefix.
//! One string doing all three is what made `pd-pm7b` possible — delete
//! `alice`, recreate `alice`, and the new database lands under the old
//! one's replica stream for the rest of the retention window.
//!
//! Here they are two facts joined by a row:
//!
//! * `handle` — what a request names, what a person types. Renameable.
//! * `database_id` — an opaque ULID, the stem of `tenants/<id>.sqlite`.
//!   Assigned here, never chosen by a caller, never derived from the handle.
//!
//! Recreating a released handle therefore *cannot* inherit the old
//! database or its replica: it gets a fresh ULID, so it is a different file
//! under a different prefix. Not fixed — unreachable.
//!
//! ULID because it is filename-safe and creation-ordered, so a directory
//! listing stays chronologically meaningful once the names stop being
//! human-readable. It is canonically UPPERCASE Crockford base32, which
//! [`validate_tenant_name`] rejects — that validator guards
//! caller-supplied names, and a `database_id` is not one. Turning an id
//! into a path is `pd-rqgv`'s job, not this module's.
//!
//! Resolution, the CLI, and replication all live elsewhere. This module is
//! the schema and its accessor: lookup, insert, rename, detach.
//!
//! What it deliberately is *not* is the place the rules live. `database_id`
//! is the primary key, a handle's charset is a `CHECK`, and "one live user
//! per handle" is a partial unique index over `state = 'active'` — all three
//! in `schema_registry.sql`, so they hold for every writer including an
//! operator with `sqlite3` open on the file. The functions below are the
//! ergonomic path to them, not the enforcement.

use rusqlite::{Connection, OptionalExtension, params};
use ulid::Ulid;

use crate::app::AppPrefix;
use crate::error::{Result, TenancyError};
use crate::paths::{registry_db_path, validate_tenant_name};

/// The one place the registry reads the wall clock.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Whether a registered user is live, or has released their handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserState {
    /// A live user: resolvable, and their handle is taken.
    Active,
    /// The handle was released; the database and its replica were kept.
    /// The row survives so those bytes stay attributable to someone.
    Detached,
}

impl UserState {
    /// The value stored in `user.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            UserState::Active => "active",
            UserState::Detached => "detached",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(UserState::Active),
            "detached" => Ok(UserState::Detached),
            other => Err(TenancyError::Env(format!(
                "registry: unknown user state {other:?}"
            ))),
        }
    }
}

/// One row of the registry: who they are, and where their collection lives.
///
/// `database_id` first because it is the primary key — the identity. `handle`
/// is a label on it, and a detached user keeps theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub database_id: String,
    pub handle: String,
    pub created_at: String,
    pub state: UserState,
    /// When the handle was released. `None` while active.
    pub retired_at: Option<String>,
}

const COLS: &str = "database_id, handle, created_at, state, retired_at";

/// The columns as [`COLS`] names them, still as SQLite handed them over.
type Row = (String, String, String, String, Option<String>);

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn into_user(row: Row) -> Result<User> {
    Ok(User {
        database_id: row.0,
        handle: row.1,
        created_at: row.2,
        state: UserState::parse(&row.3)?,
        retired_at: row.4,
    })
}

/// Open the registry at the data root, creating it if absent.
pub fn open(app: &AppPrefix) -> Result<Connection> {
    crate::connection::open_registry(&registry_db_path(app)?)
}

/// The **active** user registered under `handle`, if any.
///
/// Active-only, and that is the whole meaning of the word "registered" here:
/// a detached row keeps its holder's real handle, so a handle can name one
/// live user and any number of retired ones. Which of them a caller wants is
/// never in question — the live one, or nobody. Retired rows are reached by
/// `database_id` ([`find`]) or read in bulk ([`list`]), the two places that
/// are asking about a *database* rather than about a name.
///
/// At most one row can come back: `user_one_active_handle` is a unique index
/// over exactly this predicate.
///
/// The handle is a bound parameter and nothing else: it is compared against
/// a column, never concatenated, never turned into a path. An unknown
/// handle — including one full of `../` — is simply not in the table.
pub fn lookup(conn: &Connection, handle: &str) -> Result<Option<User>> {
    let row = conn
        .query_row(
            &format!("SELECT {COLS} FROM user WHERE handle = ?1 AND state = 'active'"),
            params![handle],
            from_row,
        )
        .optional()?;
    row.map(into_user).transpose()
}

/// Mint a fresh `database_id`. The one place ids come from.
///
/// A canonical ULID: 26 characters of uppercase Crockford base32, which is
/// exactly what [`crate::paths::validate_database_id`] admits — that
/// function is the gate every id passes on its way to becoming a path, and
/// this is the only thing on the far side of it.
pub fn mint_database_id() -> String {
    Ulid::generate().to_string()
}

/// Register a new user and mint their `database_id`. Returns the new row.
///
/// The id is generated here, never supplied: it is the one guarantee that
/// two users cannot be pointed at one file, and that a recycled handle
/// gets fresh storage.
///
/// Fails with [`TenancyError::Conflict`] if the handle is taken by a live user —
/// by `user_one_active_handle`, not by a check-then-insert, so two concurrent
/// creates cannot both win. A handle held only by *detached* rows is free,
/// which is the point of the index being partial.
///
/// [`validate_tenant_name`] runs first for the error message, not for the
/// guarantee: the `CHECK` on `user.handle` is the same rule and it is the one
/// that cannot be gone around.
pub fn insert(conn: &Connection, handle: &str) -> Result<User> {
    validate_tenant_name(handle)?;
    let user = User {
        database_id: mint_database_id(),
        handle: handle.to_string(),
        created_at: now_rfc3339(),
        state: UserState::Active,
        retired_at: None,
    };
    conn.execute(
        &format!("INSERT INTO user ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5)"),
        params![
            user.database_id,
            user.handle,
            user.created_at,
            user.state.as_str(),
            user.retired_at
        ],
    )
    .map_err(|e| conflict(e, format!("handle {handle:?} is already registered")))?;
    Ok(user)
}

/// Rename `from` to `to`. Returns the renamed row.
///
/// Only the `handle` column is written, and the row is addressed by its
/// `database_id` — the database, its replica prefix and the user's history
/// are all keyed on that id and cannot move because someone changed their
/// name.
pub fn rename(conn: &Connection, from: &str, to: &str) -> Result<User> {
    validate_tenant_name(to)?;
    let user = require(conn, from)?;
    conn.execute(
        "UPDATE user SET handle = ?2 WHERE database_id = ?1",
        params![user.database_id, to],
    )
    .map_err(|e| conflict(e, format!("handle {to:?} is already registered")))?;
    Ok(User {
        handle: to.to_string(),
        ..user
    })
}

/// Release `handle` and keep the database: the row goes to
/// `state = 'detached'`, stamped with when. Returns the detached row.
///
/// This is what `tenant remove` becomes. Nothing is deleted — not the file,
/// not the replica — so the retention window stops being the liability
/// `pd-pm7b` made of it and becomes a safety net. Hard deletion is a
/// separate, explicit act.
///
/// **The retired row keeps the person's real handle.** Freeing the name is
/// the index's job, not a rewrite's: `user_one_active_handle` covers only
/// `state = 'active'`, so the moment this `UPDATE` commits the handle is
/// available and the row still says whose bytes those are. An orphaned
/// database is therefore attributable by reading a column rather than by
/// parsing a composite string.
///
/// The handle is genuinely free afterwards: registering it again mints a
/// new `database_id`, so the new user gets a new file and a new replica
/// prefix. That is the property, and `handle_is_reusable_and_gets_new_storage`
/// is the test that holds it.
pub fn detach(conn: &Connection, handle: &str) -> Result<User> {
    let user = require(conn, handle)?;
    let retired_at = now_rfc3339();
    conn.execute(
        "UPDATE user SET state = ?2, retired_at = ?3 WHERE database_id = ?1",
        params![user.database_id, UserState::Detached.as_str(), retired_at],
    )?;
    Ok(User {
        state: UserState::Detached,
        retired_at: Some(retired_at),
        ..user
    })
}

/// Make a detached user live again — the inverse of [`detach`]. Returns the
/// reattached row.
///
/// Addressed by `database_id`, the same way [`delete`] is: a detached row
/// keeps its real handle, but by now that handle may belong to someone else,
/// and the id is what says which bytes are meant. The row goes back to
/// `state = 'active'` with `retired_at` cleared, and the identity bindings it
/// held come back with it through the `user_identity_sync_state` trigger.
///
/// Refuses anything that is not detached. A handle or an email that a live
/// user holds in the meantime is a [`TenancyError::Conflict`] from the partial
/// unique indexes, not from a check-then-update, and the transaction leaves
/// the row detached.
pub fn reattach(conn: &Connection, database_id: &str) -> Result<User> {
    let user = find(conn, database_id)?.ok_or_else(|| {
        TenancyError::NotFound(format!("no user with database id {database_id:?}"))
    })?;
    if user.state != UserState::Detached {
        return Err(TenancyError::Conflict(format!(
            "user {:?} is not detached — nothing to reattach",
            user.handle
        )));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE user SET state = ?2, retired_at = NULL WHERE database_id = ?1",
        params![database_id, UserState::Active.as_str()],
    )
    .map_err(|e| {
        conflict(
            e,
            format!(
                "cannot reattach {:?}: its handle or one of its identity emails is held by a \
                 live user now — `tenant rename` or `identity remove` that one first",
                user.handle
            ),
        )
    })?;
    tx.commit()?;
    Ok(User {
        state: UserState::Active,
        retired_at: None,
        ..user
    })
}

/// The user whose collection lives in `database_id`, if any.
///
/// The inverse of the map: a file on disk back to whoever it belongs to.
/// What a purge and a post-restore audit both start from.
pub fn find(conn: &Connection, database_id: &str) -> Result<Option<User>> {
    let row = conn
        .query_row(
            &format!("SELECT {COLS} FROM user WHERE database_id = ?1"),
            params![database_id],
            from_row,
        )
        .optional()?;
    row.map(into_user).transpose()
}

/// Forget a detached user entirely — the registry half of a hard delete.
/// Returns the row that was removed.
///
/// Refuses an `active` user, which is not a policy but the invariant: an
/// active row is what makes a database reachable, and dropping it would
/// leave bytes on disk that belong to nobody. [`detach`] first, deliberately,
/// then this. Deleting the file is the application's purge's half.
///
/// Identity bindings are deleted here too — they are personal data that must
/// not outlive the user row.
pub fn delete(conn: &Connection, database_id: &str) -> Result<User> {
    let user = find(conn, database_id)?.ok_or_else(|| {
        TenancyError::NotFound(format!("no user with database id {database_id:?}"))
    })?;
    if user.state == UserState::Active {
        return Err(TenancyError::Conflict(format!(
            "user {:?} is still active — detach them before forgetting the mapping",
            user.handle
        )));
    }
    // Delete identity rows first: the FK constraint on user_identity prevents
    // deleting the user row while referencing rows exist.
    identity_delete_all(conn, database_id)?;
    conn.execute(
        "DELETE FROM user WHERE database_id = ?1",
        params![database_id],
    )?;
    Ok(user)
}

/// Drop an **active** user's row without touching their database — the
/// registry half of `pd-hqee`'s rollback, and the only thing that may do it.
///
/// [`delete`] refuses an active user because dropping their row would leave
/// bytes on disk that belong to nobody. That invariant is about
/// *attributability*, not about the row: the application's un-migration renames
/// `tenants/<database_id>.sqlite` back to `tenants/<handle>.sqlite` **first**,
/// so by the time this runs the file is attributable by its own name again —
/// which is what the pre-registry layout meant by a tenant existing. Calling
/// it in the other order would produce exactly the anonymous database
/// [`delete`] exists to prevent.
///
/// It is not a detach: a rollback has to leave the handle free and the
/// registry with nothing to say about it, because the build being rolled back
/// to does not read the registry at all.
pub fn unregister(conn: &Connection, database_id: &str) -> Result<User> {
    let user = find(conn, database_id)?.ok_or_else(|| {
        TenancyError::NotFound(format!("no user with database id {database_id:?}"))
    })?;
    conn.execute(
        "DELETE FROM user WHERE database_id = ?1",
        params![database_id],
    )?;
    Ok(user)
}

/// Every registered user, detached ones included, in creation order —
/// which is `database_id` order, ULIDs being time-prefixed.
pub fn list(conn: &Connection) -> Result<Vec<User>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM user ORDER BY database_id"))?;
    let rows: Vec<_> = stmt
        .query_map([], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter().map(into_user).collect()
}

/// [`lookup`], but a missing handle is an error rather than `None`.
///
/// "Missing" includes a handle only detached rows hold: [`lookup`] is
/// active-only, so [`rename`] and [`detach`] can act on what it returns
/// without asking a second time whether the user is live.
fn require(conn: &Connection, handle: &str) -> Result<User> {
    lookup(conn, handle)?
        .ok_or_else(|| TenancyError::NotFound(format!("no user with handle {handle:?}")))
}

/// Report a UNIQUE/PRIMARY KEY violation as a conflict; anything else is
/// the SQLite error it was.
fn conflict(e: rusqlite::Error, msg: String) -> TenancyError {
    match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            TenancyError::Conflict(msg)
        }
        _ => e.into(),
    }
}

// ── IDENTITY BINDINGS ────────────────────────────────────────────────────

/// Normalise an email address for storage and comparison.
///
/// Returns `None` for an empty string; `Some(lowercased)` for everything
/// else. The normalised form is what the `email` column stores, and what
/// the `CHECK (email = lower(email))` accepts.
///
/// Two enforcers of "normalised email": this function (Rust, every accessor
/// call site) and the `CHECK` in `schema_registry.sql` (SQL, every writer).
/// They cannot share an implementation — one is a function and the other is
/// a constraint evaluated by SQLite — so they share `conformance/emails/cases.json`.
/// [`tests::the_normaliser_and_the_check_agree`] runs each case through both.
pub fn normalise_email(email: &str) -> Option<String> {
    if email.is_empty() {
        None
    } else {
        Some(email.to_lowercase())
    }
}

/// One row of `user_identity`: a verified identity bound to a tenant.
///
/// `database_id` is the stable key; `email` is the human-readable anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityBinding {
    pub database_id: String,
    pub email: String,
    pub sub: Option<String>,
    pub issuer: Option<String>,
    pub created_at: String,
}

fn identity_from_row(row: &rusqlite::Row) -> rusqlite::Result<IdentityBinding> {
    Ok(IdentityBinding {
        database_id: row.get(0)?,
        email: row.get(1)?,
        sub: row.get(2)?,
        issuer: row.get(3)?,
        created_at: row.get(4)?,
    })
}

/// Bind a verified identity to an active tenant.
///
/// `email` is normalised (lowercased) before storage. Returns the row that
/// was inserted.
///
/// Fails with [`TenancyError::NotFound`] if `database_id` names no registered
/// user, or with [`TenancyError::Conflict`] if the user is detached.
/// Fails with [`TenancyError::Conflict`] if this email is already bound to
/// another active tenant — one email, one active tenant.
pub fn identity_add(
    conn: &Connection,
    database_id: &str,
    email: &str,
    sub: Option<&str>,
    issuer: Option<&str>,
) -> Result<IdentityBinding> {
    let user = find(conn, database_id)?.ok_or_else(|| {
        TenancyError::NotFound(format!("no user with database id {database_id:?}"))
    })?;
    if user.state != UserState::Active {
        return Err(TenancyError::Conflict(format!(
            "user {:?} is detached — identity bindings require an active tenant",
            user.handle
        )));
    }
    let normalised = normalise_email(email)
        .ok_or_else(|| TenancyError::Env("email must not be empty".into()))?;
    let created_at = now_rfc3339();
    conn.execute(
        "INSERT INTO user_identity \
         (database_id, email, sub, issuer, created_at, user_state) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'active')",
        params![database_id, normalised, sub, issuer, created_at],
    )
    .map_err(|e| {
        conflict(
            e,
            format!("email {normalised:?} is already bound to an active tenant"),
        )
    })?;
    Ok(IdentityBinding {
        database_id: database_id.to_string(),
        email: normalised,
        sub: sub.map(|s| s.to_string()),
        issuer: issuer.map(|s| s.to_string()),
        created_at,
    })
}

/// Remove one identity binding. `email` is normalised before lookup.
///
/// Fails with [`TenancyError::NotFound`] if no such binding exists.
pub fn identity_remove(
    conn: &Connection,
    database_id: &str,
    email: &str,
) -> Result<IdentityBinding> {
    let normalised = normalise_email(email)
        .ok_or_else(|| TenancyError::Env("email must not be empty".into()))?;
    let binding = conn
        .query_row(
            "SELECT database_id, email, sub, issuer, created_at \
             FROM user_identity WHERE database_id = ?1 AND email = ?2",
            params![database_id, normalised],
            identity_from_row,
        )
        .optional()?
        .ok_or_else(|| {
            TenancyError::NotFound(format!(
                "no identity binding for {normalised:?} on database {database_id:?}"
            ))
        })?;
    conn.execute(
        "DELETE FROM user_identity WHERE database_id = ?1 AND email = ?2",
        params![database_id, normalised],
    )?;
    Ok(binding)
}

/// Every identity binding for one tenant, in creation order.
///
/// An empty `Vec` means the tenant has no bindings, not that it does not
/// exist. The caller decides whether that is an error.
pub fn identity_list(conn: &Connection, database_id: &str) -> Result<Vec<IdentityBinding>> {
    let mut stmt = conn.prepare(
        "SELECT database_id, email, sub, issuer, created_at \
         FROM user_identity WHERE database_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![database_id], identity_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every identity binding across all tenants, ordered by tenant then email.
///
/// The roster: everything that would answer "who can log in".
pub fn identity_roster(conn: &Connection) -> Result<Vec<IdentityBinding>> {
    let mut stmt = conn.prepare(
        "SELECT database_id, email, sub, issuer, created_at \
         FROM user_identity ORDER BY database_id, created_at",
    )?;
    let rows = stmt
        .query_map([], identity_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Delete all identity bindings for a tenant.
///
/// The deletion-obligation half: identity rows are personal data that must
/// not outlive the decision to remove a tenant. Returns the number of rows
/// removed (zero is not an error — a tenant may have no bindings).
pub fn identity_delete_all(conn: &Connection, database_id: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM user_identity WHERE database_id = ?1",
        params![database_id],
    )?)
}

/// The **active** tenant whose identity is bound to `email`, if any.
///
/// Returns the `User` row for the tenant whose `user_identity` row matches
/// this email with `user_state = 'active'`. The email is normalised before
/// the query.
///
/// Returns `None` when no active binding exists for this email — either
/// nobody registered it or the tenant was detached.
pub fn lookup_by_email(conn: &Connection, email: &str) -> Result<Option<User>> {
    let normalised = match normalise_email(email) {
        Some(e) => e,
        None => return Ok(None),
    };
    let row = conn
        .query_row(
            "SELECT u.database_id, u.handle, u.created_at, u.state, u.retired_at \
             FROM user u \
             JOIN user_identity ui ON ui.database_id = u.database_id \
             WHERE ui.email = ?1 AND ui.user_state = 'active'",
            params![normalised],
            from_row,
        )
        .optional()?;
    row.map(into_user).transpose()
}

/// Record the provider subject on the active binding for `email`, if that binding has none yet.
///
/// Called when a user signs in to the site, so a bearer token that carries only `sub` (Pocket ID
/// API access tokens) can later be resolved by [`lookup_by_sub`]. A binding that already holds a
/// different `sub` is left alone: a changed subject for the same address is a provider change to
/// surface, not to accept silently. Returns whether a row was updated.
pub fn learn_sub(conn: &Connection, email: &str, sub: &str) -> Result<bool> {
    let Some(normalised) = normalise_email(email) else {
        return Ok(false);
    };
    if sub.is_empty() {
        return Ok(false);
    }
    let n = conn.execute(
        "UPDATE user_identity SET sub = ?2 \
         WHERE email = ?1 AND user_state = 'active' AND sub IS NULL",
        params![normalised, sub],
    )?;
    Ok(n > 0)
}

/// The **active** tenant bound to the provider subject `sub`, if any. `issuer`, when the binding
/// records one, must match; a binding with no recorded issuer matches any.
pub fn lookup_by_sub(conn: &Connection, sub: &str, issuer: &str) -> Result<Option<User>> {
    if sub.is_empty() {
        return Ok(None);
    }
    let row = conn
        .query_row(
            "SELECT u.database_id, u.handle, u.created_at, u.state, u.retired_at \
             FROM user u \
             JOIN user_identity ui ON ui.database_id = u.database_id \
             WHERE ui.sub = ?1 AND (ui.issuer IS NULL OR ui.issuer = ?2) \
               AND ui.user_state = 'active'",
            params![sub, issuer],
            from_row,
        )
        .optional()?;
    row.map(into_user).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_registry;

    fn registry() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_registry(&dir.path().join("registry.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn insert_then_lookup() {
        let (_dir, conn) = registry();
        let created = insert(&conn, "alice").unwrap();
        assert_eq!(created.handle, "alice");
        assert_eq!(created.state, UserState::Active);
        // A canonical ULID: 26 characters of Crockford base32.
        assert_eq!(created.database_id.len(), 26);
        assert!(
            created
                .database_id
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        );

        assert_eq!(lookup(&conn, "alice").unwrap(), Some(created));
        assert_eq!(lookup(&conn, "bob").unwrap(), None);
    }

    #[test]
    fn the_handle_never_names_a_file() {
        // The load-bearing negative: a handle full of traversal is just a
        // string that is not in the table. Nothing constructs a path from
        // it, so there is no path to escape.
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        for hostile in [
            "../../etc/passwd",
            "../alice",
            "alice/../bob",
            "/etc/shadow",
            "alice\0",
        ] {
            assert_eq!(lookup(&conn, hostile).unwrap(), None, "{hostile:?}");
            assert!(insert(&conn, hostile).is_err(), "{hostile:?}");
        }
    }

    /// Insert a row past the accessor entirely, so the assertion is about the
    /// schema and not about Rust. Returns whatever SQLite said.
    fn raw_insert(
        conn: &Connection,
        database_id: &str,
        handle: &str,
        state: &str,
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO user (database_id, handle, created_at, state) \
             VALUES (?1, ?2, '2026-08-08T00:00:00Z', ?3)",
            params![database_id, handle, state],
        )
    }

    #[test]
    fn one_active_user_per_handle_by_schema() {
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        let err = insert(&conn, "alice").unwrap_err();
        assert!(matches!(err, TenancyError::Conflict(_)), "{err:?}");

        // Not a check-then-insert, and not the accessor being careful: the
        // partial unique index refuses a second ACTIVE alice even when the
        // accessor is bypassed entirely.
        assert!(
            raw_insert(&conn, "SOMEOTHERID", "alice", "active").is_err(),
            "the schema must refuse two active users with one handle"
        );

        // ...and admits a DETACHED one, which is the whole reason the index
        // is partial: a released handle is free while its row survives.
        raw_insert(&conn, "SOMEOTHERID", "alice", "detached")
            .expect("a detached row may share a handle with a live user");
    }

    #[test]
    fn a_database_id_is_the_primary_key() {
        // Two handles pointing at one file is the failure this forecloses,
        // and the id being the key is the epic's thesis in the schema.
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        assert!(
            raw_insert(&conn, &alice.database_id, "bob", "active").is_err(),
            "the schema must enforce database_id uniqueness"
        );
    }

    #[test]
    fn state_is_constrained_by_schema() {
        let (_dir, conn) = registry();
        assert!(
            raw_insert(&conn, "SOMEID", "alice", "banished").is_err(),
            "state must be constrained to the two values"
        );
    }

    /// The SQL half of `conformance/handles/cases.json`; the Rust half is
    /// `paths::tests::tenant_names_are_validated`, over the same list.
    ///
    /// Two things at once. First, the definition of a valid handle is part of
    /// the data model, so it holds against a writer that never goes near
    /// `validate_tenant_name` — a migration, or an operator with sqlite3 open
    /// on the file. Second, and the reason the corpus is shared: the `CHECK`
    /// and the validator must not drift. The validator is what a request is
    /// refused by at the boundary (`pd-4g7c`), and the `CHECK` is what a row is
    /// refused by; a handle either of them admits and the other does not is a
    /// request answered wrongly — 400 for a name that could have been
    /// registered, or 404 for one that could not.
    #[test]
    fn the_check_and_the_validator_agree() {
        let (_dir, conn) = registry();

        // A distinct id each time, so the CHECK is the only thing that can
        // refuse the row — a shared id would fail on the primary key and read
        // as a pass whatever the constraint did.
        for (i, case) in dumpster_kit_conformance::handle_cases().iter().enumerate() {
            let (handle, valid) = (&case.value, case.valid);
            let written = raw_insert(&conn, &format!("ID{i:024}"), handle, "active");
            assert_eq!(
                written.is_ok(),
                valid,
                "the CHECK and validate_tenant_name disagree about {handle:?}: {written:?}"
            );
            if let Err(e) = written {
                assert!(
                    e.to_string().contains("CHECK constraint failed"),
                    "{handle:?} must be refused by the CHECK, not by something else: {e}"
                );
            }
        }
    }

    #[test]
    fn rename_does_not_touch_the_database_id() {
        let (_dir, conn) = registry();
        let before = insert(&conn, "alice").unwrap();
        let after = rename(&conn, "alice", "alicia").unwrap();

        assert_eq!(after.handle, "alicia");
        assert_eq!(after.database_id, before.database_id);
        assert_eq!(after.created_at, before.created_at);
        assert_eq!(after.state, before.state);

        assert_eq!(lookup(&conn, "alice").unwrap(), None);
        assert_eq!(lookup(&conn, "alicia").unwrap(), Some(after));
    }

    #[test]
    fn rename_rejects_a_taken_or_missing_handle() {
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        insert(&conn, "bob").unwrap();

        let taken = rename(&conn, "alice", "bob").unwrap_err();
        assert!(matches!(taken, TenancyError::Conflict(_)), "{taken:?}");

        let missing = rename(&conn, "carol", "carla").unwrap_err();
        assert!(matches!(missing, TenancyError::NotFound(_)), "{missing:?}");

        // A rename that failed changed nothing.
        assert!(lookup(&conn, "alice").unwrap().is_some());
        assert!(lookup(&conn, "bob").unwrap().is_some());

        // The new handle is validated, so a rename cannot smuggle in a
        // name the registry would never have issued.
        assert!(rename(&conn, "alice", "../bob").is_err());
    }

    #[test]
    fn detach_keeps_the_row_and_releases_the_handle() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        assert_eq!(alice.retired_at, None);
        let detached = detach(&conn, "alice").unwrap();

        assert_eq!(detached.state, UserState::Detached);
        assert_eq!(detached.database_id, alice.database_id);
        assert_eq!(detached.created_at, alice.created_at);
        // The row is stamped with when the handle was released.
        assert!(detached.retired_at.is_some(), "{detached:?}");
        // The handle is released — nothing live answers to it...
        assert_eq!(lookup(&conn, "alice").unwrap(), None);
        // ...and yet the row still carries alice's REAL handle, so the file
        // on disk and its replica stay attributable to who owned them
        // without anything having to parse a composite string.
        assert_eq!(detached.handle, "alice");
        assert_eq!(find(&conn, &alice.database_id).unwrap(), Some(detached));

        // There is no second detach to do: no live user answers to the name.
        assert!(matches!(
            detach(&conn, "alice").unwrap_err(),
            TenancyError::NotFound(_)
        ));
    }

    #[test]
    fn reattach_restores_the_row_and_its_identity_bindings() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        identity_add(&conn, &alice.database_id, "alice@test.local", None, None).unwrap();
        detach(&conn, "alice").unwrap();
        assert_eq!(lookup_by_email(&conn, "alice@test.local").unwrap(), None);

        let back = reattach(&conn, &alice.database_id).unwrap();

        assert_eq!(back.state, UserState::Active);
        assert_eq!(back.retired_at, None);
        assert_eq!(back.database_id, alice.database_id);
        assert_eq!(lookup(&conn, "alice").unwrap(), Some(back.clone()));
        assert_eq!(
            lookup_by_email(&conn, "alice@test.local").unwrap(),
            Some(back)
        );
    }

    #[test]
    fn reattach_refuses_a_user_that_is_not_detached() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        assert!(matches!(
            reattach(&conn, &alice.database_id).unwrap_err(),
            TenancyError::Conflict(_)
        ));
        assert!(matches!(
            reattach(&conn, "01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap_err(),
            TenancyError::NotFound(_)
        ));
    }

    /// The handle was released, so someone else may hold it now. Reattaching
    /// must not give two live users one name, and must say how to get out.
    #[test]
    fn reattach_refuses_when_the_handle_has_been_taken() {
        let (_dir, conn) = registry();
        let first = insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();
        let second = insert(&conn, "alice").unwrap();

        let err = reattach(&conn, &first.database_id).unwrap_err();
        assert!(matches!(err, TenancyError::Conflict(_)), "{err:?}");
        assert!(err.to_string().contains("rename"), "{err}");
        assert_eq!(lookup(&conn, "alice").unwrap(), Some(second));
        assert_eq!(
            find(&conn, &first.database_id).unwrap().unwrap().state,
            UserState::Detached
        );
    }

    #[test]
    fn reattach_refuses_when_an_identity_is_bound_elsewhere_now() {
        let (_dir, conn) = registry();
        let first = insert(&conn, "alice").unwrap();
        identity_add(&conn, &first.database_id, "a@test.local", None, None).unwrap();
        detach(&conn, "alice").unwrap();
        let second = insert(&conn, "alice2").unwrap();
        identity_add(&conn, &second.database_id, "a@test.local", None, None).unwrap();

        let err = reattach(&conn, &first.database_id).unwrap_err();
        assert!(matches!(err, TenancyError::Conflict(_)), "{err:?}");
        assert_eq!(
            find(&conn, &first.database_id).unwrap().unwrap().state,
            UserState::Detached
        );
        assert_eq!(
            lookup_by_email(&conn, "a@test.local").unwrap(),
            Some(second)
        );
    }

    #[test]
    fn handle_is_reusable_and_gets_new_storage() {
        // pd-pm7b, made unreachable: recreating a released handle cannot
        // inherit the previous holder's database or replica prefix.
        let (_dir, conn) = registry();
        let first = insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();

        let second = insert(&conn, "alice").unwrap();
        assert_ne!(
            second.database_id, first.database_id,
            "a recycled handle must not inherit its predecessor's database"
        );
        assert_eq!(second.state, UserState::Active);
        assert_eq!(lookup(&conn, "alice").unwrap(), Some(second.clone()));

        // Both rows are on the books: one live, one detached.
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.iter().filter(|u| u.state == UserState::Active).count(),
            1
        );
        // And the detached row still names the old database *and* the handle
        // it was held under, which is what a restore has to be able to find.
        assert!(all.iter().any(|u| u.state == UserState::Detached
            && u.database_id == first.database_id
            && u.handle == "alice"));
    }

    #[test]
    fn a_handle_may_be_retired_any_number_of_times() {
        // Two alices released and a third live: three rows all named
        // "alice", exactly one of them active. Under a PRIMARY KEY on
        // handle this shape was unrepresentable, which is why detach used
        // to have to rewrite the name.
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();
        insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();
        let live = insert(&conn, "alice").unwrap();

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|u| u.handle == "alice"), "{all:?}");
        assert_eq!(
            all.iter()
                .filter(|u| u.state == UserState::Active)
                .collect::<Vec<_>>(),
            vec![&live]
        );
        // Every retired row keeps the handle its holder actually had — the
        // property an orphaned database is identified by.
        for u in all.iter().filter(|u| u.state == UserState::Detached) {
            validate_tenant_name(&u.handle).unwrap();
            assert!(u.retired_at.is_some(), "{u:?}");
        }
        // And each is a different database. Three alices, three files.
        let ids: std::collections::HashSet<&str> =
            all.iter().map(|u| u.database_id.as_str()).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn find_maps_a_database_back_to_its_owner() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        assert_eq!(find(&conn, &alice.database_id).unwrap(), Some(alice));
        assert_eq!(find(&conn, "NOSUCHDATABASE").unwrap(), None);
    }

    #[test]
    fn delete_refuses_an_active_user() {
        // Dropping the row of a live user would leave their bytes on disk
        // attributable to nobody. Detaching is the deliberate first step.
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        let err = delete(&conn, &alice.database_id).unwrap_err();
        assert!(matches!(err, TenancyError::Conflict(_)), "{err:?}");
        assert_eq!(lookup(&conn, "alice").unwrap(), Some(alice.clone()));

        detach(&conn, "alice").unwrap();
        let gone = delete(&conn, &alice.database_id).unwrap();
        assert_eq!(gone.database_id, alice.database_id);
        assert_eq!(gone.state, UserState::Detached);
        assert_eq!(find(&conn, &alice.database_id).unwrap(), None);
        assert_eq!(list(&conn).unwrap(), Vec::new());

        // And there is nothing left to delete twice.
        assert!(matches!(
            delete(&conn, &alice.database_id).unwrap_err(),
            TenancyError::NotFound(_)
        ));
    }

    /// The rollback's registry half: an ACTIVE row goes, the handle comes
    /// free, and nothing is left saying the user was ever registered — which
    /// is the state a build predating the registry expects to find.
    #[test]
    fn unregister_drops_an_active_row() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        // `delete` will not do this, and that is the distinction: it guards
        // attributability, and only the caller that renames the file back to
        // the handle first is entitled to bypass it.
        assert!(delete(&conn, &alice.database_id).is_err());

        let gone = unregister(&conn, &alice.database_id).unwrap();
        assert_eq!(gone, alice);
        assert_eq!(lookup(&conn, "alice").unwrap(), None);
        assert_eq!(find(&conn, &alice.database_id).unwrap(), None);
        assert_eq!(list(&conn).unwrap(), Vec::new());

        // The handle is genuinely free, not retired.
        assert!(insert(&conn, "alice").is_ok());
        // And there is nothing left to drop twice.
        assert!(matches!(
            unregister(&conn, &alice.database_id).unwrap_err(),
            TenancyError::NotFound(_)
        ));
    }

    #[test]
    fn list_is_in_creation_order() {
        let (_dir, conn) = registry();
        let a = insert(&conn, "alice").unwrap();
        let b = insert(&conn, "bob").unwrap();
        let ids: Vec<String> = list(&conn)
            .unwrap()
            .into_iter()
            .map(|u| u.database_id)
            .collect();
        let mut sorted = vec![a.database_id, b.database_id];
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn the_schema_is_applied_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.sqlite");
        let first = open_registry(&path).unwrap();
        insert(&first, "alice").unwrap();
        drop(first);

        let second = open_registry(&path).unwrap();
        assert!(lookup(&second, "alice").unwrap().is_some());
    }

    #[test]
    fn the_registry_is_created_at_the_data_root() {
        let app = AppPrefix::new("KITTEST").unwrap();
        crate::paths::with_home(&app, |home| {
            let conn = open(&app).unwrap();
            insert(&conn, "alice").unwrap();
            assert!(home.join("registry.sqlite").exists());
            // Not a tenant, and not the catalog.
            assert!(!home.join("tenants").join("registry.sqlite").exists());
        });
    }

    // ── Identity binding tests ──────────────────────────────────────────

    /// The corpus both enforcers of "normalised email" are held to:
    /// `conformance/emails/cases.json`. `valid` means the string is already in normalised form
    /// and the SQL CHECK must accept it; otherwise it is empty or has uppercase and the CHECK
    /// must reject it.
    fn email_cases() -> Vec<(String, bool)> {
        dumpster_kit_conformance::email_cases()
            .into_iter()
            .map(|c| (c.value, c.valid))
            .collect()
    }

    /// Helper: raw INSERT into user_identity, bypassing the accessor.
    fn raw_insert_identity(conn: &Connection, db_id: &str, email: &str) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO user_identity \
             (database_id, email, sub, issuer, created_at, user_state) \
             VALUES (?1, ?2, NULL, NULL, '2026-01-01T00:00:00Z', 'active')",
            params![db_id, email],
        )
    }

    /// The SQL half of the email corpus.
    ///
    /// The CHECK in `schema_registry.sql` must accept exactly the emails
    /// the email corpus marks as already-normalised (true) and reject the rest.
    /// The Rust half is [`normalise_email_produces_check_passing_values`].
    ///
    /// Two things at once: the definition of a normalised email belongs in
    /// the schema, and the CHECK and the normaliser must not drift. The
    /// normaliser is what a binding is inserted through; the CHECK is what
    /// a direct sqlite3 insert or a migration is held to.
    #[test]
    fn the_normaliser_and_the_check_agree() {
        let (_dir, conn) = registry();

        for (i, (email, already_normalised)) in email_cases().iter().enumerate() {
            // A user row for the FK.
            let db_id = format!("TEST{i:022}");
            conn.execute(
                "INSERT INTO user (database_id, handle, created_at, state) \
                 VALUES (?1, ?2, '2026-01-01T00:00:00Z', 'active')",
                params![db_id, format!("testuser{i}")],
            )
            .unwrap();

            let written = raw_insert_identity(&conn, &db_id, email);
            assert_eq!(
                written.is_ok(),
                *already_normalised,
                "the CHECK and email corpus disagree about {email:?}: {written:?}"
            );
            if let Err(ref e) = written {
                assert!(
                    e.to_string().contains("CHECK constraint failed"),
                    "{email:?} must be refused by the CHECK, not something else: {e}"
                );
            }
        }
    }

    /// normalise_email always produces a form the CHECK accepts.
    ///
    /// The SQL half above tests the CHECK in isolation; this test closes
    /// the loop: the normalised form of every non-empty email must pass
    /// the CHECK, so a caller who normalises before inserting cannot be
    /// blocked by the constraint.
    #[test]
    fn normalise_email_produces_check_passing_values() {
        let (_dir, conn) = registry();

        // One shared user for every case. The UNIQUE constraint on the
        // partial index is per-email per ACTIVE tenant; using a single
        // database_id means the constraint is on (database_id, email) which
        // is the PRIMARY KEY — so two inputs that normalise to the same email
        // are de-duped naturally and the second is a no-op rather than a
        // failure. What we are testing is the CHECK, not the UNIQUE index.
        conn.execute(
            "INSERT INTO user (database_id, handle, created_at, state) \
             VALUES ('NORMUSR', 'normusr', '2026-01-01T00:00:00Z', 'active')",
            [],
        )
        .unwrap();

        for (email, _) in email_cases().iter().filter(|(e, _)| !e.is_empty()) {
            let normalised = normalise_email(email).expect("non-empty email must normalise");
            // A UNIQUE/PK violation means the normalised form was already
            // inserted for this user — the CHECK was satisfied. Only a CHECK
            // violation means normalise_email produced a non-normalised value.
            match raw_insert_identity(&conn, "NORMUSR", &normalised) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.extended_code == 2067 /* SQLITE_CONSTRAINT_UNIQUE */
                        || e.extended_code == 1555 /* SQLITE_CONSTRAINT_PRIMARYKEY */ =>
                {
                    // same email already inserted — CHECK was satisfied
                }
                Err(e) => {
                    panic!("normalise_email({email:?}) = {normalised:?} must pass the CHECK: {e}")
                }
            }
        }
        assert!(
            normalise_email("").is_none(),
            "empty email must return None"
        );
    }

    #[test]
    fn identity_add_binds_email_to_active_tenant() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        let binding =
            identity_add(&conn, &alice.database_id, "Alice@Example.com", None, None).unwrap();
        // email is normalised to lowercase
        assert_eq!(binding.email, "alice@example.com");
        assert_eq!(binding.database_id, alice.database_id);
        assert_eq!(binding.sub, None);

        let listed = identity_list(&conn, &alice.database_id).unwrap();
        assert_eq!(listed, vec![binding]);
    }

    #[test]
    fn identity_add_refuses_a_second_active_tenant_for_one_email() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        let bob = insert(&conn, "bob").unwrap();

        identity_add(&conn, &alice.database_id, "shared@example.com", None, None).unwrap();
        let err =
            identity_add(&conn, &bob.database_id, "shared@example.com", None, None).unwrap_err();
        assert!(matches!(err, TenancyError::Conflict(_)), "{err:?}");
    }

    #[test]
    fn identity_email_freed_after_detach() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        identity_add(&conn, &alice.database_id, "alice@example.com", None, None).unwrap();

        // Detach alice: her email must be freed for re-use.
        detach(&conn, "alice").unwrap();

        // A new active tenant may now take the same email.
        let alice2 = insert(&conn, "alice").unwrap();
        identity_add(&conn, &alice2.database_id, "alice@example.com", None, None)
            .expect("same email must be bindable after the first tenant is detached");
    }

    #[test]
    fn identity_add_allows_several_emails_per_tenant() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        identity_add(&conn, &alice.database_id, "alice@example.com", None, None).unwrap();
        identity_add(
            &conn,
            &alice.database_id,
            "alice@work.example.com",
            None,
            None,
        )
        .unwrap();
        let listed = identity_list(&conn, &alice.database_id).unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn identity_remove_deletes_one_binding() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        identity_add(&conn, &alice.database_id, "alice@example.com", None, None).unwrap();
        identity_add(&conn, &alice.database_id, "alice@work.com", None, None).unwrap();

        let removed = identity_remove(&conn, &alice.database_id, "alice@example.com").unwrap();
        assert_eq!(removed.email, "alice@example.com");

        let remaining = identity_list(&conn, &alice.database_id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].email, "alice@work.com");

        // Removing the same binding twice is a not-found error.
        assert!(matches!(
            identity_remove(&conn, &alice.database_id, "alice@example.com").unwrap_err(),
            TenancyError::NotFound(_)
        ));
    }

    #[test]
    fn identity_roster_lists_all_tenants() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        let bob = insert(&conn, "bob").unwrap();
        identity_add(&conn, &alice.database_id, "alice@example.com", None, None).unwrap();
        identity_add(&conn, &bob.database_id, "bob@example.com", None, None).unwrap();

        let roster = identity_roster(&conn).unwrap();
        assert_eq!(roster.len(), 2);
    }

    #[test]
    fn identity_delete_all_clears_a_tenants_bindings() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        identity_add(&conn, &alice.database_id, "alice@example.com", None, None).unwrap();
        identity_add(&conn, &alice.database_id, "alice@work.com", None, None).unwrap();

        let n = identity_delete_all(&conn, &alice.database_id).unwrap();
        assert_eq!(n, 2);
        assert!(identity_list(&conn, &alice.database_id).unwrap().is_empty());

        // A second call removes nothing and succeeds.
        assert_eq!(identity_delete_all(&conn, &alice.database_id).unwrap(), 0);
    }

    #[test]
    fn registry_delete_removes_identity_rows() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        identity_add(&conn, &alice.database_id, "alice@example.com", None, None).unwrap();
        detach(&conn, "alice").unwrap();

        // delete() must remove the identity rows along with the user row.
        delete(&conn, &alice.database_id).unwrap();
        assert!(identity_list(&conn, &alice.database_id).unwrap().is_empty());
    }
}
