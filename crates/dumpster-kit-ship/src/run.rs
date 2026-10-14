//! The run: every registered tenant, one at a time, and what to make of how
//! it went.
//!
//! ## The unit of work is the registry
//!
//! Not `$DUMPSTAR_USER`. The transform tier learned this the expensive way
//! (`pd-s5yn`): a nightly job that operated on "the current user" reported
//! success for everybody while snapshotting one collection. Any successor to
//! it walks the registry, and so does this.
//!
//! ## One tenant's bad day is not the run's
//!
//! A tenant whose database is missing, locked, or not registered for a key is
//! **skipped and named**; the run carries on and says so at the end. The
//! alternative — the first failure ends the run — means the tenant sorted
//! first by ULID can stop everybody else from shipping, which is a much worse
//! failure than the one it prevents.
//!
//! ## Four answers, because there are four things to say
//!
//! | exit | meaning                                    | who hears about it |
//! |------|--------------------------------------------|--------------------|
//! | 0    | every tenant shipped                       | nobody             |
//! | 2    | the run finished, some tenants were skipped| a warning          |
//! | 3    | a SEQUENCE GAP — events were lost          | a page             |
//! | 1    | the run never started, or shipped nobody   | a page             |
//!
//! 2 and 3 are separated deliberately. A skipped tenant is a normal shape of
//! night — a database mid-import, a restore in flight — and a job that paged
//! for it would be a job whose pages get ignored (`pd-me6h`). A gap is the
//! opposite: it means the online and offline sides disagree and nothing else
//! in the system would ever notice. Collapsing them would make one of the two
//! wrong, and it is not obvious afterwards which.
//!
//! 1 covers "shipped nobody at all", which 2 would otherwise swallow: a
//! missing master key or an unreachable bucket makes *every* tenant skip, and
//! a run that achieved nothing is a failure however politely each individual
//! tenant declined.

use std::io::Write;

use std::path::{Path, PathBuf};

use dumpster_kit_keys::KeyError;
use dumpster_kit_keys::master::KeyEnv;
use dumpster_kit_lake::{ObjectStore, TenantDataset, TenantZoneConfig};
use dumpster_kit_tenancy::AppPrefix;
use dumpster_kit_tenancy::registry::User;
use rusqlite::Connection;

use crate::error::{Result, ShipError};
use crate::plan::Gap;
use crate::{cipher, cursor, encode, plan};

/// How many outbox rows one part holds, and therefore how much of the outbox
/// is read at a time.
///
/// Part of the addressing rather than a tuning knob — see [`crate::plan`].
/// Twenty thousand rows of JSON payload is a part of a few megabytes, which
/// is a comfortable object and a comfortable amount to hold in memory.
pub const DEFAULT_MAX_ROWS: usize = 20_000;

/// A registered user and the file their collection lives in — what the shipper needs to know
/// about one tenant. A site builds these from its registry (`tenants/<database_id>.sqlite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    pub user: User,
    /// The collection database. Derived from the id, never the handle.
    pub path: PathBuf,
}

/// Everything one shipping run needs that is not a tenant: where the zone is, who may open
/// it, and how a site opens its own tenant database (schema, version gate, pragmas).
pub struct ShipContext<'a> {
    /// The app's key-custody names.
    pub keys: &'a KeyEnv,
    /// The destination.
    pub zone: &'a dyn ObjectStore,
    /// Where the zone is rooted.
    pub config: &'a TenantZoneConfig,
    /// Answers both questions the run asks of it — who exists, and whose key may still be
    /// derived — because they are rows in one file.
    pub registry: &'a Connection,
    /// The site's one opener for a tenant database. The kit never opens a collection any
    /// other way, so the site's schema gate applies here too.
    pub open_tenant: &'a dyn Fn(&Path) -> Result<Connection>,
    /// The environment variable that names [`KEY_CRASH_AFTER_PARTS`]'s test seam, e.g.
    /// `DUMPSTAR_SHIP_CRASH_AFTER_PARTS`.
    pub crash_var: String,
    /// The site's operator command, for the registration hint (`dumpstar`).
    pub cli: String,
}

impl ShipContext<'_> {
    /// The names a site derives from its prefix.
    pub fn names(app: &AppPrefix) -> (String, String) {
        (app.var(KEY_CRASH_AFTER_PARTS), app.cli())
    }
}

/// The suffix, under the app prefix, of the test seam that makes "killed mid-run"
/// reproducible.
///
/// Set `<PREFIX>_SHIP_CRASH_AFTER_PARTS` to a number, and the process **aborts** immediately after that many
/// parts have landed in the zone and before the cursor records them — which
/// is the exact instant at-least-once delivery is about. A real `SIGKILL`
/// would test the same thing on whichever iteration it happened to land in;
/// this tests it on the one that matters, every time.
///
/// Named, announced on stderr when it fires, and never read in a run that
/// does not set it.
pub const KEY_CRASH_AFTER_PARTS: &str = "SHIP_CRASH_AFTER_PARTS";

/// What happened to one tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantOutcome {
    /// The database that was shipped.
    pub database_id: String,
    /// Whose it is, for the log line. Never a path component.
    pub handle: String,
    /// How it went.
    pub status: Status,
    /// Objects written.
    pub parts: usize,
    /// Outbox rows in them.
    pub events: usize,
    /// Sequence ranges found missing during this run.
    pub gaps: Vec<Gap>,
}

/// How one tenant's shipping ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Everything the outbox held is in the zone. Includes the common case of
    /// there being nothing to ship.
    Shipped,
    /// The key was destroyed on purpose. Not an anomaly: this is the system
    /// working, and the tenant is meant never to ship again.
    Revoked,
    /// Something stopped this tenant and not the others.
    Skipped(String),
}

/// What the whole run amounts to.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// One entry per registered tenant, in registry order.
    pub tenants: Vec<TenantOutcome>,
}

/// The run's verdict, which is also its exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Everybody shipped. Exit 0.
    Clean,
    /// The run finished; some tenants were skipped. Exit 2.
    Partial,
    /// Events were lost. Exit 3.
    Gap,
    /// Nothing shipped at all. Exit 1.
    Failed,
}

impl Outcome {
    /// The process exit status this outcome is.
    pub fn code(self) -> i32 {
        match self {
            Outcome::Clean => 0,
            Outcome::Failed => 1,
            Outcome::Partial => 2,
            Outcome::Gap => 3,
        }
    }
}

impl Report {
    /// The verdict. Gaps outrank everything: a run that both skipped a tenant
    /// and lost events is a run about the lost events.
    pub fn outcome(&self) -> Outcome {
        if self.tenants.iter().any(|t| !t.gaps.is_empty()) {
            return Outcome::Gap;
        }
        let skipped = self.skipped();
        if skipped.is_empty() {
            return Outcome::Clean;
        }
        if skipped.len() == self.tenants.len() {
            return Outcome::Failed;
        }
        Outcome::Partial
    }

    /// The tenants that did not ship, with the reason each gave.
    pub fn skipped(&self) -> Vec<(&str, &str)> {
        self.tenants
            .iter()
            .filter_map(|t| match &t.status {
                Status::Skipped(why) => Some((t.database_id.as_str(), why.as_str())),
                _ => None,
            })
            .collect()
    }

    /// Every gap this run found, tenant by tenant.
    pub fn gaps(&self) -> Vec<(&str, Gap)> {
        self.tenants
            .iter()
            .flat_map(|t| t.gaps.iter().map(|g| (t.database_id.as_str(), *g)))
            .collect()
    }

    /// Objects written across the run.
    pub fn parts(&self) -> usize {
        self.tenants.iter().map(|t| t.parts).sum()
    }

    /// Outbox rows shipped across the run.
    pub fn events(&self) -> usize {
        self.tenants.iter().map(|t| t.events).sum()
    }
}

/// Ship every registered tenant.
///
/// `registry` answers both questions the run asks of it — who exists, and
/// whose key may still be derived — because they are rows in one file
/// (`registry.sqlite`), and opening it twice would be two chances to open
/// different ones.
pub fn ship_all(ctx: &ShipContext<'_>, tenants: &[Tenant], max_rows: usize) -> Report {
    let mut report = Report::default();
    for tenant in tenants {
        let outcome = ship_one(ctx, tenant, max_rows);
        describe(&outcome);
        report.tenants.push(outcome);
    }
    report
}

/// Ship one tenant, turning anything that goes wrong into a [`Status`].
///
/// Infallible by design: this is the boundary where one tenant's problem
/// stops being the run's.
pub fn ship_one(ctx: &ShipContext<'_>, tenant: &Tenant, max_rows: usize) -> TenantOutcome {
    let mut outcome = TenantOutcome {
        database_id: tenant.user.database_id.clone(),
        handle: tenant.user.handle.clone(),
        status: Status::Shipped,
        parts: 0,
        events: 0,
        gaps: Vec::new(),
    };

    // The key first, before the database is even opened. A tombstoned tenant
    // is one whose holdings we have undertaken not to hold — reading them to
    // find out there is nothing to do would be the wrong shape of obedience.
    let key = match dumpster_kit_keys::tenant_key(ctx.keys, ctx.registry, &tenant.user.database_id)
    {
        Ok(key) => key,
        Err(e) if e.is_deliberate_revocation() => {
            outcome.status = Status::Revoked;
            return outcome;
        }
        Err(KeyError::NotRegistered { .. }) => {
            outcome.status = Status::Skipped(format!(
                "no key state is registered — `{} keys register {}` if this is a live \
                 tenant (absence is not permission)",
                ctx.cli, tenant.user.database_id
            ));
            return outcome;
        }
        Err(e) => {
            outcome.status = Status::Skipped(e.to_string());
            return outcome;
        }
    };

    match ship_tenant(ctx, tenant, &key, max_rows, &mut outcome) {
        Ok(()) => outcome,
        Err(e) => {
            outcome.status = Status::Skipped(e.to_string());
            outcome
        }
    }
}

/// The loop itself. Separated so every failure inside it is one `?` away from
/// becoming a skip, and none of them can accidentally become a panic.
fn ship_tenant(
    ctx: &ShipContext<'_>,
    tenant: &Tenant,
    key: &dumpster_kit_keys::TenantKey,
    max_rows: usize,
    outcome: &mut TenantOutcome,
) -> Result<()> {
    if !tenant.path.exists() {
        return Err(ShipError::NoDatabase {
            database_id: tenant.user.database_id.clone(),
            path: tenant.path.clone(),
        });
    }
    let conn = (ctx.open_tenant)(&tenant.path)?;

    loop {
        let from = cursor::shipped_thru(&conn)?;
        let events = cursor::read_after(&conn, from, max_rows)?;
        if events.is_empty() {
            return Ok(());
        }
        let planned = plan::plan(events, from, max_rows)?;

        // Before anything moves. Once the cursor is past a hole, nothing can
        // find it again — the rows are not there to be missed twice.
        cursor::record_gaps(&conn, &planned.gaps)?;
        outcome.gaps.extend_from_slice(&planned.gaps);

        for part in &planned.parts {
            let object_key = ctx.config.rooted(dumpster_kit_lake::range_part_key(
                &tenant.user.database_id,
                TenantDataset::Holdings,
                &part.as_of,
                part.from_seq(),
                part.to_seq(),
            )?);
            let sealed = cipher::seal(key, &object_key, &encode::encode(&part.events)?)?;
            ctx.zone
                .put(&object_key, sealed)
                .map_err(|e| ShipError::Zone(e.to_string()))?;

            outcome.parts += 1;
            outcome.events += part.events.len();
            crash_seam(&ctx.crash_var, outcome.parts);

            // AFTER the object landed, never before. This is the instant that
            // makes delivery at-least-once rather than at-most-once.
            cursor::advance(&conn, part.to_seq())?;
        }
    }
}

/// Abort if the crash seam's variable says to. See its documentation.
fn crash_seam(var: &str, parts: usize) {
    let Ok(after) = std::env::var(var) else {
        return;
    };
    let Ok(after) = after.parse::<usize>() else {
        return;
    };
    if parts >= after {
        eprintln!(
            "!! {var}={after}: aborting after {parts} part(s), with the \
             cursor deliberately not yet advanced. This is a test seam."
        );
        let _ = std::io::stderr().flush();
        std::process::abort();
    }
}

/// One line per tenant, flushed — a run over a long backlog must be visible
/// while it is running, not only in its epitaph.
fn describe(outcome: &TenantOutcome) {
    match &outcome.status {
        Status::Shipped if outcome.parts == 0 => {
            println!(
                "    {} ({}): nothing to ship",
                outcome.handle, outcome.database_id
            )
        }
        Status::Shipped => println!(
            "    {} ({}): {} event(s) in {} part(s)",
            outcome.handle, outcome.database_id, outcome.events, outcome.parts
        ),
        Status::Revoked => println!(
            "    {} ({}): revoked — not shipped, by design",
            outcome.handle, outcome.database_id
        ),
        Status::Skipped(why) => {
            // `skipped <id>:` is the shape deploy/ship.sh greps for to name
            // who was skipped in its warning.
            println!("    skipped {}: {}", outcome.database_id, first_line(why))
        }
    }
    for gap in &outcome.gaps {
        println!(
            "    !! SEQUENCE GAP {} ({}): {} — {} event(s) LOST",
            outcome.handle,
            outcome.database_id,
            gap,
            gap.events()
        );
    }
    let _ = std::io::stdout().flush();
}

/// Error text is long here on purpose; a progress line is not the place for
/// all of it.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}
