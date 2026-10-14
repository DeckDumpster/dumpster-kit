//! `dumpster-kit-derive` — the parts of "derive a catalog from `raw/`" that no site gets to
//! vary: the one clock, and the bracket that makes a landing run's manifests exist whichever
//! way it ends.
//!
//! > Only lakehouse code reads `raw/`. The shared and tenant databases are derived from that,
//! > so whatever produces them is **also** lakehouse code.
//!
//! That rule is how the offline and online processes decouple onto different machines. A site
//! therefore has two entry points of its own — a `land` that fetches every upstream into
//! `raw/` and derives nothing, and a `derive` that replays one `raw/` partition into its
//! catalog — and what they share is here:
//!
//! | piece | what it settles |
//! | --- | --- |
//! | [`DeriveClock`] | the instant the inputs were fetched: read once by the landing side, recorded in every manifest, read back by the deriving side, so "row-identical" means identical |
//! | [`finalize_landing`] | the run's manifests are written whichever way acquisition ended; a run that dies partway leaves a manifest that says so, not a short prefix that reads as whole |
//!
//! **What is the site's:** which upstreams it fetches, the catalog it derives into, the
//! mapping from a landed payload to a catalog row. None of that is game-agnostic, and none of
//! it is here.

pub mod clock;

use std::sync::Arc;

use dumpster_kit_lake::RawLanding;

pub use clock::DeriveClock;

/// Write the run's manifests and report what landed.
///
/// `error` is the acquisition phase's failure, if it had one; every manifest then records that
/// the run stopped early. A manifest that cannot be written is an error in its own right — an
/// unwritten manifest is indistinguishable from a run that never got that far — but it must
/// not mask the fetch failure that is the more useful diagnosis.
pub fn finalize_landing(
    landing: &Arc<RawLanding>,
    error: Option<&anyhow::Error>,
) -> anyhow::Result<()> {
    let text = error.map(|e| format!("{e:#}"));
    let outcome = landing.finalize(text.as_deref());

    for manifest in landing.manifests() {
        println!(
            "  raw: {}/{} — {} part(s), {} byte(s), {}",
            manifest.source,
            manifest.dataset,
            manifest.parts.len(),
            manifest.total_bytes(),
            if manifest.complete {
                "complete".to_string()
            } else {
                format!("INCOMPLETE ({} failure(s))", manifest.failures.len())
            }
        );
    }

    match outcome {
        Ok(()) => Ok(()),
        // The acquisition error is the one worth propagating; this one still has to be said
        // out loud rather than dropped.
        Err(e) if error.is_some() => {
            eprintln!("WARN: could not write the raw landing manifests: {e}");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumpster_kit_lake::{DirStore, PartFormat, RawZone, Source};

    const STARTED: &str = "2026-08-11T04:51:02+00:00";
    const DATE: &str = "2026-08-11";

    fn landing(root: &std::path::Path) -> Arc<RawLanding> {
        Arc::new(RawLanding::new(
            Box::new(DirStore::new(root)),
            DATE,
            STARTED,
        ))
    }

    fn land_one(l: &RawLanding) {
        l.land(
            Source::TCGCSV,
            dumpster_kit_lake::Dataset::GROUPS,
            "https://up/79/groups",
            200,
            PartFormat::Json,
            b"{}",
        )
        .unwrap();
    }

    fn complete_runs(root: &std::path::Path) -> usize {
        RawZone::new(Box::new(DirStore::new(root)))
            .runs(Source::TCGCSV, dumpster_kit_lake::Dataset::GROUPS, DATE)
            .unwrap()
            .iter()
            .filter(|r| r.manifest.complete)
            .count()
    }

    #[test]
    fn a_run_that_acquired_cleanly_is_finalised_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let l = landing(tmp.path());
        land_one(&l);
        finalize_landing(&l, None).unwrap();
        assert_eq!(complete_runs(tmp.path()), 1);
    }

    /// The point of the bracket: an acquisition that died still leaves a manifest, and it says
    /// the run is incomplete, so a reader refuses it rather than reading a short prefix.
    #[test]
    fn a_run_that_died_partway_leaves_an_incomplete_manifest_not_none() {
        let tmp = tempfile::tempdir().unwrap();
        let l = landing(tmp.path());
        land_one(&l);
        let err = anyhow::anyhow!("upstream hung up after group 3");
        finalize_landing(&l, Some(&err)).unwrap();

        let runs = RawZone::new(Box::new(DirStore::new(tmp.path())))
            .runs(Source::TCGCSV, dumpster_kit_lake::Dataset::GROUPS, DATE)
            .unwrap();
        assert_eq!(runs.len(), 1, "the dying run still wrote its manifest");
        assert!(!runs[0].manifest.complete);
        assert_eq!(complete_runs(tmp.path()), 0);
    }
}
