//! Which raw runs a date's derive reads, and when it refuses to run at all.
//!
//! The derive is scheduled separately from the landing, deliberately: a
//! derive can then run against yesterday's raw on a night the fetch failed.
//! The trap on the other side of that feature is the one this module exists
//! to close —
//!
//! > yesterday's raw silently deriving today's `shared` and looking current.
//!
//! So there is no "newest available" anywhere in here. The job is told a date
//! and builds *that* date or stops. What this module decides is the narrower
//! question of which of a date's runs to read, and what makes a date
//! underivable:
//!
//! - **a required dataset that landed nothing** — the date was never fetched,
//!   or was fetched by something that is not a refresh. Refuse.
//! - **a required dataset with no complete run** — parts exist, but nothing in
//!   the landing zone can say whether they add up to a day (the writer never
//!   learns how many parts it was owed). A smaller catalog reads as *cards
//!   that do not exist*, so refuse rather than build one.
//! - **runs that disagree about the clock** — see [`clock_of`].
//!
//! Every dataset is all-or-nothing: an incomplete TCGCSV prefix is a refusal,
//! because the shortfall is unknowable — 200 groups of an unknown 450 — and a
//! price is a fact about one day that there is no asking for later.

use std::collections::BTreeMap;

use dumpster_kit_derive::DeriveClock;
use dumpster_kit_lake::{Dataset, RawZone, Run, Source, select_run};

/// Whether a derive can proceed without a dataset's bytes at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Every refresh fetches this. Absent is a refusal.
    Required,
    /// A refresh may legitimately not have fetched this. Absent contributes
    /// no URLs and any request for one becomes a replay miss, which is loud.
    Optional,
}

/// What a derive does with a dataset that has runs but no complete one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incomplete {
    /// Refuse the date. Nothing in the landing zone can say whether the parts
    /// add up to a day, so a smaller catalog would read as cards that do not
    /// exist.
    Refuse,
    /// Read the newest run even though it is not complete — for a site whose
    /// derive records the shortfall itself.
    Partial,
}

/// One dataset's line in a site's requirement table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    /// Where the dataset's payloads come from.
    pub source: Source,
    /// Which endpoint's payloads.
    pub dataset: Dataset,
    /// Whether a derive can run without it.
    pub need: Need,
}

/// A site's requirement table: which datasets a date's derive reads, in the
/// order they are chosen and reported, and what an incomplete run means.
///
/// The kit enumerates no site's upstreams; each consumer states its own.
/// Deciding whether a rebuild may run without a dataset is the coverage
/// question, and a row here is where it is asked for the *reading* side.
#[derive(Debug, Clone, Copy)]
pub struct Requirements<'a> {
    /// One row per dataset, in choose order.
    pub rows: &'a [Requirement],
    /// What a dataset with no complete run means.
    pub incomplete: Incomplete,
}

/// The TCGCSV table dumpstar has always used: groups, products and prices,
/// all required, an incomplete run a refusal.
pub const TCGCSV: Requirements<'static> = Requirements {
    rows: &[
        Requirement {
            source: Source::TCGCSV,
            dataset: Dataset::GROUPS,
            need: Need::Required,
        },
        Requirement {
            source: Source::TCGCSV,
            dataset: Dataset::PRODUCTS,
            need: Need::Required,
        },
        Requirement {
            source: Source::TCGCSV,
            dataset: Dataset::PRICES,
            need: Need::Required,
        },
    ],
    incomplete: Incomplete::Refuse,
};

/// One dataset's chosen run for the date being derived.
#[derive(Debug, Clone)]
pub struct Chosen {
    /// Where the payloads came from.
    pub source: Source,
    /// Which endpoint's payloads.
    pub dataset: Dataset,
    /// The run selected — newest complete, per [`select_run`].
    pub run: Run,
}

impl Chosen {
    /// `source/dataset date`, the phrase every refusal names.
    pub fn what(&self, ingest_date: &str) -> String {
        format!("{}/{} {ingest_date}", self.source, self.dataset)
    }
}

/// The runs a date's derive will read.
#[derive(Debug, Clone)]
pub struct Plan {
    /// One run per dataset, in the requirement table's order.
    pub chosen: Vec<Chosen>,
}

/// Choose one run per row of the site's `table` for `ingest_date`, refusing a
/// date that cannot be derived.
///
/// Returns the chosen runs in table order — deterministic, so the job's output
/// and its provenance rows do not depend on a listing's order.
///
/// A dataset with no complete run is a refusal unless the table says
/// [`Incomplete::Partial`].
pub fn choose(zone: &RawZone, ingest_date: &str, table: &Requirements<'_>) -> anyhow::Result<Plan> {
    let mut chosen = Vec::new();
    for &Requirement {
        source,
        dataset,
        need,
    } in table.rows
    {
        let what = format!("{source}/{dataset} {ingest_date}");
        let runs = zone.runs(source, dataset, ingest_date)?;

        if runs.is_empty() && need == Need::Optional {
            println!("  {what}: nothing landed (optional)");
            continue;
        }
        // `select_run` is the shared rule — newest complete wins, no complete
        // run refuses — and it is deliberately the same function the Python
        // side documents, so two jobs cannot disagree about what a date IS.
        let run = match (table.incomplete, runs.last()) {
            (Incomplete::Partial, Some(newest)) if !runs.iter().any(|r| r.manifest.complete) => {
                newest.clone()
            }
            _ => select_run(&runs, &what)?,
        };
        println!(
            "  {what}: {} ({} part(s))",
            run.run_id,
            run.manifest.parts.len()
        );
        chosen.push(Chosen {
            source,
            dataset,
            run,
        });
    }
    Ok(Plan { chosen })
}

/// The clock every chosen run agrees on.
///
/// A derive reproduces **one fetch**. Its rows carry `fetched_at` and
/// `observed_at` values that came from the clock the fetching run read once,
/// and the manifest is where that instant was written down — so recovering it
/// is what makes "row-identical" mean identical rather than
/// identical-apart-from-the-timestamps.
///
/// Two runs of one date that disagree about it are two different fetches, and
/// rows stamped from a blend of them would be neither run's output. That is
/// refused, naming both, rather than resolved by a rule (newest? first?) that
/// would quietly pick one fetch's bytes and the other's clock. It happens when
/// a date was landed by two invocations — a `setup` and a `refresh` on the same
/// day, say — and the remedy is to derive a date one run landed.
///
/// A run whose manifest carries no clock at all is refused by
/// [`DeriveClock::from_manifest`], which says which partition and what to do.
pub fn clock_of(chosen: &[Chosen], ingest_date: &str) -> anyhow::Result<DeriveClock> {
    let mut by_instant: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for c in chosen {
        by_instant
            .entry(c.run.manifest.started_at.trim())
            .or_default()
            .push(format!("{} run={}", c.what(ingest_date), c.run.run_id));
    }

    if by_instant.len() > 1 {
        let detail = by_instant
            .iter()
            .map(|(instant, who)| {
                let instant = if instant.is_empty() {
                    "(none)"
                } else {
                    instant
                };
                format!("{instant}: {}", who.join(", "))
            })
            .collect::<Vec<_>>()
            .join("\n  ");
        anyhow::bail!(
            "{ingest_date}: the runs this derive would read were fetched at different \
             instants, so there is no single clock to stamp its rows with:\n  {detail}\n\
             A derive reproduces one fetch. Rows built from one run's bytes and another \
             run's clock are neither run's output, which is precisely the \"old data looks \
             new\" failure the ingest_date partition exists to prevent.\n\
             Derive a date that one run landed, or re-land this one \
             (the site's data refresh) so every dataset comes from the same run."
        );
    }

    let (instant, who) = by_instant
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("{ingest_date}: no runs were chosen"))?;
    DeriveClock::from_manifest(instant, &who.join(", "))
}

/// One raw partition a derive consumed: the row a site records in its catalog's provenance
/// table. The table is the site's; the facts are the lakehouse's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// The `ingest_date=` partition the bytes were read from.
    pub ingest_date: String,
    /// The `source=` partition value.
    pub source: String,
    /// The `dataset=` partition value.
    pub dataset: String,
    /// The `run=` ULID that was replayed.
    pub run_id: String,
    /// How many payloads that run landed under this prefix.
    pub parts: i64,
    /// The manifest's `complete` flag. Always true today — the derive refuses an incomplete
    /// run — and recorded anyway so the row states the fact rather than implying it from the
    /// job's current policy.
    pub complete: bool,
    /// The run's clock day, `YYYY-MM-DD`. Distinct from `ingest_date`: they differ for
    /// exactly the run that crossed UTC midnight.
    pub observed_at: String,
}

/// The provenance rows for a completed derive.
pub fn provenance(chosen: &[Chosen], ingest_date: &str, clock: &DeriveClock) -> Vec<Provenance> {
    chosen
        .iter()
        .map(|c| Provenance {
            ingest_date: ingest_date.to_string(),
            source: c.source.to_string(),
            dataset: c.dataset.to_string(),
            run_id: c.run.run_id.clone(),
            parts: c.run.manifest.parts.len() as i64,
            complete: c.run.manifest.complete,
            observed_at: clock.observed_date().to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumpster_kit_lake::{DirStore, PartFormat, RawLanding};

    const STARTED: &str = "2026-08-11T04:51:02+00:00";
    const DATE: &str = "2026-08-11";

    /// Land the three required datasets through the real writer, so the reader
    /// under test is looking at bytes the landing zone actually produced.
    fn land(root: &std::path::Path, date: &str, started: &str, fail: bool) -> String {
        let sink = RawLanding::new(Box::new(DirStore::new(root)), date, started);
        for (source, dataset, url) in [
            (Source::TCGCSV, Dataset::GROUPS, "https://up/3/groups"),
            (Source::TCGCSV, Dataset::PRODUCTS, "https://up/3/1/products"),
            (Source::TCGCSV, Dataset::PRICES, "https://up/3/1/prices"),
        ] {
            sink.land(source, dataset, url, 200, PartFormat::Json, b"{}")
                .unwrap();
        }
        sink.finalize(if fail { Some("http: 503") } else { None })
            .unwrap();
        sink.run_id().to_string()
    }

    fn zone(root: &std::path::Path) -> RawZone {
        RawZone::new(Box::new(DirStore::new(root)))
    }

    #[test]
    fn a_complete_date_chooses_every_required_dataset_and_no_more() {
        let tmp = tempfile::tempdir().unwrap();
        let run = land(tmp.path(), DATE, STARTED, false);
        let plan = choose(&zone(tmp.path()), DATE, &TCGCSV).unwrap();
        let chosen = plan.chosen;

        assert_eq!(chosen.len(), 3, "the datasets every refresh fetches");
        assert!(chosen.iter().all(|c| c.run.run_id == run));
        assert_eq!(clock_of(&chosen, DATE).unwrap().observed_date(), DATE);
    }

    /// The refusal the whole "two units" design turns on. A date nobody landed
    /// must not derive from whatever else is lying around.
    #[test]
    fn a_date_that_landed_nothing_refuses_rather_than_reaching_for_another() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), "2026-08-10", STARTED, false);
        let err = choose(&zone(tmp.path()), "2026-08-11", &TCGCSV)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no runs landed"), "{err}");
        assert!(err.contains("2026-08-11"), "{err}");
        assert!(err.contains("never falls back"), "{err}");
    }

    /// A run that died partway holds real bytes and an unknown fraction of the
    /// day. Building from it would produce a catalog that is quietly smaller.
    #[test]
    fn an_incomplete_run_refuses_and_names_what_it_found() {
        let tmp = tempfile::tempdir().unwrap();
        let run = land(tmp.path(), DATE, STARTED, true);
        let err = choose(&zone(tmp.path()), DATE, &TCGCSV)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no complete run"), "{err}");
        assert!(err.contains(&run), "{err}");
    }

    /// …and a complete run followed by a failed retry still derives, from the
    /// complete one. Otherwise a retry that died would poison a good night.
    #[test]
    fn a_failed_retry_does_not_beat_the_complete_run_before_it() {
        let tmp = tempfile::tempdir().unwrap();
        let good = land(tmp.path(), DATE, STARTED, false);
        land(tmp.path(), DATE, "2026-08-11T05:10:00+00:00", true);
        let plan = choose(&zone(tmp.path()), DATE, &TCGCSV).unwrap();
        assert!(plan.chosen.iter().all(|c| c.run.run_id == good));
    }

    /// Two complete runs of one date, fetched at different instants. Every
    /// dataset resolves, and there is still no answer to "what time is it".
    #[test]
    fn runs_that_disagree_about_the_clock_refuse_naming_both() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), DATE, STARTED, false);
        let mut chosen = choose(&zone(tmp.path()), DATE, &TCGCSV).unwrap().chosen;
        // A second invocation landed `prices` an hour later — the shape a setup
        // and a refresh on one day leaves behind.
        chosen[2].run.manifest.started_at = "2026-08-11T05:51:02+00:00".to_string();

        let err = clock_of(&chosen, DATE).unwrap_err().to_string();
        assert!(err.contains("different instants"), "{err}");
        assert!(err.contains("04:51:02"), "{err}");
        assert!(err.contains("05:51:02"), "{err}");
        assert!(err.contains("tcgcsv/prices"), "{err}");
    }

    /// A partition landed before the manifest recorded a clock cannot be
    /// derived from at all, and the refusal has to say which one and why.
    #[test]
    fn a_clockless_partition_refuses_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), DATE, "", false);
        let chosen = choose(&zone(tmp.path()), DATE, &TCGCSV).unwrap().chosen;
        let err = clock_of(&chosen, DATE).unwrap_err().to_string();
        assert!(err.contains("records no started_at"), "{err}");
        assert!(err.contains("tcgcsv/groups"), "{err}");
    }

    /// Provenance names the run, not the derive: two derives of one date
    /// produce the same rows apart from `derived_at`, which is what makes a
    /// rerun identifiable rather than invisible.
    #[test]
    fn provenance_names_the_runs_that_were_read() {
        let tmp = tempfile::tempdir().unwrap();
        let run = land(tmp.path(), DATE, STARTED, false);
        let chosen = choose(&zone(tmp.path()), DATE, &TCGCSV).unwrap().chosen;
        let clock = clock_of(&chosen, DATE).unwrap();
        let rows = provenance(&chosen, DATE, &clock);

        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.run_id == run && r.complete));
        assert!(rows.iter().all(|r| r.parts == 1));
        assert_eq!(rows[0].source, "tcgcsv");
        assert_eq!(rows[0].dataset, "groups");
    }

    /// The UTC-midnight case, stated as an assertion rather than a comment: a
    /// run that started at 23:59 on the 10th and landed under the 11th's
    /// partition observed the 10th, and its rows must say so.
    #[test]
    fn the_observed_day_comes_from_the_fetch_not_the_partition() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), "2026-08-11", "2026-08-10T23:59:30+00:00", false);
        let chosen = choose(&zone(tmp.path()), "2026-08-11", &TCGCSV)
            .unwrap()
            .chosen;
        let clock = clock_of(&chosen, "2026-08-11").unwrap();

        assert_eq!(clock.observed_date(), "2026-08-10");
        let rows = provenance(&chosen, "2026-08-11", &clock);
        assert_eq!(rows[0].ingest_date, "2026-08-11");
        assert_eq!(rows[0].observed_at, "2026-08-10");
    }

    /// A site that is not dumpstar defines its own consts and table, and
    /// `choose` partitions by them — never touching the kit's TCGCSV ones.
    #[test]
    fn a_site_partitions_by_its_own_consts_and_table() {
        const SRC: Source = Source::new("other-site");
        const LISTINGS: Dataset = Dataset::new("listings");
        const EXTRAS: Dataset = Dataset::new("extras");
        let table = Requirements {
            rows: &[
                Requirement {
                    source: SRC,
                    dataset: LISTINGS,
                    need: Need::Required,
                },
                Requirement {
                    source: SRC,
                    dataset: EXTRAS,
                    need: Need::Optional,
                },
            ],
            incomplete: Incomplete::Refuse,
        };
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), DATE, STARTED);
        sink.land(SRC, LISTINGS, "https://up/l", 200, PartFormat::Json, b"{}")
            .unwrap();
        sink.finalize(None).unwrap();

        let plan = choose(&zone(tmp.path()), DATE, &table).unwrap();
        assert_eq!(plan.chosen.len(), 1, "the optional dataset landed nothing");
        assert_eq!(
            plan.chosen[0].what(DATE),
            format!("other-site/listings {DATE}")
        );
        // The kit's own table finds nothing for this site's date.
        assert!(choose(&zone(tmp.path()), DATE, &TCGCSV).is_err());
    }

    #[test]
    fn partial_reads_the_newest_incomplete_run_and_refuse_does_not() {
        const SRC: Source = Source::new("other-site");
        const LISTINGS: Dataset = Dataset::new("listings");
        let rows = [Requirement {
            source: SRC,
            dataset: LISTINGS,
            need: Need::Required,
        }];
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), DATE, STARTED);
        sink.land(SRC, LISTINGS, "https://up/l", 200, PartFormat::Json, b"{}")
            .unwrap();
        sink.finalize(Some("http: 503")).unwrap();

        let refuse = Requirements {
            rows: &rows,
            incomplete: Incomplete::Refuse,
        };
        let partial = Requirements {
            rows: &rows,
            incomplete: Incomplete::Partial,
        };
        assert!(choose(&zone(tmp.path()), DATE, &refuse).is_err());
        let plan = choose(&zone(tmp.path()), DATE, &partial).unwrap();
        assert_eq!(plan.chosen[0].run.run_id, sink.run_id());
    }

    /// The kit's shipped table covers every dataset the kit names.
    #[test]
    fn the_shipped_table_looks_for_every_tcgcsv_dataset() {
        let datasets: Vec<Dataset> = TCGCSV.rows.iter().map(|r| r.dataset).collect();
        assert_eq!(datasets, Dataset::ALL);
    }
}
