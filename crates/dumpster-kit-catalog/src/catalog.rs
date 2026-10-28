use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use dumpster_kit_tcgcsv::Fetch;
use rusqlite::{Connection, OptionalExtension, params};

use crate::alarms;
use crate::ddl::{KIT_DDL, KIT_SCHEMA_VERSION};
use crate::error::{Error, Result, io};
use crate::game::{ApplyReport, Game};
use crate::landing::{RawLanding, hash_hex};

pub const ALARM_FETCH_FAILED: &str = "fetch_failed";
pub const ALARM_APPLY_FAILED: &str = "apply_failed";

/// The clock a run reads: the day its payloads describe and the instant it ran, both as ISO text.
#[derive(Debug, Clone, Copy)]
pub struct RunContext<'a> {
    pub day: &'a str,
    pub at: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceOutcome {
    Applied {
        source: String,
        hash: String,
        report: ApplyReport,
    },
    /// The ledger already holds these bytes; nothing was applied.
    Unchanged {
        source: String,
        hash: String,
    },
    FetchFailed {
        source: String,
        message: String,
    },
    ApplyFailed {
        source: String,
        hash: String,
        message: String,
    },
}

/// The single owner of one game's `shared.sqlite`. Opening writes the kit's DDL, then the game's,
/// then the (game, schema_version) record; a file recording anything else is rebuilt from raw
/// landing. Nothing here ever opens a tenant file.
pub struct Catalog<G: Game> {
    conn: Connection,
    raw: RawLanding,
    rebuilt: bool,
    _game: PhantomData<G>,
}

fn ddl_hash<G: Game>() -> String {
    hash_hex(format!("{KIT_DDL}\n--game--\n{}", G::SCHEMA).as_bytes())
}

fn recorded_matches<G: Game>(db: &Path) -> bool {
    let Ok(conn) = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    let row: rusqlite::Result<Option<(u32, String, u32)>> = conn
        .query_row(
            "SELECT schema_version, ddl_hash, kit_version FROM catalog_schema WHERE game = ?1",
            [G::ID],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional();
    matches!(row, Ok(Some((v, h, k)))
        if v == G::SCHEMA_VERSION && h == ddl_hash::<G>() && k == KIT_SCHEMA_VERSION)
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn build<G: Game>(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(KIT_DDL)?;
    conn.execute_batch(G::SCHEMA)?;
    conn.execute(
        "INSERT INTO catalog_schema (game, schema_version, ddl_hash, kit_version) VALUES (?1, ?2, ?3, ?4)",
        params![G::ID, G::SCHEMA_VERSION, ddl_hash::<G>(), KIT_SCHEMA_VERSION],
    )?;
    Ok(conn)
}

impl<G: Game> Catalog<G> {
    /// Open `db`, rebuilding it from the raw landing under `raw_root` when it is absent or records
    /// another schema. The rebuild is made beside the old file and renamed over it.
    pub fn open(db: &Path, raw_root: &Path) -> Result<Self> {
        let raw = RawLanding::new(raw_root, G::ID);
        if db.exists() && recorded_matches::<G>(db) {
            let conn = Connection::open(db)?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            return Ok(Self {
                conn,
                raw,
                rebuilt: false,
                _game: PhantomData,
            });
        }
        let tmp = sidecar(db, ".rebuild");
        for p in [&tmp, &sidecar(&tmp, "-wal"), &sidecar(&tmp, "-shm")] {
            let _ = fs::remove_file(p);
        }
        let conn = build::<G>(&tmp)?;
        for landed in raw.all()? {
            let bytes = fs::read(&landed.path).map_err(io(&landed.path))?;
            apply_bytes::<G>(&conn, &landed.source, &landed.day, &landed.day, &bytes)?;
        }
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        drop(conn);
        for p in [sidecar(db, "-wal"), sidecar(db, "-shm")] {
            let _ = fs::remove_file(p);
        }
        let _ = fs::remove_file(sidecar(&tmp, "-wal"));
        let _ = fs::remove_file(sidecar(&tmp, "-shm"));
        fs::rename(&tmp, db).map_err(io(db))?;
        let conn = Connection::open(db)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self {
            conn,
            raw,
            rebuilt: true,
            _game: PhantomData,
        })
    }

    pub fn was_rebuilt(&self) -> bool {
        self.rebuilt
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn open_alarms(&self) -> Result<Vec<alarms::Alarm>> {
        alarms::open_alarms(&self.conn, G::ID)
    }

    /// Fetch every source of the game, land each response, and apply the ones the ledger has not
    /// seen. Each source is its own transaction: parse, `Game::apply`, ledger row and sync_state
    /// commit together or not at all. A failure raises an alarm and the run moves on to the next
    /// source; the alarm clears the next time that source applies or is unchanged.
    pub fn run(&self, fetch: &dyn Fetch, ctx: RunContext<'_>) -> Result<Vec<SourceOutcome>> {
        let mut out = Vec::new();
        for source in G::sources() {
            out.push(self.run_source(fetch, ctx, &source.name, &source.location)?);
        }
        Ok(out)
    }

    fn run_source(
        &self,
        fetch: &dyn Fetch,
        ctx: RunContext<'_>,
        name: &str,
        location: &str,
    ) -> Result<SourceOutcome> {
        let c = &self.conn;
        c.execute(
            "INSERT INTO sync_state (game, source, last_attempt_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (game, source) DO UPDATE SET last_attempt_at = ?3",
            params![G::ID, name, ctx.at],
        )?;
        let bytes = match fetch.get(location) {
            Ok(b) => b,
            Err(e) => {
                let message = e.to_string();
                self.record_failure(ALARM_FETCH_FAILED, name, &message, ctx)?;
                return Ok(SourceOutcome::FetchFailed {
                    source: name.into(),
                    message,
                });
            }
        };
        alarms::clear(c, G::ID, ALARM_FETCH_FAILED, name, ctx.at)?;
        let hash = hash_hex(&bytes);
        let seen: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM applied_payloads WHERE game = ?1 AND source = ?2 AND hash = ?3)",
            params![G::ID, name, hash],
            |r| r.get(0),
        )?;
        self.raw.land(name, ctx.day, &bytes)?;
        if seen {
            self.record_success(name, &hash, ctx)?;
            alarms::clear(c, G::ID, ALARM_APPLY_FAILED, name, ctx.at)?;
            return Ok(SourceOutcome::Unchanged {
                source: name.into(),
                hash,
            });
        }
        match apply_bytes::<G>(c, name, ctx.day, ctx.at, &bytes) {
            Ok(report) => {
                self.record_success(name, &hash, ctx)?;
                alarms::clear(c, G::ID, ALARM_APPLY_FAILED, name, ctx.at)?;
                Ok(SourceOutcome::Applied {
                    source: name.into(),
                    hash,
                    report,
                })
            }
            Err(e) => {
                let message = e.to_string();
                self.record_failure(ALARM_APPLY_FAILED, name, &message, ctx)?;
                Ok(SourceOutcome::ApplyFailed {
                    source: name.into(),
                    hash,
                    message,
                })
            }
        }
    }

    fn record_success(&self, name: &str, hash: &str, ctx: RunContext<'_>) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_state SET last_hash = ?3, last_day = ?4, last_success_at = ?5, last_error = NULL
             WHERE game = ?1 AND source = ?2",
            params![G::ID, name, hash, ctx.day, ctx.at],
        )?;
        Ok(())
    }

    fn record_failure(
        &self,
        kind: &str,
        name: &str,
        message: &str,
        ctx: RunContext<'_>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_state SET last_error = ?3 WHERE game = ?1 AND source = ?2",
            params![G::ID, name, message],
        )?;
        alarms::raise(&self.conn, G::ID, kind, name, message, ctx.at)
    }
}

/// Parse and apply one payload in a single transaction, writing its ledger row in the same one.
fn apply_bytes<G: Game>(
    conn: &Connection,
    source: &str,
    day: &str,
    at: &str,
    bytes: &[u8],
) -> Result<ApplyReport> {
    let payload = G::parse(source, bytes)?;
    let tx = conn.unchecked_transaction()?;
    let report = G::apply(&tx, &payload)?;
    let total =
        i64::try_from(report.total()).map_err(|_| Error::Apply("row count overflow".into()))?;
    tx.execute(
        "INSERT INTO applied_payloads (game, source, hash, day, applied_at, rows_changed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (game, source, hash) DO NOTHING",
        params![G::ID, source, hash_hex(bytes), day, at, total],
    )?;
    tx.commit()?;
    Ok(report)
}
