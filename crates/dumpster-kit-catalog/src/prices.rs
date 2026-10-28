use rusqlite::{Connection, Transaction};

use crate::alarm::{MISSING_PRICE_DAY, raise};
use crate::error::{Error, Result};
use crate::game::FinishPlacement;

#[derive(Debug, Clone, PartialEq)]
pub struct PriceRow {
    pub printing_id: i64,
    /// `Some` exactly when the game's placement is `OnCopy`.
    pub finish: Option<String>,
    pub source: String,
    pub observed_on: String,
    pub low: Option<f64>,
    pub mid: Option<f64>,
    pub high: Option<f64>,
    pub market: Option<f64>,
    pub direct_low: Option<f64>,
}

/// Append observations. A row already present for (printing, finish, source, day) is left
/// alone, so replaying a day is idempotent. Returns the rows inserted.
pub fn record_prices(
    tx: &Transaction<'_>,
    game: &'static str,
    placement: FinishPlacement,
    rows: &[PriceRow],
) -> Result<u64> {
    let mut stmt = tx.prepare_cached(
        "INSERT OR IGNORE INTO prices
         (printing_id, finish, source, observed_on, low, mid, high, market, direct_low)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut inserted = 0;
    for r in rows {
        let finish = match (placement, r.finish.as_deref()) {
            (FinishPlacement::OnPrinting, None) => "",
            (FinishPlacement::OnPrinting, Some(f)) => {
                return Err(Error::FinishNotAllowed {
                    game,
                    finish: f.to_string(),
                });
            }
            (FinishPlacement::OnCopy, Some(f)) if !f.is_empty() => f,
            (FinishPlacement::OnCopy, _) => {
                return Err(Error::FinishRequired {
                    game,
                    printing_id: r.printing_id,
                });
            }
        };
        inserted += stmt.execute((
            r.printing_id,
            finish,
            &r.source,
            &r.observed_on,
            r.low,
            r.mid,
            r.high,
            r.market,
            r.direct_low,
        ))? as u64;
    }
    Ok(inserted)
}

/// Raise `missing_price_day` for every day from the source's first observation through
/// `through` (inclusive, `YYYY-MM-DD`) that has no rows. Returns the missing days.
pub fn check_missing_days(
    conn: &Connection,
    game: &str,
    source: &str,
    through: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE days(d) AS (
             SELECT MIN(observed_on) FROM prices WHERE source = ?1
             UNION ALL SELECT date(d, '+1 day') FROM days WHERE d < ?2)
         SELECT d FROM days WHERE d IS NOT NULL AND d <= ?2
           AND NOT EXISTS (SELECT 1 FROM prices WHERE source = ?1 AND observed_on = d)
         ORDER BY d",
    )?;
    let missing: Vec<String> = stmt
        .query_map((source, through), |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for d in &missing {
        raise(conn, MISSING_PRICE_DAY, game, &format!("{source}/{d}"), "")?;
    }
    Ok(missing)
}
