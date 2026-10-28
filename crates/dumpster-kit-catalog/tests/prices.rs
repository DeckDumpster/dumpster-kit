//! Prices, the crosswalk store, the category mirror and their alarms, through the fake games.

use dumpster_kit_catalog::fake::{FakeOnCopy, FakeOnPrinting};
use dumpster_kit_catalog::*;
use dumpster_kit_tcgcsv::{Category, Price, Product};

fn open<G: Game>(dir: &tempfile::TempDir) -> Catalog<G> {
    Catalog::<G>::open(&dir.path().join("shared.sqlite"), &dir.path().join("raw")).unwrap()
}

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

// FakeOnPrinting matches products that carry a number (printing "fp-<number>") and leaves the
// rest unmatched; FakeOnCopy prices numbered products per finish and calls the rest sealed.
fn day() -> (Vec<Product>, Vec<Price>) {
    (
        vec![product(1, "a", Some("001")), product(2, "no number", None)],
        vec![
            price(1, "Normal", 1.5),
            price(2, "Normal", 9.0),
            price(99, "Normal", 1.0),
        ],
    )
}

fn count(c: &rusqlite::Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn fake_game_prices_by_printing_id() {
    let dir = tempfile::tempdir().unwrap();
    let cat = open::<FakeOnPrinting>(&dir);
    let (p, pr) = day();
    let r = ingest_tcgcsv::<FakeOnPrinting>(cat.conn(), "2026-10-27", "t", &p, &pr).unwrap();
    assert_eq!(r.prices_inserted, 1);
    assert_eq!(
        count(
            cat.conn(),
            "SELECT COUNT(*) FROM latest_prices WHERE printing_id = 'fp-001' AND finish = ''"
        ),
        1
    );
    let id: String = cat
        .conn()
        .query_row(
            "SELECT printing_id FROM printing_source_id WHERE source='tcgcsv' AND source_id='1' AND sub_type='Normal'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(id, "fp-001");
}

#[test]
fn on_copy_fake_game_keys_prices_by_finish() {
    let dir = tempfile::tempdir().unwrap();
    let cat = open::<FakeOnCopy>(&dir);
    let tx = cat.conn().unchecked_transaction().unwrap();
    let row = |finish: Option<&str>| PriceRow {
        printing_id: "fc-1".into(),
        finish: finish.map(Into::into),
        source: "s".into(),
        observed_on: "2026-10-27".into(),
        low: None,
        mid: None,
        high: None,
        market: Some(1.0),
        direct_low: None,
    };
    let rows = [row(Some("Normal")), row(Some("Foil"))];
    assert_eq!(
        record_prices(&tx, "fakecopy", FakeOnCopy::FINISH, &rows).unwrap(),
        2
    );
    assert!(matches!(
        record_prices(&tx, "fakecopy", FinishPlacement::OnCopy, &[row(None)]),
        Err(Error::FinishRequired { .. })
    ));
    assert!(matches!(
        record_prices(
            &tx,
            "fakeprint",
            FinishPlacement::OnPrinting,
            &[row(Some("Foil"))]
        ),
        Err(Error::FinishNotAllowed { .. })
    ));
    tx.commit().unwrap();
    assert_eq!(
        count(
            cat.conn(),
            "SELECT COUNT(*) FROM latest_prices WHERE printing_id = 'fc-1'"
        ),
        2
    );
}

#[test]
fn unmatched_tcgcsv_product_raises_the_alarm() {
    let dir = tempfile::tempdir().unwrap();
    let cat = open::<FakeOnPrinting>(&dir);
    let (p, pr) = day();
    let r = ingest_tcgcsv::<FakeOnPrinting>(cat.conn(), "2026-10-27", "t", &p, &pr).unwrap();
    assert_eq!(r.unmatched, vec!["2/Normal", "99/Normal"]);
    let open_alarms = cat.open_alarms().unwrap();
    assert_eq!(open_alarms.len(), 2);
    assert!(open_alarms.iter().all(|a| a.kind == UNMATCHED_PRODUCT));
    // Replaying the day neither duplicates alarms nor prices.
    let r = ingest_tcgcsv::<FakeOnPrinting>(cat.conn(), "2026-10-27", "t2", &p, &pr).unwrap();
    assert_eq!(r.prices_inserted, 0);
    assert_eq!(cat.open_alarms().unwrap().len(), 2);
}

#[test]
fn sealed_products_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let cat = open::<FakeOnCopy>(&dir);
    let (p, pr) = day();
    let r = ingest_tcgcsv::<FakeOnCopy>(cat.conn(), "2026-10-27", "t", &p, &pr[1..2]).unwrap();
    assert_eq!(r.sealed_upserted, 1);
    assert_eq!(count(cat.conn(), "SELECT COUNT(*) FROM sealed_products"), 1);
}

#[test]
fn append_only_triggers_refuse_update_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let cat = open::<FakeOnPrinting>(&dir);
    let (p, pr) = day();
    ingest_tcgcsv::<FakeOnPrinting>(cat.conn(), "2026-10-27", "t", &p, &pr).unwrap();
    let e = cat
        .conn()
        .execute("UPDATE prices SET market = 0", [])
        .unwrap_err();
    assert!(e.to_string().contains("append-only"), "{e}");
    let e = cat.conn().execute("DELETE FROM prices", []).unwrap_err();
    assert!(e.to_string().contains("append-only"), "{e}");
}

#[test]
fn latest_prices_takes_the_newest_day() {
    let dir = tempfile::tempdir().unwrap();
    let cat = open::<FakeOnPrinting>(&dir);
    let (p, mut pr) = day();
    ingest_tcgcsv::<FakeOnPrinting>(cat.conn(), "2026-10-26", "t", &p, &pr).unwrap();
    pr[0].market = Some(2.5);
    ingest_tcgcsv::<FakeOnPrinting>(cat.conn(), "2026-10-27", "t", &p, &pr).unwrap();
    let m: f64 = cat
        .conn()
        .query_row(
            "SELECT market FROM latest_prices WHERE printing_id = 'fp-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(m, 2.5);
}

#[test]
fn missing_day_alarm() {
    let dir = tempfile::tempdir().unwrap();
    let cat = open::<FakeOnPrinting>(&dir);
    let (p, pr) = day();
    ingest_tcgcsv::<FakeOnPrinting>(cat.conn(), "2026-10-24", "t", &p, &pr).unwrap();
    ingest_tcgcsv::<FakeOnPrinting>(cat.conn(), "2026-10-27", "t", &p, &pr).unwrap();
    let missing = check_missing_days(cat.conn(), "fakeprint", TCGCSV, "2026-10-28", "t").unwrap();
    assert_eq!(missing, vec!["2026-10-25", "2026-10-26", "2026-10-28"]);
    let n = cat
        .open_alarms()
        .unwrap()
        .iter()
        .filter(|a| a.kind == MISSING_PRICE_DAY)
        .count();
    assert_eq!(n, 3);
}

#[test]
fn category_mirror_upserts() {
    let dir = tempfile::tempdir().unwrap();
    let cat = open::<FakeOnPrinting>(&dir);
    let c = |name: &str| Category {
        category_id: 79,
        name: name.into(),
        display_name: None,
        modified_on: None,
        sealed_label: None,
        popularity: 1,
    };
    let tx = cat.conn().unchecked_transaction().unwrap();
    assert_eq!(mirror_categories(&tx, &[c("a")]).unwrap(), 1);
    assert_eq!(mirror_categories(&tx, &[c("a")]).unwrap(), 0);
    assert_eq!(mirror_categories(&tx, &[c("b")]).unwrap(), 1);
}
