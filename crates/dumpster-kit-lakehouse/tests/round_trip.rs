//! The whole offline loop over the kit's recorded tcgcsv payloads (`fixtures/tcgcsv/`): land
//! them through the real writer, choose the date's runs, recover the clock, then replay the
//! partition back through the same client and require the same records.
//!
//! This is the shape of a site's glue — a `Landing` adapter on the way in, a `Fetch` adapter
//! on the way out — and it is all that glue has to be: the choosing, the clock, the digest
//! check and the miss diagnosis are the kit's.

use std::path::PathBuf;
use std::sync::Arc;

use dumpster_kit_derive::finalize_landing;
use dumpster_kit_lake::{Dataset, DirStore, PartFormat, RawLanding, RawZone, Source};
use dumpster_kit_lakehouse::partition::{TCGCSV, choose, clock_of, provenance};
use dumpster_kit_lakehouse::replay::RawReplay;
use dumpster_kit_tcgcsv as tcgcsv;

const BASE: &str = "https://tcgcsv.example";
const STARTED: &str = "2026-10-12T20:05:44+00:00";
const DATE: &str = "2026-10-12";
const SOR: i64 = 23405;

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/tcgcsv");
    std::fs::read(p.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The recorded upstream.
struct Upstream;

impl tcgcsv::Fetch for Upstream {
    fn get(&self, url: &str) -> tcgcsv::Result<Vec<u8>> {
        let name = if url.ends_with("/79/groups") {
            "swu-groups.json"
        } else if url.ends_with(&format!("/79/{SOR}/products")) {
            "sor-products.json"
        } else if url.ends_with(&format!("/79/{SOR}/prices")) {
            "sor-prices.json"
        } else {
            panic!("the fixture upstream was asked for {url}");
        };
        Ok(fixture(name))
    }
}

/// A site's landing adapter: tcgcsv kind -> lake dataset.
struct IntoLake(Arc<RawLanding>);

impl tcgcsv::Landing for IntoLake {
    fn land(&self, p: &tcgcsv::Payload<'_>) -> tcgcsv::Result<()> {
        let dataset = match p.kind {
            tcgcsv::Kind::Groups => Dataset::GROUPS,
            tcgcsv::Kind::Products => Dataset::PRODUCTS,
            tcgcsv::Kind::Prices => Dataset::PRICES,
            tcgcsv::Kind::LastUpdated | tcgcsv::Kind::Categories => return Ok(()),
        };
        self.0
            .land(
                Source::TCGCSV,
                dataset,
                p.url,
                200,
                PartFormat::Json,
                p.bytes,
            )
            .map_err(|e| tcgcsv::Error::Landing(e.to_string()))
    }
}

/// A site's replay adapter: a landed partition answers the client's requests by URL.
struct FromLake(RawReplay);

impl tcgcsv::Fetch for FromLake {
    fn get(&self, url: &str) -> tcgcsv::Result<Vec<u8>> {
        match self.0.body(url) {
            Ok(Some(body)) => Ok(body),
            Ok(None) => Err(tcgcsv::Error::Fetch {
                url: url.to_string(),
                message: self.0.missing(url).to_string(),
            }),
            Err(e) => Err(tcgcsv::Error::Fetch {
                url: url.to_string(),
                message: e.to_string(),
            }),
        }
    }
}

fn land_the_night(
    root: &std::path::Path,
) -> (Vec<tcgcsv::Group>, Vec<tcgcsv::Product>, Vec<tcgcsv::Price>) {
    let landing = Arc::new(RawLanding::new(
        Box::new(DirStore::new(root)),
        DATE,
        STARTED,
    ));
    let adapter = IntoLake(Arc::clone(&landing));
    let client = tcgcsv::TcgcsvClient::new(&Upstream, BASE, 79).landing_in(&adapter);
    let groups = client.fetch_groups().unwrap();
    let products = client.fetch_products(SOR).unwrap();
    let prices = client.fetch_prices(SOR).unwrap();
    finalize_landing(&landing, None).unwrap();
    (groups, products, prices)
}

#[test]
fn a_landed_night_replays_to_the_records_it_was_fetched_as() {
    let tmp = tempfile::tempdir().unwrap();
    let (groups, products, prices) = land_the_night(tmp.path());
    assert!(!groups.is_empty() && !products.is_empty() && !prices.is_empty());

    let zone = RawZone::new(Box::new(DirStore::new(tmp.path())));
    let plan = choose(&zone, DATE, &TCGCSV).unwrap();
    let clock = clock_of(&plan.chosen, DATE).unwrap();
    assert_eq!(
        clock.observed_date(),
        DATE,
        "the clock comes from the manifest, not today"
    );
    assert_eq!(
        provenance(&plan.chosen, DATE, &clock)
            .iter()
            .map(|p| (p.dataset.as_str(), p.parts, p.complete))
            .collect::<Vec<_>>(),
        [
            ("groups", 1, true),
            ("products", 1, true),
            ("prices", 1, true)
        ]
    );

    let replay = FromLake(RawReplay::new(zone, &plan.chosen).unwrap());
    let client = tcgcsv::TcgcsvClient::new(&replay, BASE, 79);
    assert_eq!(
        format!("{:?}", client.fetch_groups().unwrap()),
        format!("{groups:?}")
    );
    assert_eq!(
        format!("{:?}", client.fetch_products(SOR).unwrap()),
        format!("{products:?}")
    );
    assert_eq!(
        format!("{:?}", client.fetch_prices(SOR).unwrap()),
        format!("{prices:?}")
    );
}

/// The replay has no path to the network: a request the night did not make is the end of
/// the run, and says that coverage regressed.
#[test]
fn a_request_the_night_never_made_is_a_refusal_not_a_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    land_the_night(tmp.path());
    let zone = RawZone::new(Box::new(DirStore::new(tmp.path())));
    let plan = choose(&zone, DATE, &TCGCSV).unwrap();
    let replay = FromLake(RawReplay::new(zone, &plan.chosen).unwrap());
    let client = tcgcsv::TcgcsvClient::new(&replay, BASE, 79);
    let err = client.fetch_products(1).unwrap_err().to_string();
    assert!(err.contains("no record of"), "{err}");
}

/// A date nobody landed is not derived from the newest one that was.
#[test]
fn a_date_that_was_not_landed_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    land_the_night(tmp.path());
    let zone = RawZone::new(Box::new(DirStore::new(tmp.path())));
    assert!(choose(&zone, "2026-10-13", &TCGCSV).is_err());
}
