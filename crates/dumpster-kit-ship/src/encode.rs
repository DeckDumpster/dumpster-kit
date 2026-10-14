//! One part, as Parquet.
//!
//! The tenant zone is plain partitioned Parquet (`pd-uz8q`) — plain because
//! Iceberg records absolute paths and would make a later bucket split a
//! metadata rewrite, partitioned because a deletion has to be a prefix drop.
//! This module is the only place a batch of outbox rows becomes those bytes.
//!
//! ## The schema is the outbox's, not the collection's
//!
//! ```text
//! message holdings_event {
//!   required int64  seq;
//!   required binary occurred_at (UTF8);
//!   required binary source_table (UTF8);
//!   required binary op (UTF8);
//!   required int64  row_id;
//!   required binary payload (UTF8);   // the whole row, as the trigger wrote it
//!   required binary source (UTF8);    // trigger | backfill | redrive
//! }
//! ```
//!
//! It is [`crate::outbox::Event`]'s seven fields, all of them, because it
//! IS that struct (pd-mixm) — so a column added to `ownership_outbox` fails
//! this module to compile rather than quietly stopping at the bucket.
//!
//! ## Carrying `source` is not branching on it
//!
//! Rule 2 of the outbox's design forbids a *consumer* that treats a
//! backfilled event differently from a triggered one — the moment it does,
//! backfill stops being the same path. It does not forbid recording which it
//! was, and dropping the column would cost two things: an operator asking
//! "did last night's redrive actually reach the zone" would have nothing in
//! the zone to answer with, and `decode` could not rebuild an [`Event`] at
//! all, so a reader could not hand a part to [`crate::outbox::project`]
//! — the one implementation of the resolution rule — without inventing the
//! field. This crate reads it nowhere.
//!
//! `payload` stays the JSON object the trigger built rather than being
//! flattened into typed columns, and that is a decision. The outbox's payload
//! is already pinned to `collection`'s columns by a test that compares the two
//! (`pd-5m54`), so flattening here would add a *third* place the collection's
//! shape is written down — one that no test could hold to the other two
//! without reimplementing the comparison. Passing the object through means a
//! column added to `collection` reaches the zone the night it lands, with no
//! shipper change and nothing to forget. A reader that wants a column asks
//! for it with `json_extract`, which is what DuckDB is for.
//!
//! `database_id` is deliberately **not** a column: the partition carries it,
//! and [`crate::cipher`] binds each object to its own key, so a second copy
//! inside the file could only ever disagree with those.
//!
//! ## `row_id` alone identifies nothing — the key is (`source_table`, `row_id`)
//!
//! `row_id` is unique only WITHIN a source table (pd-4gop). `collection` and
//! `sealed_collection` number their rows independently, so the first single
//! and the first sealed lot are BOTH `row_id = 1` — which is the ordinary
//! shape of a collection, not a corner case. A reader that grouped this
//! dataset by `row_id` would silently merge two unrelated streams of events
//! into one projection and produce a number that looks entirely plausible.
//!
//! So `source_table` is carried in every part, beside `row_id`, and it is
//! there for that reason rather than for provenance. **The shipper itself
//! keys nothing on either**: its only key is `seq`, which is unique across the
//! whole outbox. The pair is here for whatever reads these parts.
//!
//! (`payload` also carries the row's own `id`, but only the pair says which
//! table that id is in — so it is no substitute either.)
//!
//! ## Determinism
//!
//! `created_by` is pinned rather than left to the writer, so two builds of
//! this crate produce identical footers. Byte-identity is a property *within*
//! a build of the `parquet` crate: an upgrade that changes its encoding would
//! make a re-ship of an already-shipped part write different bytes to the same
//! key. That is still the same rows at the same address — idempotent in the
//! sense the design asks for — but it is why the gate that compares bytes runs
//! against one build rather than across two.

use std::sync::Arc;

use parquet::basic::Compression;
use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::file::writer::SerializedFileWriter;
use parquet::record::Field;
use parquet::schema::parser::parse_message_type;

use crate::error::{Result, ShipError};
use crate::plan::Event;

/// The message type every holdings part is written with.
pub const SCHEMA: &str = "
message holdings_event {
  required int64 seq;
  required binary occurred_at (UTF8);
  required binary source_table (UTF8);
  required binary op (UTF8);
  required int64 row_id;
  required binary payload (UTF8);
  required binary source (UTF8);
}
";

/// What the footer records as the writer. Pinned so the bytes do not move
/// when a dependency's version string does.
const CREATED_BY: &str = "dumpster-kit-ship";

/// Encode `events` as a single-row-group Parquet file.
pub fn encode(events: &[Event]) -> Result<Vec<u8>> {
    let schema = Arc::new(parse_message_type(SCHEMA)?);
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_created_by(CREATED_BY.to_string())
            .build(),
    );

    let mut buf: Vec<u8> = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut buf, schema, props)?;
    let mut group = writer.next_row_group()?;

    let seq: Vec<i64> = events.iter().map(|e| e.seq).collect();
    let row_id: Vec<i64> = events.iter().map(|e| e.row_id).collect();
    let text = |f: fn(&Event) -> &str| -> Vec<ByteArray> {
        events.iter().map(|e| ByteArray::from(f(e))).collect()
    };

    let mut column = 0usize;
    while let Some(mut writer) = group.next_column()? {
        match column {
            0 => writer.typed::<Int64Type>().write_batch(&seq, None, None)?,
            1 => writer.typed::<ByteArrayType>().write_batch(
                &text(|e| &e.occurred_at),
                None,
                None,
            )?,
            2 => writer.typed::<ByteArrayType>().write_batch(
                &text(|e| &e.source_table),
                None,
                None,
            )?,
            3 => writer
                .typed::<ByteArrayType>()
                .write_batch(&text(|e| &e.op), None, None)?,
            4 => writer
                .typed::<Int64Type>()
                .write_batch(&row_id, None, None)?,
            5 => writer
                .typed::<ByteArrayType>()
                .write_batch(&text(|e| &e.payload), None, None)?,
            6 => writer
                .typed::<ByteArrayType>()
                .write_batch(&text(|e| &e.source), None, None)?,
            // Unreachable while SCHEMA has seven columns, and a compile-time
            // check is not available for a string the crate parses — so it is
            // an error rather than a panic, and it names the mismatch.
            n => {
                return Err(ShipError::Zone(format!(
                    "the parquet schema has more columns ({}) than this writer fills ({n})",
                    n + 1
                )));
            }
        };
        writer.close()?;
        column += 1;
    }
    group.close()?;
    writer.close()?;
    Ok(buf)
}

/// Read a part back. The inverse of [`encode`], for the decrypt path and for
/// the tests that check a shipped object actually holds what was shipped.
pub fn decode(bytes: Vec<u8>) -> Result<Vec<Event>> {
    let reader = SerializedFileReader::new(bytes::Bytes::from(bytes))?;
    let mut out = Vec::new();
    for row in reader.get_row_iter(None)? {
        let row = row?;
        let mut event = Event {
            seq: 0,
            occurred_at: String::new(),
            source_table: String::new(),
            op: String::new(),
            row_id: 0,
            payload: String::new(),
            source: String::new(),
        };
        for (name, field) in row.get_column_iter() {
            match (name.as_str(), field) {
                ("seq", Field::Long(v)) => event.seq = *v,
                ("row_id", Field::Long(v)) => event.row_id = *v,
                ("occurred_at", Field::Str(v)) => event.occurred_at = v.clone(),
                ("source_table", Field::Str(v)) => event.source_table = v.clone(),
                ("op", Field::Str(v)) => event.op = v.clone(),
                ("payload", Field::Str(v)) => event.payload = v.clone(),
                ("source", Field::Str(v)) => event.source = v.clone(),
                (other, _) => {
                    return Err(ShipError::Zone(format!(
                        "a holdings part carries an unexpected column {other:?}"
                    )));
                }
            }
        }
        out.push(event);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(n: i64) -> Vec<Event> {
        (1..=n)
            .map(|seq| Event {
                seq,
                occurred_at: format!("2026-08-14T09:00:0{}.000Z", seq % 10),
                source_table: "collection".into(),
                op: if seq % 3 == 0 { "delete" } else { "insert" }.into(),
                row_id: seq * 7,
                payload: format!(
                    r#"{{"id":{},"printing_id":"sv3pt5-{seq}-normal"}}"#,
                    seq * 7
                ),
                source: if seq % 5 == 0 { "backfill" } else { "trigger" }.into(),
            })
            .collect()
    }

    #[test]
    fn a_part_round_trips() {
        let events = events(120);
        assert_eq!(decode(encode(&events).unwrap()).unwrap(), events);
    }

    #[test]
    fn what_is_written_is_parquet() {
        let bytes = encode(&events(3)).unwrap();
        assert_eq!(&bytes[..4], b"PAR1", "a parquet file starts with PAR1");
        assert_eq!(&bytes[bytes.len() - 4..], b"PAR1", "…and ends with it");
    }

    /// The other half of what makes a re-ship byte-identical: the encoding is
    /// a function of the rows and nothing else.
    #[test]
    fn encoding_is_deterministic() {
        let events = events(50);
        let once = encode(&events).unwrap();
        for _ in 0..4 {
            assert_eq!(encode(&events).unwrap(), once);
        }
    }

    #[test]
    fn an_empty_part_is_still_a_readable_file() {
        // Nothing ships one — a part is built from at least one row — but a
        // writer that panicked on the empty case would be a landmine for the
        // backfill, which will call this with whatever a scope produces.
        assert_eq!(decode(encode(&[]).unwrap()).unwrap(), Vec::<Event>::new());
    }

    /// Two events that differ ONLY by source table stay distinguishable in the
    /// zone. `row_id` is unique within a table, not across the outbox
    /// (pd-4gop), so a part that dropped `source_table` — or a reader that
    /// grouped on `row_id` alone — would merge a single and a sealed lot into
    /// one row and produce a plausible wrong answer.
    ///
    /// Seen red: writing a constant into the `source_table` column instead of
    /// the event's own makes these two events identical here.
    #[test]
    fn the_same_row_id_in_two_tables_is_two_different_things() {
        let of = |seq: i64, table: &str| Event {
            seq,
            occurred_at: "2026-08-14T00:00:00.000Z".into(),
            source_table: table.to_string(),
            op: "insert".into(),
            row_id: 1,
            payload: r#"{"id":1}"#.into(),
            source: "trigger".into(),
        };
        let events = vec![of(1, "collection"), of(2, "sealed_collection")];
        let back = decode(encode(&events).unwrap()).unwrap();

        assert_eq!(back, events);
        assert_eq!(
            back[0].row_id, back[1].row_id,
            "the fixture is the hard case"
        );
        assert_ne!(
            back[0].source_table, back[1].source_table,
            "…and the only thing that tells them apart survived the round trip"
        );
    }

    /// The payload reaches the zone as the trigger wrote it, character for
    /// character — no re-serialisation, no key reordering, no lost precision.
    #[test]
    fn the_payload_is_passed_through_untouched() {
        let awkward = r#"{"notes":"a \"quoted\" ünïcode note","purchase_price":12.30,"tags":null}"#;
        let event = Event {
            seq: 1,
            occurred_at: "2026-08-14T00:00:00.000Z".into(),
            source_table: "collection".into(),
            op: "update".into(),
            row_id: 4,
            payload: awkward.to_string(),
            source: "redrive".into(),
        };
        let back = decode(encode(std::slice::from_ref(&event)).unwrap()).unwrap();
        assert_eq!(back[0].payload, awkward);
    }
}
