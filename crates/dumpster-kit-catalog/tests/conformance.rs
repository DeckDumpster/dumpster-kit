//! Runs `conformance/catalog` (embedded in dumpster-kit-conformance) against the kit's fake games,
//! and proves the runner fails on implementations that break the rules the cases state.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;

use dumpster_kit_catalog::fake::{FakeOnCopy, FakeOnPrinting};
use dumpster_kit_catalog::{
    ApplyReport, Catalog, FinishPlacement, Game, Match, RunContext, Source, SourceOutcome,
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
fn play<G: Game>(case: &CatalogCase) -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let raw = dir.path().join("raw");
    let cat =
        Catalog::<G>::open(&dir.path().join("shared.sqlite"), &raw).map_err(|e| e.to_string())?;
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

fn play_named(game: &str, case: &CatalogCase) -> Result<(), String> {
    match game {
        "fakeprint" => play::<FakeOnPrinting>(case),
        "fakecopy" => play::<FakeOnCopy>(case),
        "broken-chatty" => play::<Chatty>(case),
        "broken-lenient" => play::<Lenient>(case),
        other => Err(format!("{}: unknown game {other}", case.name)),
    }
}

/// Every case, run against its own game; names of the cases that failed, with why.
fn failures(swap: Option<&str>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for case in catalog_cases() {
        if swap.is_some() && case.game != "fakeprint" {
            continue;
        }
        if let Err(e) = play_named(swap.unwrap_or(&case.game), &case) {
            out.insert(case.name.clone(), e);
        }
    }
    out
}

macro_rules! delegate_game {
    ($t:ident, $id:expr) => {
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
        fn crosswalk(p: &Product, q: Option<&Price>) -> Match {
            FakeOnPrinting::crosswalk(p, q)
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
    delegate_game!(Chatty, "fakeprint");
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
    delegate_game!(Lenient, "fakeprint");
    fn apply(
        tx: &rusqlite::Transaction<'_>,
        p: &Self::Payload,
    ) -> dumpster_kit_catalog::Result<ApplyReport> {
        Ok(FakeOnPrinting::apply(tx, p).unwrap_or_default())
    }
}

#[test]
fn every_catalog_case_passes_on_the_fake_games() {
    let failed = failures(None);
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn a_game_that_reports_unchanged_rows_as_changed_is_caught() {
    let failed = failures(Some("broken-chatty"));
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
    let failed = failures(Some("broken-lenient"));
    let name = "a_payload_failing_mid_apply_leaves_no_partial_rows";
    assert!(failed.contains_key(name), "{name} passed a lenient game");
}
