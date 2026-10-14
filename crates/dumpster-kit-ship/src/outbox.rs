//! The ownership outbox as the shipper sees it: the row type, the table names, the one
//! reduction, and the DDL for the tables the kit owns.
//!
//! A site owns the *triggers* — which of its tables are holdings, and what their payload is.
//! The kit owns everything downstream of a row landing in `ownership_outbox`: the cursor
//! and gap ledger, the part layout, the envelope, the reduction. `Event` is the one struct
//! for one table; a column added to the outbox travels to the encoder's schema through a
//! compile error rather than through somebody noticing.

use std::collections::BTreeMap;

use rusqlite::Connection;

/// The outbox table's name. The one place it is spelled in Rust.
pub const TABLE: &str = "ownership_outbox";

/// The shipper's position in the outbox: the highest `seq` known to be in the tenant zone.
pub const CURSOR_TABLE: &str = "ownership_outbox_cursor";

/// Sequence ranges the shipper found MISSING — events that were lost.
pub const GAP_TABLE: &str = "ownership_outbox_gap";

/// The DDL for the outbox, its cursor and its gap ledger. A site applies it beside its own
/// holdings tables and writes its own triggers into [`TABLE`].
///
/// `source` is provenance and the one column consumers must not branch on: a trigger writes
/// `trigger`, a backfill `backfill`, a redrive `redrive`, and the shipper handles all three
/// identically.
pub const SCHEMA_SQL: &str = include_str!("schema_outbox.sql");

/// One outbox event, as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub seq: i64,
    pub occurred_at: String,
    pub source_table: String,
    pub op: String,
    pub row_id: i64,
    pub payload: String,
    pub source: String,
}

/// Every event in the outbox, in `seq` order.
pub fn events(conn: &Connection) -> rusqlite::Result<Vec<Event>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT seq, occurred_at, source_table, op, row_id, payload, source \
         FROM {TABLE} ORDER BY seq"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(Event {
            seq: r.get(0)?,
            occurred_at: r.get(1)?,
            source_table: r.get(2)?,
            op: r.get(3)?,
            row_id: r.get(4)?,
            payload: r.get(5)?,
            source: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// **The only implementation of the resolution rule**: reduce an event stream to the
/// holdings it describes, resolving conflicts by `occurred_at` and tie-breaking by `seq`.
///
/// A redrive appends a snapshot with a NEW, higher `seq`; resolving by `seq` alone would let
/// a stale snapshot overwrite a live mutation that landed in between. `occurred_at` cannot,
/// because a re-emission is dated with the row's own last-known change time. `seq` breaks
/// exact ties. A `delete` removes the holding; `insert` and `update` are the same upsert
/// because the payload is the whole row in both cases.
pub fn project(events: &[Event]) -> BTreeMap<(String, i64), serde_json::Value> {
    let mut ordered: Vec<&Event> = events.iter().collect();
    ordered.sort_by(|a, b| {
        a.occurred_at
            .cmp(&b.occurred_at)
            .then_with(|| a.seq.cmp(&b.seq))
    });

    let mut state = BTreeMap::new();
    for e in ordered {
        let key = (e.source_table.clone(), e.row_id);
        if e.op == "delete" {
            state.remove(&key);
        } else {
            // A payload that will not parse is a corrupted event, and a projection that
            // silently skipped it would under-report a holding.
            let value = serde_json::from_str(&e.payload)
                .unwrap_or_else(|_| serde_json::Value::String(e.payload.clone()));
            state.insert(key, value);
        }
    }
    state
}
