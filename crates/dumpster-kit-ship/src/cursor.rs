//! The shipper's end of a tenant's database: reading the outbox, and the two
//! tables that remember where it got to.
//!
//! Both tables are declared in [`crate::outbox::SCHEMA_SQL`], beside the
//! outbox they are about, and their names come from [`crate::outbox`] so
//! this crate never spells one twice. What is here is the accessor and the
//! ordering rules, which are the part that is easy to get wrong:
//!
//! * **the cursor advances only after the object has landed.** Written the
//!   other way round, a crash between the two would lose events instead of
//!   repeating them, and a lost event is exactly what this whole leg exists
//!   to make impossible;
//! * **the cursor never moves backwards.** Not because a re-ship would be
//!   harmful — it is idempotent by construction — but because a cursor that
//!   went backwards is a bug that would otherwise show up only as mysterious
//!   duplicate traffic weeks later;
//! * **a gap is recorded before the cursor passes it.** Once the cursor is
//!   past, nothing can detect that hole again — the rows are not there to be
//!   missed a second time. Recording first is what makes "a gap is detectable"
//!   survive the process that detected it.

use crate::outbox::{CURSOR_TABLE, GAP_TABLE, TABLE};
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;
use crate::plan::{Event, Gap};

/// One recorded gap, as the ledger holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapRow {
    /// The first missing sequence number.
    pub from_seq: i64,
    /// The last missing sequence number.
    pub to_seq: i64,
    /// When the shipper noticed (RFC 3339).
    pub detected_at: String,
}

/// The highest sequence number known to be in the tenant zone.
///
/// Zero for a collection that has never shipped, which is also the right
/// answer for one that has never had an event: the first `seq` is 1.
pub fn shipped_thru(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            &format!("SELECT shipped_thru FROM {CURSOR_TABLE} WHERE id = 1"),
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Record that everything up to and including `seq` is in the zone.
///
/// Refuses to move backwards — see the module docs.
pub fn advance(conn: &Connection, seq: i64) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        &format!(
            "INSERT INTO {CURSOR_TABLE} (id, shipped_thru, shipped_at) VALUES (1, ?1, ?2) \
             ON CONFLICT(id) DO UPDATE SET shipped_thru = ?1, shipped_at = ?2 \
             WHERE ?1 > shipped_thru"
        ),
        params![seq, now],
    )?;
    Ok(())
}

/// Write `gaps` to the ledger, keeping whatever is already there.
///
/// Idempotent: the primary key is the range itself, so re-detecting a gap
/// before the cursor has passed it keeps the first `detected_at` rather than
/// refreshing it. When it was noticed is part of what the record is for.
/// Returns how many were new.
pub fn record_gaps(conn: &Connection, gaps: &[Gap]) -> Result<usize> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut written = 0;
    for gap in gaps {
        // `ON CONFLICT DO NOTHING`, deliberately not `INSERT OR IGNORE`:
        // the latter swallows CHECK violations as well as key collisions, so
        // a backwards range — which is a bug in the detector, not a duplicate
        // — would vanish instead of failing.
        written += conn.execute(
            &format!(
                "INSERT INTO {GAP_TABLE} (from_seq, to_seq, detected_at) \
                 VALUES (?1, ?2, ?3) ON CONFLICT(from_seq, to_seq) DO NOTHING"
            ),
            params![gap.from_seq, gap.to_seq, now],
        )?;
    }
    Ok(written)
}

/// Every gap ever recorded against this collection, lowest first.
pub fn gaps(conn: &Connection) -> Result<Vec<GapRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT from_seq, to_seq, detected_at FROM {GAP_TABLE} ORDER BY from_seq"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(GapRow {
            from_seq: r.get(0)?,
            to_seq: r.get(1)?,
            detected_at: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The next `limit` outbox rows after `after`, in sequence order.
///
/// Reading a window rather than the whole table is what bounds a run's memory
/// against a collection that has been offline for a month. The window is the
/// caller's `max_rows`, which is also the part size — so a window boundary is
/// always a part boundary, and resuming reproduces exactly the objects an
/// uninterrupted run would have written.
pub fn read_after(conn: &Connection, after: i64, limit: usize) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT seq, occurred_at, source_table, op, row_id, payload, source \
         FROM {TABLE} WHERE seq > ?1 ORDER BY seq LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![after, limit as i64], |r| {
        Ok(Event {
            seq: r.get(0)?,
            occurred_at: r.get(1)?,
            source_table: r.get(2)?,
            op: r.get(3)?,
            row_id: r.get(4)?,
            payload: r.get(5)?,
            // Carried, never read here — see `encode`. The shipper does not
            // branch on where a row came from.
            source: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// How many outbox rows are still unshipped.
pub fn pending(conn: &Connection) -> Result<i64> {
    let cursor = shipped_thru(conn)?;
    Ok(conn.query_row(
        &format!("SELECT count(*) FROM {TABLE} WHERE seq > ?1"),
        params![cursor],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database with the fixture catalog's schema, and nothing in it.
    fn user_db() -> (tempfile::TempDir, Connection) {
        crate::test_support::collection_db()
    }

    fn gap(from_seq: i64, to_seq: i64) -> Gap {
        Gap { from_seq, to_seq }
    }

    #[test]
    fn a_collection_that_has_never_shipped_is_at_zero() {
        let (_d, conn) = user_db();
        assert_eq!(shipped_thru(&conn).unwrap(), 0);
    }

    #[test]
    fn the_cursor_advances_and_stays_advanced() {
        let (_d, conn) = user_db();
        advance(&conn, 7).unwrap();
        assert_eq!(shipped_thru(&conn).unwrap(), 7);
        advance(&conn, 19).unwrap();
        assert_eq!(shipped_thru(&conn).unwrap(), 19);
    }

    #[test]
    fn the_cursor_never_moves_backwards() {
        let (_d, conn) = user_db();
        advance(&conn, 19).unwrap();
        advance(&conn, 4).unwrap();
        assert_eq!(
            shipped_thru(&conn).unwrap(),
            19,
            "a backwards cursor would re-ship silently forever"
        );
    }

    #[test]
    fn there_is_only_ever_one_cursor_row() {
        let (_d, conn) = user_db();
        advance(&conn, 3).unwrap();
        advance(&conn, 5).unwrap();
        let rows: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {CURSOR_TABLE}"), [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
        // …and the CHECK is what enforces it against a hand-written row.
        assert!(
            conn.execute(
                &format!(
                    "INSERT INTO {CURSOR_TABLE} (id, shipped_thru, shipped_at) \
                     VALUES (2, 99, 'x')"
                ),
                [],
            )
            .is_err()
        );
    }

    #[test]
    fn a_gap_is_recorded_once_and_keeps_its_first_sighting() {
        let (_d, conn) = user_db();
        assert_eq!(record_gaps(&conn, &[gap(3, 4)]).unwrap(), 1);
        let first = gaps(&conn).unwrap();
        assert_eq!(
            record_gaps(&conn, &[gap(3, 4)]).unwrap(),
            0,
            "already known"
        );
        assert_eq!(gaps(&conn).unwrap(), first);
    }

    #[test]
    fn gaps_come_back_in_order() {
        let (_d, conn) = user_db();
        record_gaps(&conn, &[gap(30, 30), gap(3, 4), gap(11, 20)]).unwrap();
        assert_eq!(
            gaps(&conn)
                .unwrap()
                .into_iter()
                .map(|g| (g.from_seq, g.to_seq))
                .collect::<Vec<_>>(),
            [(3, 4), (11, 20), (30, 30)]
        );
    }

    #[test]
    fn a_backwards_range_is_not_a_gap() {
        let (_d, conn) = user_db();
        assert!(record_gaps(&conn, &[gap(9, 3)]).is_err());
    }

    // ── reading the outbox ──────────────────────────────────────────────────

    fn append(conn: &Connection, seq: i64) {
        conn.execute(
            &format!(
                "INSERT INTO {TABLE} (seq, occurred_at, source_table, op, row_id, payload) \
                 VALUES (?1, '2026-08-14T00:00:00.000Z', 'collection', 'insert', ?1, '{{}}')"
            ),
            params![seq],
        )
        .unwrap();
    }

    #[test]
    fn reading_starts_after_the_cursor_and_is_bounded() {
        let (_d, conn) = user_db();
        for seq in 1..=10 {
            append(&conn, seq);
        }
        let seqs: Vec<i64> = read_after(&conn, 4, 3)
            .unwrap()
            .into_iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, [5, 6, 7]);
        assert!(read_after(&conn, 10, 100).unwrap().is_empty());
    }

    #[test]
    fn pending_counts_what_is_left() {
        let (_d, conn) = user_db();
        for seq in 1..=10 {
            append(&conn, seq);
        }
        assert_eq!(pending(&conn).unwrap(), 10);
        advance(&conn, 6).unwrap();
        assert_eq!(pending(&conn).unwrap(), 4);
    }
}
