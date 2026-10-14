//! Comparing two catalogs **row by row** — never byte by byte.
//!
//! This is the acceptance test's instrument. Item 3 of the epic asks whether a
//! catalog derived from `raw/` is the same catalog the online refresh built
//! from the responses that `raw/` holds, and the answer has to be about
//! *content*:
//!
//! > Row-identical is the bar, never byte-identical. SQLite files differ on
//! > page layout and vacuum state for identical content, so a file hash fails
//! > for reasons that mean nothing.
//!
//! So: same tables, same columns in the same order, same number of rows, and
//! every row equal — read in a deterministic order that does not depend on
//! either file's physical layout. `ORDER BY` every column is that order. It is
//! not the cheapest possible comparison and it is the only one that cannot be
//! fooled by two files holding one table's rows in different pages.
//!
//! ## Exclusions are named, never implied
//!
//! Every table left out has to be named on the command line and is echoed in
//! the report. A comparator that quietly skipped a table would let exactly the
//! regression it exists to catch through, and the one table that legitimately
//! differs — `raw_derivation`, which the online path does not write at all
//! because it fetched rather than replayed — is a decision a reader of the
//! gate should have to see.

use std::collections::BTreeSet;

use rusqlite::Connection;
use rusqlite::types::Value;

/// What one table's comparison found.
pub struct TableReport {
    /// The table.
    pub table: String,
    /// Rows on the left.
    pub left_rows: usize,
    /// Rows on the right.
    pub right_rows: usize,
    /// Differences, most useful first, capped by `max_diffs`.
    pub differences: Vec<String>,
}

impl TableReport {
    /// Whether this table matched.
    pub fn matched(&self) -> bool {
        self.differences.is_empty() && self.left_rows == self.right_rows
    }
}

/// The whole comparison.
pub struct Report {
    /// One entry per compared table, in name order.
    pub tables: Vec<TableReport>,
    /// Tables present in one catalog and not the other.
    pub only_in_left: Vec<String>,
    /// As above, the other way round.
    pub only_in_right: Vec<String>,
    /// Tables the caller asked to skip, echoed so a skip is never silent.
    pub excluded: Vec<String>,
}

impl Report {
    /// Whether the two catalogs are row-identical over everything compared.
    pub fn matched(&self) -> bool {
        self.only_in_left.is_empty()
            && self.only_in_right.is_empty()
            && self.tables.iter().all(TableReport::matched)
    }

    /// Print the report. Every table is listed, matching or not: a report that
    /// only showed failures would make "compared nothing" look like success.
    pub fn print(&self) {
        if !self.excluded.is_empty() {
            println!(
                "  excluded (named by the caller): {}",
                self.excluded.join(", ")
            );
        }
        for t in &self.tables {
            if t.matched() {
                println!("  ok   {:<28} {} row(s)", t.table, t.left_rows);
            } else {
                println!(
                    "  DIFF {:<28} {} row(s) vs {}",
                    t.table, t.left_rows, t.right_rows
                );
                for d in &t.differences {
                    println!("       {d}");
                }
            }
        }
        for t in &self.only_in_left {
            println!("  DIFF {t:<28} present on the left only");
        }
        for t in &self.only_in_right {
            println!("  DIFF {t:<28} present on the right only");
        }
    }
}

/// Compare two catalogs, skipping `exclude`.
///
/// `max_diffs` caps how many differing rows are reported per table — a table
/// that differs in a million rows is one finding, and printing it a million
/// times helps nobody. The count of rows is always exact.
pub fn compare(
    left: &Connection,
    right: &Connection,
    exclude: &[String],
    max_diffs: usize,
) -> anyhow::Result<Report> {
    let skip: BTreeSet<&str> = exclude.iter().map(String::as_str).collect();
    let left_tables = tables(left)?;
    let right_tables = tables(right)?;

    let shared: Vec<String> = left_tables
        .intersection(&right_tables)
        .filter(|t| !skip.contains(t.as_str()))
        .cloned()
        .collect();

    let mut tables_report = Vec::new();
    for table in shared {
        tables_report.push(compare_table(left, right, &table, max_diffs)?);
    }

    Ok(Report {
        tables: tables_report,
        only_in_left: left_tables
            .difference(&right_tables)
            .filter(|t| !skip.contains(t.as_str()))
            .cloned()
            .collect(),
        only_in_right: right_tables
            .difference(&left_tables)
            .filter(|t| !skip.contains(t.as_str()))
            .cloned()
            .collect(),
        excluded: exclude.to_vec(),
    })
}

/// Every ordinary table in a catalog, sorted.
///
/// Views are excluded because they hold no rows of their own — comparing one
/// would be comparing its inputs a second time. `sqlite_*` is SQLite's own
/// bookkeeping, and `sqlite_sequence` in particular records AUTOINCREMENT
/// high-water marks, which are a property of insertion history rather than of
/// content.
fn tables(conn: &Connection) -> anyhow::Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master \
          WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Column names, in declaration order.
fn columns(conn: &Connection, table: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn compare_table(
    left: &Connection,
    right: &Connection,
    table: &str,
    max_diffs: usize,
) -> anyhow::Result<TableReport> {
    let left_cols = columns(left, table)?;
    let right_cols = columns(right, table)?;
    if left_cols != right_cols {
        return Ok(TableReport {
            table: table.to_string(),
            left_rows: 0,
            right_rows: 0,
            differences: vec![format!(
                "columns differ: [{}] vs [{}]",
                left_cols.join(", "),
                right_cols.join(", ")
            )],
        });
    }

    // ORDER BY every column, so the read order is a property of the CONTENT
    // and not of how either file happens to store it. Quoted because a column
    // may be a keyword, positional because a column name may be ambiguous.
    let order = (1..=left_cols.len())
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT * FROM \"{table}\" ORDER BY {order}");

    let mut l = left.prepare(&sql)?;
    let mut r = right.prepare(&sql)?;
    let mut l_rows = l.query([])?;
    let mut r_rows = r.query([])?;

    let mut differences = Vec::new();
    let (mut l_count, mut r_count) = (0usize, 0usize);
    loop {
        let a = l_rows.next()?;
        let a = a.map(|row| read_row(row, left_cols.len())).transpose()?;
        let b = r_rows.next()?;
        let b = b.map(|row| read_row(row, left_cols.len())).transpose()?;

        match (a, b) {
            (None, None) => break,
            (Some(a), Some(b)) => {
                l_count += 1;
                r_count += 1;
                if a != b && differences.len() < max_diffs {
                    differences.push(format!(
                        "row {l_count}: {} != {}",
                        render(&a, &left_cols),
                        render(&b, &left_cols)
                    ));
                }
            }
            (Some(a), None) => {
                l_count += 1;
                if differences.len() < max_diffs {
                    differences.push(format!("left only: {}", render(&a, &left_cols)));
                }
            }
            (None, Some(b)) => {
                r_count += 1;
                if differences.len() < max_diffs {
                    differences.push(format!("right only: {}", render(&b, &left_cols)));
                }
            }
        }
    }

    Ok(TableReport {
        table: table.to_string(),
        left_rows: l_count,
        right_rows: r_count,
        differences,
    })
}

fn read_row(row: &rusqlite::Row<'_>, n: usize) -> rusqlite::Result<Vec<Value>> {
    (0..n).map(|i| row.get::<_, Value>(i)).collect()
}

/// A row as `col=value`, short enough to read in a failure message.
fn render(row: &[Value], cols: &[String]) -> String {
    let body = row
        .iter()
        .zip(cols)
        .map(|(v, c)| {
            let v = match v {
                Value::Null => "NULL".to_string(),
                Value::Integer(i) => i.to_string(),
                Value::Real(f) => f.to_string(),
                Value::Text(t) => format!("{t:?}"),
                Value::Blob(b) => format!("<{} bytes>", b.len()),
            };
            format!("{c}={v}")
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("({body})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(rows: &[(i64, &str, Option<f64>)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER, name TEXT, price REAL)")
            .unwrap();
        for (id, name, price) in rows {
            conn.execute(
                "INSERT INTO t VALUES (?1, ?2, ?3)",
                rusqlite::params![id, name, price],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn identical_content_matches() {
        let a = db(&[(1, "a", Some(1.5)), (2, "b", None)]);
        let b = db(&[(1, "a", Some(1.5)), (2, "b", None)]);
        let report = compare(&a, &b, &[], 10).unwrap();
        assert!(report.matched());
        assert_eq!(report.tables[0].left_rows, 2);
    }

    /// The property that makes this a ROW comparison: the same rows inserted
    /// in a different order are the same content. A file hash would call
    /// these different, which is exactly the false alarm the epic rules out.
    #[test]
    fn insertion_order_does_not_matter() {
        let a = db(&[(1, "a", Some(1.5)), (2, "b", None)]);
        let b = db(&[(2, "b", None), (1, "a", Some(1.5))]);
        assert!(compare(&a, &b, &[], 10).unwrap().matched());
    }

    /// …and it is still a comparison. One changed value in one row of one
    /// table fails, and the message says which row and what changed.
    #[test]
    fn a_single_changed_value_is_caught_and_named() {
        let a = db(&[(1, "a", Some(1.5))]);
        let b = db(&[(1, "a", Some(1.6))]);
        let report = compare(&a, &b, &[], 10).unwrap();
        assert!(!report.matched());
        let d = &report.tables[0].differences[0];
        assert!(d.contains("price=1.5"), "{d}");
        assert!(d.contains("price=1.6"), "{d}");
    }

    /// NULL is a value, not an absence: a column that went NULL is a real
    /// difference and must not compare equal to anything.
    #[test]
    fn null_is_not_equal_to_a_value() {
        let a = db(&[(1, "a", None)]);
        let b = db(&[(1, "a", Some(0.0))]);
        assert!(!compare(&a, &b, &[], 10).unwrap().matched());
    }

    #[test]
    fn a_missing_row_is_caught_on_either_side() {
        let a = db(&[(1, "a", None), (2, "b", None)]);
        let b = db(&[(1, "a", None)]);
        let report = compare(&a, &b, &[], 10).unwrap();
        assert!(!report.matched());
        assert_eq!(report.tables[0].left_rows, 2);
        assert_eq!(report.tables[0].right_rows, 1);

        let flipped = compare(&b, &a, &[], 10).unwrap();
        assert!(!flipped.matched());
    }

    /// An excluded table is not compared AND is named in the report. A skip
    /// nobody can see is how a comparator starts proving nothing.
    #[test]
    fn an_exclusion_is_honoured_and_echoed() {
        let a = db(&[(1, "a", None)]);
        let b = db(&[(9, "z", None)]);
        let report = compare(&a, &b, &["t".to_string()], 10).unwrap();
        assert!(report.matched());
        assert!(report.tables.is_empty());
        assert_eq!(report.excluded, vec!["t"]);
    }

    /// A table on one side only is a difference in its own right — the shape
    /// of the catalog is part of what is being compared.
    #[test]
    fn a_table_present_on_one_side_only_fails() {
        let a = db(&[(1, "a", None)]);
        let b = db(&[(1, "a", None)]);
        b.execute_batch("CREATE TABLE extra (x INTEGER)").unwrap();
        let report = compare(&a, &b, &[], 10).unwrap();
        assert!(!report.matched());
        assert_eq!(report.only_in_right, vec!["extra"]);
    }

    /// Same table, different shape. Comparing rows would be meaningless, so
    /// the columns are checked first and reported as the difference.
    #[test]
    fn a_changed_column_list_is_reported_as_such() {
        let a = db(&[(1, "a", None)]);
        let b = Connection::open_in_memory().unwrap();
        b.execute_batch("CREATE TABLE t (id INTEGER, name TEXT)")
            .unwrap();
        let report = compare(&a, &b, &[], 10).unwrap();
        assert!(!report.matched());
        assert!(report.tables[0].differences[0].contains("columns differ"));
    }

    /// The cap is on how much is PRINTED, never on how much is compared: the
    /// row counts stay exact so a truncated report cannot read as a small
    /// difference.
    #[test]
    fn the_diff_cap_truncates_the_report_not_the_comparison() {
        let a = db(&[(1, "a", None), (2, "b", None), (3, "c", None)]);
        let b = db(&[(1, "x", None), (2, "y", None), (3, "z", None)]);
        let report = compare(&a, &b, &[], 1).unwrap();
        assert_eq!(report.tables[0].differences.len(), 1);
        assert_eq!(report.tables[0].left_rows, 3);
        assert!(!report.matched());
    }
}
