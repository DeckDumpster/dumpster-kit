//! The recorded payloads in `fixtures/tcgcsv/` (captured 2026-10-12) through the client.
//! Counts below are the recorded ones: `totalItems` / `results` lengths in the captures,
//! and `tcgcsv-swu-counts.txt` for SOR's 550 products, 544 of them cards.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use dumpster_kit_tcgcsv::*;

const STAMP: &str = "2026-10-12T20:05:44+0000";
const SOR: i64 = 23405;
const EMPTY: &[u8] = br#"{"success":true,"errors":[],"results":[]}"#;

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/tcgcsv");
    std::fs::read(p.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Serves recorded bytes by URL and remembers what it was asked for.
struct Recorded {
    by_suffix: BTreeMap<String, Vec<u8>>,
    asked: RefCell<Vec<String>>,
}

impl Recorded {
    fn new(category: i64, groups: &str) -> Self {
        let mut by_suffix = BTreeMap::new();
        by_suffix.insert(format!("/tcgplayer/{category}/groups"), fixture(groups));
        if category == 79 {
            by_suffix.insert(
                format!("/tcgplayer/79/{SOR}/products"),
                fixture("sor-products.json"),
            );
            by_suffix.insert(
                format!("/tcgplayer/79/{SOR}/prices"),
                fixture("sor-prices.json"),
            );
        }
        by_suffix.insert("/last-updated.txt".into(), STAMP.as_bytes().to_vec());
        Self {
            by_suffix,
            asked: RefCell::new(vec![]),
        }
    }
}

impl Fetch for Recorded {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        self.asked.borrow_mut().push(url.to_string());
        Ok(self
            .by_suffix
            .iter()
            .find(|(k, _)| url.ends_with(k.as_str()))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| EMPTY.to_vec()))
    }
}

#[derive(Default)]
struct Kept(RefCell<Vec<Landed>>);

type Landed = (Kind, i64, Option<i64>, Option<String>, usize);

impl Landing for Kept {
    fn land(&self, p: &Payload<'_>) -> Result<()> {
        self.0.borrow_mut().push((
            p.kind,
            p.category,
            p.group_id,
            p.as_of.map(str::to_string),
            p.bytes.len(),
        ));
        Ok(())
    }
}

#[test]
fn category_79_yields_the_recorded_group_product_and_price_counts() {
    let http = Recorded::new(79, "swu-groups.json");
    let Sync::Fresh(s) = TcgcsvClient::new(&http, DEFAULT_BASE, 79)
        .sync(None)
        .unwrap()
    else {
        panic!("first sync must fetch")
    };
    assert_eq!(s.groups.len(), 34);
    assert_eq!(s.products.len(), 34);
    assert_eq!(s.products.iter().map(|(_, p)| p.len()).sum::<usize>(), 550);
    assert_eq!(s.prices.iter().map(|(_, p)| p.len()).sum::<usize>(), 986);
    let sor: &Vec<Product> = &s.products.iter().find(|(g, _)| *g == SOR).unwrap().1;
    assert_eq!(sor.iter().filter(|p| p.is_single_card()).count(), 544);
    assert_eq!((s.stamp.as_str(), s.day.as_str()), (STAMP, "2026-10-12"));
}

#[test]
fn category_71_yields_the_recorded_group_count_and_nothing_cross_category() {
    let http = Recorded::new(71, "lorcana-groups.json");
    let client = TcgcsvClient::new(&http, "https://tcgcsv.com/", 71);
    let groups = client.fetch_groups().unwrap();
    assert_eq!(groups.len(), 21);
    assert!(http.asked.borrow()[0].ends_with("/tcgplayer/71/groups"));
    assert_eq!(client.category(), 71);
}

#[test]
fn an_unchanged_stamp_fetches_nothing_else() {
    let http = Recorded::new(79, "swu-groups.json");
    let kept = Kept::default();
    let r = TcgcsvClient::new(&http, DEFAULT_BASE, 79)
        .landing_in(&kept)
        .sync(Some(STAMP))
        .unwrap();
    assert!(matches!(r, Sync::Unchanged { .. }));
    assert_eq!(http.asked.borrow().len(), 1);
    assert!(http.asked.borrow()[0].ends_with("/last-updated.txt"));
    assert!(kept.0.borrow().is_empty());
}

#[test]
fn every_response_is_landed_and_the_stamp_last() {
    let http = Recorded::new(79, "swu-groups.json");
    let kept = Kept::default();
    TcgcsvClient::new(&http, DEFAULT_BASE, 79)
        .landing_in(&kept)
        .sync(Some("2026-10-11T20:05:44+0000"))
        .unwrap();
    let kept = kept.0.borrow();
    assert_eq!(kept.len(), 1 + 34 * 2 + 1);
    assert_eq!(kept[0].0, Kind::Groups);
    assert_eq!(kept.last().unwrap().0, Kind::LastUpdated);
    assert!(
        kept.iter()
            .all(|k| k.1 == 79 && k.3.as_deref() == Some("2026-10-12"))
    );
    let sor_prices = kept
        .iter()
        .find(|k| k.0 == Kind::Prices && k.2 == Some(SOR))
        .unwrap();
    assert_eq!(sor_prices.4, fixture("sor-prices.json").len());
}

#[test]
fn an_unreadable_stamp_is_refused() {
    let mut http = Recorded::new(79, "swu-groups.json");
    http.by_suffix
        .insert("/last-updated.txt".into(), b"yesterday".to_vec());
    assert!(
        TcgcsvClient::new(&http, DEFAULT_BASE, 79)
            .sync(None)
            .is_err()
    );
}

#[test]
fn stamp_day_reads_only_a_date() {
    assert_eq!(stamp_day(STAMP), Some("2026-10-12"));
    assert_eq!(stamp_day("12/10/2026 x"), None);
}

fn ext<'a>(p: &'a Product, name: &str) -> Option<&'a str> {
    p.extended_data
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

#[test]
fn a_pokemon_product_keeps_every_extended_data_pair_and_its_price_sub_type() {
    let products = parse_products(&fixture("pokemon-trick-or-trade-products.json")).unwrap();
    let phantump = products.iter().find(|p| p.product_id == 515647).unwrap();
    let pairs: Vec<(&str, &str)> = phantump
        .extended_data
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("Number", "016/196"),
            ("Rarity", "Common"),
            ("Card Type", "Grass"),
            ("HP", "70"),
            ("Stage", "Basic"),
            ("Attack 1", "[1] Hook (10)"),
            ("Weakness", "Rx2"),
            ("RetreatCost", "2"),
        ]
    );
    assert_eq!(phantump.number.as_deref(), Some("016/196"));
    let prices = parse_prices(&fixture("pokemon-trick-or-trade-prices.json")).unwrap();
    assert!(
        prices
            .iter()
            .any(|p| p.sub_type_name.as_deref() == Some("Holofoil"))
    );
    assert!(prices.iter().all(|p| p.sub_type_name.is_some()));
}

#[test]
fn an_swu_product_keeps_every_extended_data_pair_and_its_price_sub_type() {
    let products = parse_products(&fixture("sor-products.json")).unwrap();
    let base = products
        .iter()
        .find(|p| ext(p, "CardType") == Some("Base") && p.number.as_deref() == Some("019/252"))
        .unwrap();
    assert_eq!(base.extended_data.len(), 7);
    assert_eq!(ext(base, "Rarity"), Some("Rare"));
    assert_eq!(ext(base, "Aspect"), Some("Vigilance"));
    assert_eq!(ext(base, "Traits"), Some("Scarif"));
    assert_eq!(ext(base, "HP"), Some("25"));
    let sealed = products.iter().find(|p| p.product_id == 533897).unwrap();
    assert!(sealed.extended_data.is_empty());
    let prices = parse_prices(&fixture("sor-prices.json")).unwrap();
    assert_eq!(prices[0].sub_type_name.as_deref(), Some("Normal"));
}

#[test]
fn a_price_without_a_sub_type_name_is_kept_with_none() {
    let raw = br#"{"results":[{"productId":1,"lowPrice":null,"midPrice":null,"highPrice":null,"marketPrice":1.5,"directLowPrice":null,"subTypeName":null}]}"#;
    let prices = parse_prices(raw).unwrap();
    assert_eq!(prices[0].sub_type_name, None);
    assert_eq!(prices[0].market, Some(1.5));
}
