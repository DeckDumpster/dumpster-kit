use rusqlite::Connection;

use crate::error::Result;

pub const UNMATCHED_PRODUCT: &str = "unmatched_tcgcsv_product";
pub const MISSING_PRICE_DAY: &str = "missing_price_day";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alarm {
    pub kind: String,
    pub game: String,
    pub subject: String,
    pub detail: String,
}

/// Raise an alarm; raising the same (kind, game, subject) again is a no-op. Returns whether
/// it was new.
pub fn raise(
    conn: &Connection,
    kind: &str,
    game: &str,
    subject: &str,
    detail: &str,
) -> Result<bool> {
    Ok(conn.execute(
        "INSERT OR IGNORE INTO catalog_alarms (kind, game, subject, detail) VALUES (?1, ?2, ?3, ?4)",
        (kind, game, subject, detail),
    )? > 0)
}

pub fn alarms(conn: &Connection, kind: &str) -> Result<Vec<Alarm>> {
    let mut stmt = conn.prepare(
        "SELECT kind, game, subject, detail FROM catalog_alarms WHERE kind = ?1 ORDER BY alarm_id",
    )?;
    let rows = stmt.query_map([kind], |r| {
        Ok(Alarm {
            kind: r.get(0)?,
            game: r.get(1)?,
            subject: r.get(2)?,
            detail: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
