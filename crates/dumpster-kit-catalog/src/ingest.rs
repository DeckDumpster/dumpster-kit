use std::collections::BTreeMap;

use dumpster_kit_tcgcsv::{Price, Product};
use rusqlite::Connection;

use crate::alarm::{UNMATCHED_PRODUCT, raise};
use crate::error::Result;
use crate::game::{Game, Match};
use crate::prices::{PriceRow, record_prices};

/// The `source` name tcgcsv rows carry in `prices` and `printing_source_id`.
pub const TCGCSV: &str = "tcgcsv";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub prices_inserted: u64,
    pub crosswalked: u64,
    pub sealed_upserted: u64,
    /// `product_id/sub_type` of every pair the game could not match; each raised an alarm.
    pub unmatched: Vec<String>,
}

/// Run one day's tcgcsv products and prices through the game's crosswalk in one
/// transaction: matched prices are appended, the crosswalk store and `sealed_products` are
/// updated, and each unmatched pair raises `unmatched_tcgcsv_product`.
pub fn ingest_tcgcsv<G: Game>(
    conn: &mut Connection,
    observed_on: &str,
    products: &[Product],
    prices: &[Price],
) -> Result<IngestReport> {
    let by_id: BTreeMap<i64, &Product> = products.iter().map(|p| (p.product_id, p)).collect();
    let tx = conn.transaction()?;
    let mut report = IngestReport::default();
    let mut rows = Vec::new();
    for price in prices {
        let sub_type = price.sub_type_name.as_deref().unwrap_or("");
        let subject = format!("{}/{sub_type}", price.product_id);
        let matched = match by_id.get(&price.product_id) {
            Some(product) => G::crosswalk(product, price),
            None => Match::Unmatched,
        };
        match matched {
            Match::Printing {
                printing_id,
                finish,
            } => {
                report.crosswalked += tx.execute(
                    "INSERT OR REPLACE INTO printing_source_id
                     (source, source_id, sub_type, printing_id) VALUES (?1, ?2, ?3, ?4)",
                    (TCGCSV, price.product_id.to_string(), sub_type, printing_id),
                )? as u64;
                rows.push(PriceRow {
                    printing_id,
                    finish,
                    source: TCGCSV.to_string(),
                    observed_on: observed_on.to_string(),
                    low: price.low,
                    mid: price.mid,
                    high: price.high,
                    market: price.market,
                    direct_low: price.direct_low,
                });
            }
            Match::Sealed => {
                let p = by_id[&price.product_id];
                report.sealed_upserted += tx.execute(
                    "INSERT INTO sealed_products (source, source_id, group_id, name)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (source, source_id) DO UPDATE SET
                         group_id = excluded.group_id, name = excluded.name
                     WHERE (group_id, name) IS NOT (excluded.group_id, excluded.name)",
                    (TCGCSV, p.product_id.to_string(), p.group_id, &p.name),
                )? as u64;
            }
            Match::Unmatched => {
                raise(&tx, UNMATCHED_PRODUCT, G::ID, &subject, "")?;
                report.unmatched.push(subject);
            }
        }
    }
    report.prices_inserted = record_prices(&tx, G::ID, G::FINISH, &rows)?;
    tx.commit()?;
    Ok(report)
}
