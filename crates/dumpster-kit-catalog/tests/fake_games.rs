//! The kit is tested only with invented games: `Mugs` keys prices by printing id (finish is
//! part of the printing), `Coins` keeps one printing per coin and prices per finish.

use dumpster_kit_catalog::*;
use dumpster_kit_tcgcsv::{Category, Price, Product};
use rusqlite::{Connection, Transaction};

struct Mugs;
struct Coins;

fn rule(p: &Product, price: &Price, on_copy: bool) -> Match {
    if !p.is_single_card() {
        return Match::Sealed;
    }
    match p.name.strip_prefix("known-") {
        Some(_) => Match::Printing {
            printing_id: p.product_id * 10,
            finish: on_copy.then(|| price.sub_type_name.clone().unwrap_or_default()),
        },
        None => Match::Unmatched,
    }
}

macro_rules! fake {
    ($t:ty, $id:literal, $finish:expr, $on_copy:literal, $cat:literal) => {
        impl Game for $t {
            const ID: &'static str = $id;
            const SCHEMA: &'static str =
                "CREATE TABLE IF NOT EXISTS printing (printing_id INTEGER PRIMARY KEY);";
            const FINISH: FinishPlacement = $finish;
            type Payload = ();
            fn sources() -> Vec<&'static str> {
                vec!["tcgcsv"]
            }
            fn parse(_: &str, _: &[u8]) -> Result<()> {
                Ok(())
            }
            fn apply(_: &Transaction<'_>, _: &()) -> Result<ApplyReport> {
                Ok(ApplyReport::default())
            }
            fn crosswalk(p: &Product, price: &Price) -> Match {
                rule(p, price, $on_copy)
            }
            fn search_terms() -> Vec<SearchTerm> {
                vec![SearchTerm {
                    key: "name",
                    column: "name",
                }]
            }
            fn tcgcsv_category() -> i64 {
                $cat
            }
        }
    };
}
fake!(Mugs, "mugs", FinishPlacement::OnPrinting, false, 901);
fake!(Coins, "coins", FinishPlacement::OnCopy, true, 902);

fn product(id: i64, name: &str, number: Option<&str>) -> Product {
    Product {
        product_id: id,
        group_id: 1,
        name: name.into(),
        number: number.map(Into::into),
        extended_data: vec![],
        image_url: None,
        image_count: 0,
        url: None,
    }
}

fn price(id: i64, sub: &str, market: f64) -> Price {
    Price {
        product_id: id,
        sub_type_name: Some(sub.into()),
        low: None,
        mid: None,
        high: None,
        market: Some(market),
        direct_low: None,
    }
}

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    install(&c).unwrap();
    c
}

fn day() -> (Vec<Product>, Vec<Price>) {
    (
        vec![
            product(1, "known-a", Some("001")),
            product(2, "mystery", Some("002")),
            product(3, "booster", None),
        ],
        vec![
            price(1, "Normal", 1.5),
            price(1, "Foil", 4.0),
            price(2, "Normal", 9.0),
            price(3, "Normal", 20.0),
            price(99, "Normal", 1.0),
        ],
    )
}

#[test]
fn printing_id_game_prices_by_printing() {
    let mut c = db();
    let (p, pr) = day();
    let r = ingest_tcgcsv::<Mugs>(&mut c, "2026-10-27", &p, &pr).unwrap();
    // Normal and Foil of product 1 both land on printing 10 with finish '' - one row wins.
    assert_eq!(r.prices_inserted, 1);
    let finishes: Vec<String> = c
        .prepare("SELECT DISTINCT finish FROM prices")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(finishes, vec![String::new()]);
    assert_eq!(r.sealed_upserted, 1);
}

#[test]
fn on_copy_game_keys_prices_by_finish() {
    let mut c = db();
    let (p, pr) = day();
    let r = ingest_tcgcsv::<Coins>(&mut c, "2026-10-27", &p, &pr).unwrap();
    assert_eq!(r.prices_inserted, 2);
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM latest_prices WHERE printing_id = 10 AND finish IN ('Normal','Foil')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn placement_is_enforced() {
    let mut c = db();
    let tx = c.transaction().unwrap();
    let row = |finish: Option<&str>| PriceRow {
        printing_id: 1,
        finish: finish.map(Into::into),
        source: "s".into(),
        observed_on: "2026-10-27".into(),
        low: None,
        mid: None,
        high: None,
        market: Some(1.0),
        direct_low: None,
    };
    assert!(matches!(
        record_prices(
            &tx,
            "mugs",
            FinishPlacement::OnPrinting,
            &[row(Some("Foil"))]
        ),
        Err(Error::FinishNotAllowed { .. })
    ));
    assert!(matches!(
        record_prices(&tx, "coins", FinishPlacement::OnCopy, &[row(None)]),
        Err(Error::FinishRequired { .. })
    ));
}

#[test]
fn unmatched_product_raises_the_alarm() {
    let mut c = db();
    let (p, pr) = day();
    let r = ingest_tcgcsv::<Coins>(&mut c, "2026-10-27", &p, &pr).unwrap();
    assert_eq!(r.unmatched, vec!["2/Normal", "99/Normal"]);
    let a = alarms(&c, UNMATCHED_PRODUCT).unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].game, "coins");
    // Replaying the day neither duplicates alarms nor prices.
    let r = ingest_tcgcsv::<Coins>(&mut c, "2026-10-27", &p, &pr).unwrap();
    assert_eq!(r.prices_inserted, 0);
    assert_eq!(alarms(&c, UNMATCHED_PRODUCT).unwrap().len(), 2);
}

#[test]
fn crosswalk_store_maps_source_ids() {
    let mut c = db();
    let (p, pr) = day();
    ingest_tcgcsv::<Coins>(&mut c, "2026-10-27", &p, &pr).unwrap();
    let id: i64 = c
        .query_row(
            "SELECT printing_id FROM printing_source_id WHERE source='tcgcsv' AND source_id='1' AND sub_type='Foil'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(id, 10);
}

#[test]
fn append_only_triggers_refuse_update_and_delete() {
    let mut c = db();
    let (p, pr) = day();
    ingest_tcgcsv::<Coins>(&mut c, "2026-10-27", &p, &pr).unwrap();
    let e = c.execute("UPDATE prices SET market = 0", []).unwrap_err();
    assert!(e.to_string().contains("append-only"), "{e}");
    let e = c.execute("DELETE FROM prices", []).unwrap_err();
    assert!(e.to_string().contains("append-only"), "{e}");
}

#[test]
fn latest_prices_takes_the_newest_day() {
    let mut c = db();
    let (p, mut pr) = day();
    ingest_tcgcsv::<Mugs>(&mut c, "2026-10-26", &p, &pr).unwrap();
    pr[0].market = Some(2.5);
    ingest_tcgcsv::<Mugs>(&mut c, "2026-10-27", &p, &pr).unwrap();
    let m: f64 = c
        .query_row(
            "SELECT market FROM latest_prices WHERE printing_id = 10",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(m, 2.5);
}

#[test]
fn missing_day_alarm() {
    let mut c = db();
    let (p, pr) = day();
    ingest_tcgcsv::<Mugs>(&mut c, "2026-10-24", &p, &pr).unwrap();
    ingest_tcgcsv::<Mugs>(&mut c, "2026-10-27", &p, &pr).unwrap();
    let missing = check_missing_days(&c, "mugs", "tcgcsv", "2026-10-28").unwrap();
    assert_eq!(missing, vec!["2026-10-25", "2026-10-26", "2026-10-28"]);
    assert_eq!(alarms(&c, MISSING_PRICE_DAY).unwrap().len(), 3);
}

#[test]
fn category_mirror_upserts() {
    let mut c = db();
    let cat = |name: &str| Category {
        category_id: 79,
        name: name.into(),
        display_name: None,
        modified_on: None,
        sealed_label: None,
        popularity: 1,
    };
    let tx = c.transaction().unwrap();
    assert_eq!(mirror_categories(&tx, &[cat("a")]).unwrap(), 1);
    assert_eq!(mirror_categories(&tx, &[cat("a")]).unwrap(), 0);
    assert_eq!(mirror_categories(&tx, &[cat("b")]).unwrap(), 1);
}
