//! Turning a stretch of outbox rows into parts, and noticing what is missing.
//!
//! This module is pure — rows in, parts and gaps out, no database, no clock,
//! no object store. That is what lets the two properties the whole item is
//! about be asserted without standing anything up: **the same rows always
//! produce the same parts**, and **a hole in the sequence is seen**.
//!
//! ## What a part is
//!
//! A maximal run of consecutive rows sharing one UTC date, capped at
//! `max_rows`. Three boundaries, and each is there for its own reason:
//!
//! * **the date changes** — `as_of` is a partition, so a part cannot span
//!   two of them;
//! * **the sequence jumps** — a part's name is its range, and a part that
//!   skipped a number would be a range whose length lied about its contents.
//!   Splitting there keeps `to - from + 1 == rows` true of every part ever
//!   written, which is the invariant a reader can check without the outbox;
//! * **`max_rows`** — so a backlog is many objects rather than one enormous
//!   one.
//!
//! `max_rows` is therefore part of the addressing, not a tuning knob: change
//! it and a re-ship of the same rows produces differently-named parts holding
//! the same events. That is harmless (a reader keys on `seq`, and every copy
//! of a `seq` carries identical bytes) but it is not free, so it is a flag
//! with a stated default rather than something a caller passes casually.
//!
//! ## What a gap is
//!
//! The outbox's `seq` is `INTEGER PRIMARY KEY AUTOINCREMENT` written by a
//! trigger inside the mutating transaction. `pd-5m54` proved the three
//! properties that make a hole in it meaningful: every mutation appends
//! exactly one row, a rolled-back mutation burns no number, and a trimmed
//! number is never handed out again. So the numbers this shipper does not see
//! are not numbers that were skipped — they are events that existed and were
//! **lost**.
//!
//! The first expected number is `cursor + 1`, which is why a gap at the head
//! counts: rows deleted after the cursor passed them are gone legitimately,
//! and rows deleted *before* it reached them are not.

use crate::error::{Result, ShipError};

/// One outbox row, exactly as it was written — [`crate::outbox::Event`], the one struct for
/// the one table. A reader can reduce a shipped part with [`crate::outbox::project`] because
/// what comes out of a part is the same `Event` that went into the outbox.
pub use crate::outbox::Event;

/// The UTC date `event` belongs to — its `as_of` partition.
///
/// Read off the front of `occurred_at` rather than parsed into a datetime and
/// formatted back out: the column is written by `strftime(...,'now')` in
/// SQLite, which is UTC by definition, so there is no zone to convert and
/// nothing a round trip could add but a way to be wrong.
///
/// A free function rather than a method because partitioning is the
/// shipper's business — the outbox has no notion of an `as_of`.
pub fn as_of(event: &Event) -> Result<&str> {
    let date = event.occurred_at.get(..10).unwrap_or_default();
    let shaped = date.len() == 10
        && date.as_bytes()[4] == b'-'
        && date.as_bytes()[7] == b'-'
        && date
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    // A bare date with nothing after it is not an instant; the column is
    // declared as one, and accepting less would let a hand-written row
    // address a partition.
    if !shaped || event.occurred_at.len() < 11 {
        return Err(ShipError::Timestamp {
            seq: event.seq,
            value: event.occurred_at.clone(),
        });
    }
    Ok(date)
}

/// A contiguous run of events destined for one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The partition these events land in.
    pub as_of: String,
    /// The events, in sequence order. Never empty.
    pub events: Vec<Event>,
}

impl Part {
    /// The first sequence number in this part.
    pub fn from_seq(&self) -> i64 {
        self.events[0].seq
    }

    /// The last sequence number in this part.
    pub fn to_seq(&self) -> i64 {
        self.events[self.events.len() - 1].seq
    }
}

/// A stretch of sequence numbers that is missing, inclusive at both ends.
///
/// Not "the numbers we skipped": the numbers that were **issued to events and
/// are not here**. See the module docs for why those are the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// The first missing number.
    pub from_seq: i64,
    /// The last missing number.
    pub to_seq: i64,
}

impl Gap {
    /// How many events were lost. Named `events` rather than `len` because
    /// that is what the number IS — a gap is not a collection with a length,
    /// it is a count of things that are not here.
    pub fn events(&self) -> i64 {
        self.to_seq - self.from_seq + 1
    }
}

impl std::fmt::Display for Gap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.from_seq == self.to_seq {
            write!(f, "seq {}", self.from_seq)
        } else {
            write!(f, "seq {}..{}", self.from_seq, self.to_seq)
        }
    }
}

/// What one block of outbox rows turns into.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The objects to write, in order.
    pub parts: Vec<Part>,
    /// Everything the sequence says should have been here and was not.
    pub gaps: Vec<Gap>,
}

/// Split `events` into parts and report the holes.
///
/// `events` must be in ascending `seq` order — which is how they are read —
/// and `cursor` is the highest sequence number already known to be in the
/// zone, so the first number expected is `cursor + 1`.
pub fn plan(events: Vec<Event>, cursor: i64, max_rows: usize) -> Result<Plan> {
    assert!(max_rows > 0, "a part must be able to hold a row");
    let mut out = Plan::default();
    let mut expected = cursor + 1;

    for event in events {
        let as_of = as_of(&event)?.to_string();

        // A hole between the last number accounted for and this one.
        if event.seq > expected {
            out.gaps.push(Gap {
                from_seq: expected,
                to_seq: event.seq - 1,
            });
        }

        let starts_a_part = match out.parts.last() {
            None => true,
            Some(part) => {
                part.as_of != as_of
                    || part.to_seq() + 1 != event.seq
                    || part.events.len() >= max_rows
            }
        };
        if starts_a_part {
            out.parts.push(Part {
                as_of,
                events: Vec::new(),
            });
        }
        expected = event.seq + 1;
        out.parts
            .last_mut()
            .expect("just pushed if empty")
            .events
            .push(event);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(seq: i64, occurred_at: &str) -> Event {
        Event {
            seq,
            occurred_at: occurred_at.to_string(),
            source_table: "collection".into(),
            op: "insert".into(),
            row_id: seq * 10,
            payload: format!(r#"{{"id":{}}}"#, seq * 10),
            source: "trigger".into(),
        }
    }

    fn day(seq: i64, date: &str) -> Event {
        event(seq, &format!("{date}T12:00:00.000Z"))
    }

    fn ranges(plan: &Plan) -> Vec<(String, i64, i64)> {
        plan.parts
            .iter()
            .map(|p| (p.as_of.clone(), p.from_seq(), p.to_seq()))
            .collect()
    }

    #[test]
    fn a_contiguous_run_of_one_day_is_one_part() {
        let plan = plan(
            (1..=5).map(|s| day(s, "2026-08-14")).collect(),
            0,
            usize::MAX,
        )
        .unwrap();
        assert_eq!(ranges(&plan), [("2026-08-14".to_string(), 1, 5)]);
        assert!(plan.gaps.is_empty());
    }

    #[test]
    fn a_part_never_spans_two_dates() {
        // as_of is a partition value; a part that spanned two would have to
        // pick one of them and be wrong about half its rows.
        let events = vec![
            day(1, "2026-08-13"),
            day(2, "2026-08-13"),
            day(3, "2026-08-14"),
        ];
        let plan = plan(events, 0, usize::MAX).unwrap();
        assert_eq!(
            ranges(&plan),
            [
                ("2026-08-13".to_string(), 1, 2),
                ("2026-08-14".to_string(), 3, 3)
            ]
        );
    }

    #[test]
    fn max_rows_splits_a_long_run() {
        let plan = plan((1..=7).map(|s| day(s, "2026-08-14")).collect(), 0, 3).unwrap();
        assert_eq!(
            ranges(&plan),
            [
                ("2026-08-14".to_string(), 1, 3),
                ("2026-08-14".to_string(), 4, 6),
                ("2026-08-14".to_string(), 7, 7)
            ]
        );
    }

    /// The invariant a reader can check without ever seeing the outbox: a
    /// part's name describes its contents exactly.
    #[test]
    fn every_part_holds_exactly_the_range_it_is_named_for() {
        let events = vec![
            day(1, "2026-08-13"),
            day(2, "2026-08-13"),
            day(6, "2026-08-13"),
            day(7, "2026-08-14"),
            day(8, "2026-08-14"),
        ];
        let plan = plan(events, 0, 2).unwrap();
        for part in &plan.parts {
            assert_eq!(
                part.to_seq() - part.from_seq() + 1,
                part.events.len() as i64,
                "part {}..{} holds {} rows",
                part.from_seq(),
                part.to_seq(),
                part.events.len()
            );
        }
    }

    // ── gaps ────────────────────────────────────────────────────────────────

    #[test]
    fn a_hole_in_the_middle_is_a_gap_and_splits_the_part() {
        let events = vec![
            day(1, "2026-08-14"),
            day(2, "2026-08-14"),
            day(5, "2026-08-14"),
        ];
        let plan = plan(events, 0, usize::MAX).unwrap();
        assert_eq!(
            plan.gaps,
            [Gap {
                from_seq: 3,
                to_seq: 4
            }]
        );
        assert_eq!(plan.gaps[0].events(), 2);
        assert_eq!(
            ranges(&plan),
            [
                ("2026-08-14".to_string(), 1, 2),
                ("2026-08-14".to_string(), 5, 5)
            ]
        );
    }

    /// The head gap, which is the one an implementation forgets. Rows deleted
    /// after the cursor passed them are gone legitimately; rows deleted
    /// before it reached them are lost events, and the difference is exactly
    /// `cursor + 1`.
    #[test]
    fn a_hole_between_the_cursor_and_the_first_row_is_a_gap() {
        let plan = plan(vec![day(9, "2026-08-14")], 5, usize::MAX).unwrap();
        assert_eq!(
            plan.gaps,
            [Gap {
                from_seq: 6,
                to_seq: 8
            }]
        );
    }

    #[test]
    fn a_cursor_sitting_exactly_before_the_first_row_is_not_a_gap() {
        let plan = plan(vec![day(6, "2026-08-14")], 5, usize::MAX).unwrap();
        assert!(plan.gaps.is_empty(), "{:?}", plan.gaps);
    }

    #[test]
    fn several_holes_are_several_gaps() {
        let events = vec![
            day(2, "2026-08-14"),
            day(4, "2026-08-14"),
            day(9, "2026-08-14"),
        ];
        let plan = plan(events, 0, usize::MAX).unwrap();
        assert_eq!(
            plan.gaps,
            [
                Gap {
                    from_seq: 1,
                    to_seq: 1
                },
                Gap {
                    from_seq: 3,
                    to_seq: 3
                },
                Gap {
                    from_seq: 5,
                    to_seq: 8
                }
            ]
        );
    }

    #[test]
    fn an_empty_outbox_plans_nothing_and_reports_nothing() {
        let plan = plan(Vec::new(), 0, usize::MAX).unwrap();
        assert!(plan.parts.is_empty() && plan.gaps.is_empty());
    }

    // ── the date, and refusing to guess one ─────────────────────────────────

    #[test]
    fn a_row_whose_timestamp_is_not_an_instant_refuses() {
        for bad in [
            "",
            "2026-08-14",
            "14/08/2026",
            "yesterday",
            "2026-8-14T00:00:00Z",
        ] {
            let err = plan(vec![event(1, bad)], 0, usize::MAX).unwrap_err();
            assert!(
                matches!(err, ShipError::Timestamp { seq: 1, .. }),
                "{bad:?} produced {err}"
            );
        }
    }

    #[test]
    fn the_trigger_s_own_format_is_accepted() {
        // Exactly what strftime('%Y-%m-%dT%H:%M:%fZ','now') writes.
        let plan = plan(vec![event(1, "2026-08-14T09:41:07.123Z")], 0, usize::MAX).unwrap();
        assert_eq!(plan.parts[0].as_of, "2026-08-14");
    }

    /// Determinism, stated as the property the object keys rest on: the same
    /// rows and the same cursor always produce the same parts.
    #[test]
    fn planning_is_a_function_of_the_rows_and_the_cursor() {
        let events: Vec<Event> = (1..=40)
            .map(|s| day(s, if s <= 17 { "2026-08-13" } else { "2026-08-14" }))
            .collect();
        let first = plan(events.clone(), 0, 7).unwrap();
        for _ in 0..4 {
            assert_eq!(plan(events.clone(), 0, 7).unwrap(), first);
        }
        // …and resuming part-way produces the same tail, which is what makes
        // a restart write the objects a full run would have written.
        let tail = plan(events.into_iter().filter(|e| e.seq > 14).collect(), 14, 7).unwrap();
        assert_eq!(
            ranges(&tail),
            ranges(&first)
                .into_iter()
                .filter(|(_, from, _)| *from > 14)
                .collect::<Vec<_>>()
        );
    }
}
