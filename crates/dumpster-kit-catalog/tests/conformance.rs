//! Runs `conformance/catalog` (embedded in dumpster-kit-conformance) against the kit's fake games,
//! and proves the runner fails on implementations that break the rules the cases state.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;

use dumpster_kit_catalog::fake::{FakeOnCopy, FakeOnPrinting};
use dumpster_kit_catalog::{
    ApplyReport, Catalog, FinishPlacement, Game, Match, RunContext, Source, SourceOutcome,
    ingest_tcgcsv,
};
use dumpster_kit_conformance::{CatalogCase, catalog_cases};
use dumpster_kit_tcgcsv::{Error as FetchError, Fetch, Price, Product};
use serde_json::Value;

#[derive(Default)]
struct Feed(RefCell<HashMap<String, Option<Vec<u8>>>>);

impl Fetch for Feed {
    fn get(&self, url: &str) -> dumpster_kit_tcgcsv::Result<Vec<u8>> {
        match self.0.borrow().get(url) {
            Some(Some(b)) => Ok(b.clone()),
            _ => Err(FetchError::Fetch {
                url: url.into(),
                message: "down".into(),
            }),
        }
    }
}

fn str_of<'a>(step: &'a Value, key: &str) -> Result<&'a str, String> {
    step[key]
        .as_str()
        .ok_or_else(|| format!("step {step} lacks string {key}"))
}

fn count(c: &rusqlite::Connection, table: &str) -> Result<i64, String> {
    c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// Play one case against game `G`; the first rule it breaks comes back as the error.
fn play<G: Game>(case: &CatalogCase, tamper: &str) -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let raw = dir.path().join("raw");
    let cat =
        Catalog::<G>::open(&dir.path().join("shared.sqlite"), &raw).map_err(|e| e.to_string())?;
    cat.conn()
        .execute_batch(tamper)
        .map_err(|e| e.to_string())?;
    let feed = Feed::default();
    for (n, step) in case.steps.iter().enumerate() {
        let at = |m: String| format!("{} step {}: {m}", case.name, n + 1);
        match str_of(step, "op").map_err(at)? {
            "feed" => {
                if let Some(set) = step["set"].as_object() {
                    for (loc, body) in set {
                        let body = body.as_str().ok_or_else(|| at("body".into()))?;
                        feed.0
                            .borrow_mut()
                            .insert(loc.clone(), Some(body.as_bytes().to_vec()));
                    }
                }
                for loc in step["fail"].as_array().into_iter().flatten() {
                    let loc = loc.as_str().ok_or_else(|| at("fail".into()))?;
                    feed.0.borrow_mut().insert(loc.into(), None);
                }
            }
            "run" => {
                let day = str_of(step, "day").map_err(at)?;
                let got = cat
                    .run(&feed, RunContext { day, at: day })
                    .map_err(|e| at(e.to_string()))?;
                let want = step["expect"]
                    .as_array()
                    .ok_or_else(|| at("expect".into()))?;
                if got.len() != want.len() {
                    return Err(at(format!("{} outcomes, wanted {}", got.len(), want.len())));
                }
                for (g, w) in got.iter().zip(want) {
                    let (source, outcome, total) = match g {
                        SourceOutcome::Applied { source, report, .. } => {
                            (source, "applied", Some(report.total()))
                        }
                        SourceOutcome::Unchanged { source, .. } => (source, "unchanged", None),
                        SourceOutcome::FetchFailed { source, .. } => (source, "fetch_failed", None),
                        SourceOutcome::ApplyFailed { source, .. } => (source, "apply_failed", None),
                    };
                    if source != str_of(w, "source").map_err(at)?
                        || outcome != str_of(w, "outcome").map_err(at)?
                    {
                        return Err(at(format!("{source} {outcome}, wanted {w}")));
                    }
                    if let Some(t) = w["total"].as_u64()
                        && total != Some(t)
                    {
                        return Err(at(format!("{source}: {total:?} rows changed, wanted {t}")));
                    }
                }
            }
            "clear_ledger" => {
                cat.conn()
                    .execute("DELETE FROM applied_payloads", [])
                    .map_err(|e| e.to_string())?;
            }
            "ledger" => {
                let n = count(cat.conn(), "applied_payloads")?;
                if Some(n) != step["count"].as_i64() {
                    return Err(at(format!("{n} ledger rows, wanted {}", step["count"])));
                }
            }
            "rows" => {
                let counts = step["counts"]
                    .as_object()
                    .ok_or_else(|| at("counts".into()))?;
                for (table, want) in counts {
                    let n = count(cat.conn(), table)?;
                    if Some(n) != want.as_i64() {
                        return Err(at(format!("{table}: {n} rows, wanted {want}")));
                    }
                }
            }
            "alarms" => {
                let got: Vec<(String, String)> = cat
                    .open_alarms()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|a| (a.kind, a.subject))
                    .collect();
                let want: Vec<(String, String)> = step["open"]
                    .as_array()
                    .ok_or_else(|| at("open".into()))?
                    .iter()
                    .map(|p| {
                        (
                            p[0].as_str().unwrap_or_default().to_string(),
                            p[1].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                if got != want {
                    return Err(at(format!("open alarms {got:?}, wanted {want:?}")));
                }
            }
            "tcgcsv" => {
                let day = str_of(step, "day").map_err(at)?;
                let products: Vec<Product> = step["products"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| Product {
                        product_id: p["id"].as_i64().unwrap_or_default(),
                        group_id: 1,
                        name: p["name"].as_str().unwrap_or_default().into(),
                        number: p["number"].as_str().map(Into::into),
                        extended_data: vec![],
                        image_url: None,
                        image_count: 0,
                        url: None,
                    })
                    .collect();
                let prices: Vec<Price> = step["prices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| Price {
                        product_id: p["id"].as_i64().unwrap_or_default(),
                        sub_type_name: p["sub"].as_str().map(Into::into),
                        low: None,
                        mid: None,
                        high: None,
                        market: p["market"].as_f64(),
                        direct_low: None,
                    })
                    .collect();
                let r = ingest_tcgcsv::<G>(cat.conn(), day, day, &products, &prices)
                    .map_err(|e| at(e.to_string()))?;
                for (key, got) in [
                    ("prices", r.prices_inserted),
                    ("crosswalked", r.crosswalked),
                    ("sealed", r.sealed_upserted),
                ] {
                    if let Some(w) = step["expect"][key].as_u64()
                        && got != w
                    {
                        return Err(at(format!("{key}: {got}, wanted {w}")));
                    }
                }
                if let Some(w) = step["expect"]["unmatched"].as_array() {
                    let want: Vec<&str> = w.iter().filter_map(Value::as_str).collect();
                    if r.unmatched != want {
                        return Err(at(format!("unmatched {:?}, wanted {want:?}", r.unmatched)));
                    }
                }
            }
            "sql_refused" => {
                let sql = str_of(step, "sql").map_err(at)?;
                let why = str_of(step, "why").map_err(at)?;
                match cat.conn().execute(sql, []) {
                    Err(e) if e.to_string().contains(why) => {}
                    Err(e) => return Err(at(format!("{sql} failed as {e}, wanted {why}"))),
                    Ok(n) => return Err(at(format!("{sql} was allowed ({n} rows)"))),
                }
            }
            "latest" => {
                let id = str_of(step, "printing_id").map_err(at)?;
                let finish = step["finish"].as_str().unwrap_or("");
                let got: Option<f64> = cat
                    .conn()
                    .query_row(
                        "SELECT market FROM latest_prices WHERE printing_id = ?1 AND finish = ?2",
                        [id, finish],
                        |r| r.get(0),
                    )
                    .map_err(|e| at(e.to_string()))?;
                if got != step["market"].as_f64() {
                    return Err(at(format!(
                        "latest {id}/{finish} is {got:?}, wanted {}",
                        step["market"]
                    )));
                }
            }
            "price_keys" => {
                let mut stmt = cat
                    .conn()
                    .prepare("SELECT DISTINCT printing_id, finish FROM prices ORDER BY 1, 2")
                    .map_err(|e| e.to_string())?;
                let got: Vec<(String, String)> = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .and_then(|r| r.collect())
                    .map_err(|e| e.to_string())?;
                let want: Vec<(String, String)> = step["keys"]
                    .as_array()
                    .ok_or_else(|| at("keys".into()))?
                    .iter()
                    .map(|p| {
                        (
                            p[0].as_str().unwrap_or_default().to_string(),
                            p[1].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                if got != want {
                    return Err(at(format!("price keys {got:?}, wanted {want:?}")));
                }
            }
            "crosswalk" => {
                let got: Option<String> = cat
                    .conn()
                    .query_row(
                        "SELECT printing_id FROM printing_source_id
                         WHERE source = 'tcgcsv' AND source_id = ?1 AND sub_type = ?2",
                        [
                            str_of(step, "source_id").map_err(at)?,
                            step["sub_type"].as_str().unwrap_or(""),
                        ],
                        |r| r.get(0),
                    )
                    .ok();
                if got.as_deref() != step["printing_id"].as_str() {
                    return Err(at(format!(
                        "crosswalk {got:?}, wanted {}",
                        step["printing_id"]
                    )));
                }
            }
            "raw" => {
                let d = raw
                    .join(G::ID)
                    .join(str_of(step, "source").map_err(at)?)
                    .join(str_of(step, "day").map_err(at)?);
                let n = fs::read_dir(&d).map(|r| r.count()).unwrap_or(0);
                if Some(n as u64) != step["files"].as_u64() {
                    return Err(at(format!("{n} landed files, wanted {}", step["files"])));
                }
            }
            other => return Err(at(format!("unknown op {other}"))),
        }
    }
    Ok(())
}

const NO_TAMPER: &str = "";
const DROP_TRIGGERS: &str = "DROP TRIGGER prices_no_update; DROP TRIGGER prices_no_delete;";
const OLDEST_IS_LATEST: &str = "DROP VIEW latest_prices; CREATE VIEW latest_prices AS
    SELECT p.* FROM prices p WHERE p.observed_on = (SELECT MIN(q.observed_on) FROM prices q
        WHERE q.printing_id = p.printing_id AND q.finish = p.finish AND q.source = p.source);";
const FORGETFUL_CROSSWALK: &str = "CREATE TRIGGER forget AFTER INSERT ON printing_source_id
    BEGIN DELETE FROM printing_source_id; END;";

fn play_named(game: &str, case: &CatalogCase) -> Result<(), String> {
    match game {
        "fakeprint" => play::<FakeOnPrinting>(case, NO_TAMPER),
        "fakecopy" => play::<FakeOnCopy>(case, NO_TAMPER),
        "broken-chatty" => play::<Chatty>(case, NO_TAMPER),
        "broken-lenient" => play::<Lenient>(case, NO_TAMPER),
        "broken-mutable-prices" => play::<FakeOnPrinting>(case, DROP_TRIGGERS),
        "broken-oldest-latest" => play::<FakeOnPrinting>(case, OLDEST_IS_LATEST),
        "broken-forgetful-crosswalk" => play::<FakeOnPrinting>(case, FORGETFUL_CROSSWALK),
        "broken-matches-everything" => play::<MatchesEverything>(case, NO_TAMPER),
        "broken-finish-on-printing" => play::<FinishOnPrinting>(case, NO_TAMPER),
        "broken-finishless-copy" => play::<FinishlessCopy>(case, NO_TAMPER),
        other => Err(format!("{}: unknown game {other}", case.name)),
    }
}

/// Every case, run against its own game; names of the cases that failed, with why. With `swap`,
/// the cases written for `swap.1` run against the broken implementation `swap.0` instead.
fn failures(swap: Option<(&str, &str)>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for case in catalog_cases() {
        if swap.is_some_and(|(_, g)| case.game != g) {
            continue;
        }
        if let Err(e) = play_named(swap.map_or(&case.game, |(b, _)| b), &case) {
            out.insert(case.name.clone(), e);
        }
    }
    out
}

macro_rules! delegate_game {
    ($id:expr) => {
        const ID: &'static str = $id;
        const SCHEMA_VERSION: u32 = FakeOnPrinting::SCHEMA_VERSION;
        const SCHEMA: &'static str = FakeOnPrinting::SCHEMA;
        const FINISH: FinishPlacement = FakeOnPrinting::FINISH;
        type Payload = <FakeOnPrinting as Game>::Payload;
        fn sources() -> Vec<Source> {
            FakeOnPrinting::sources()
        }
        fn parse(s: &str, r: &[u8]) -> dumpster_kit_catalog::Result<Self::Payload> {
            FakeOnPrinting::parse(s, r)
        }
        fn search_terms() -> Vec<String> {
            FakeOnPrinting::search_terms()
        }
        fn tcgcsv_category() -> i64 {
            FakeOnPrinting::tcgcsv_category()
        }
    };
}

/// Breaks idempotence: claims a row changed on every apply.
struct Chatty;
impl Game for Chatty {
    delegate_game!("fakeprint");
    fn crosswalk(p: &Product, q: Option<&Price>) -> Match {
        FakeOnPrinting::crosswalk(p, q)
    }
    fn apply(
        tx: &rusqlite::Transaction<'_>,
        p: &Self::Payload,
    ) -> dumpster_kit_catalog::Result<ApplyReport> {
        let mut r = FakeOnPrinting::apply(tx, p)?;
        r.updated("fp_things", 1);
        Ok(r)
    }
}

/// Breaks atomicity: swallows an apply error, so the rows written before it are committed.
struct Lenient;
impl Game for Lenient {
    delegate_game!("fakeprint");
    fn crosswalk(p: &Product, q: Option<&Price>) -> Match {
        FakeOnPrinting::crosswalk(p, q)
    }
    fn apply(
        tx: &rusqlite::Transaction<'_>,
        p: &Self::Payload,
    ) -> dumpster_kit_catalog::Result<ApplyReport> {
        Ok(FakeOnPrinting::apply(tx, p).unwrap_or_default())
    }
}

macro_rules! delegate_apply {
    () => {
        fn apply(
            tx: &rusqlite::Transaction<'_>,
            p: &Self::Payload,
        ) -> dumpster_kit_catalog::Result<ApplyReport> {
            FakeOnPrinting::apply(tx, p)
        }
    };
}

/// Breaks the unmatched alarm: matches every product, so nothing is ever unmatched.
struct MatchesEverything;
impl Game for MatchesEverything {
    delegate_game!("fakeprint");
    delegate_apply!();
    fn crosswalk(p: &Product, _: Option<&Price>) -> Match {
        Match::Printing {
            printing_id: format!("fp-{}", p.product_id),
            finish: None,
        }
    }
}

/// Breaks OnPrinting keying: hands a finish to a game that prices by printing id.
struct FinishOnPrinting;
impl Game for FinishOnPrinting {
    delegate_game!("fakeprint");
    delegate_apply!();
    fn crosswalk(p: &Product, q: Option<&Price>) -> Match {
        match FakeOnPrinting::crosswalk(p, q) {
            Match::Printing { printing_id, .. } => Match::Printing {
                printing_id,
                finish: Some("Normal".into()),
            },
            other => other,
        }
    }
}

/// Breaks OnCopy keying: an OnCopy game whose crosswalk names no finish.
struct FinishlessCopy;
impl Game for FinishlessCopy {
    const ID: &'static str = "fakecopy";
    const SCHEMA_VERSION: u32 = FakeOnCopy::SCHEMA_VERSION;
    const SCHEMA: &'static str = FakeOnCopy::SCHEMA;
    const FINISH: FinishPlacement = FakeOnCopy::FINISH;
    type Payload = <FakeOnCopy as Game>::Payload;
    fn sources() -> Vec<Source> {
        FakeOnCopy::sources()
    }
    fn parse(s: &str, r: &[u8]) -> dumpster_kit_catalog::Result<Self::Payload> {
        FakeOnCopy::parse(s, r)
    }
    fn apply(
        tx: &rusqlite::Transaction<'_>,
        p: &Self::Payload,
    ) -> dumpster_kit_catalog::Result<ApplyReport> {
        FakeOnCopy::apply(tx, p)
    }
    fn crosswalk(p: &Product, q: Option<&Price>) -> Match {
        match FakeOnCopy::crosswalk(p, q) {
            Match::Printing { printing_id, .. } => Match::Printing {
                printing_id,
                finish: None,
            },
            other => other,
        }
    }
    fn search_terms() -> Vec<String> {
        FakeOnCopy::search_terms()
    }
    fn tcgcsv_category() -> i64 {
        FakeOnCopy::tcgcsv_category()
    }
}

#[test]
fn every_catalog_case_passes_on_the_fake_games() {
    let failed = failures(None);
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn a_game_that_reports_unchanged_rows_as_changed_is_caught() {
    let failed = failures(Some(("broken-chatty", "fakeprint")));
    for name in [
        "first_apply_inserts_and_enters_the_ledger",
        "reapplying_past_the_ledger_changes_zero_rows",
        "a_changed_payload_reports_only_the_changed_rows",
    ] {
        assert!(failed.contains_key(name), "{name} passed a chatty game");
    }
}

#[test]
fn a_game_that_swallows_an_apply_error_is_caught() {
    let failed = failures(Some(("broken-lenient", "fakeprint")));
    let name = "a_payload_failing_mid_apply_leaves_no_partial_rows";
    assert!(failed.contains_key(name), "{name} passed a lenient game");
}

fn assert_caught(broken: &str, game: &str, names: &[&str]) {
    let failed = failures(Some((broken, game)));
    for name in names {
        assert!(failed.contains_key(*name), "{name} passed {broken}");
    }
}

#[test]
fn mutable_prices_are_caught() {
    assert_caught(
        "broken-mutable-prices",
        "fakeprint",
        &["prices_are_append_only_an_update_is_refused"],
    );
}

#[test]
fn a_latest_view_that_picks_the_oldest_day_is_caught() {
    assert_caught(
        "broken-oldest-latest",
        "fakeprint",
        &["latest_prices_picks_the_newest_observed_on"],
    );
}

#[test]
fn a_crosswalk_store_that_forgets_is_caught() {
    assert_caught(
        "broken-forgetful-crosswalk",
        "fakeprint",
        &["a_matched_product_is_stored_in_the_crosswalk"],
    );
}

#[test]
fn a_crosswalk_that_matches_everything_is_caught() {
    assert_caught(
        "broken-matches-everything",
        "fakeprint",
        &["unmatched_products_raise_one_alarm_each_and_replay_adds_none"],
    );
}

#[test]
fn a_finish_handed_to_an_on_printing_game_is_caught() {
    assert_caught(
        "broken-finish-on-printing",
        "fakeprint",
        &["on_printing_prices_carry_no_finish"],
    );
}

#[test]
fn an_on_copy_game_without_a_finish_is_caught() {
    assert_caught(
        "broken-finishless-copy",
        "fakecopy",
        &["on_copy_prices_are_keyed_by_finish"],
    );
}
