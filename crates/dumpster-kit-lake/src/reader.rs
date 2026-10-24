//! Reading the raw landing zone — the other half of [`crate::sink`].
//!
//! ```text
//! raw/source=<source>/dataset=<dataset>/ingest_date=YYYY-MM-DD/run=<ULID>/
//!     part-NNNN.<ext>.zst
//!     _manifest.json
//! ```
//!
//! **Nothing here parses a payload.** It lists runs, reads manifests, and
//! hands back the bytes a part held — checked against the length and SHA-256
//! the manifest recorded, because verifying the landing zone *offline* is the
//! entire reason those fields exist. What the bytes mean is the caller's
//! business.
//!
//! This is the Rust twin of `lake/src/dumpster_kit_lake/raw.py`, and the
//! duplication is deliberate rather than regrettable. The Python reader feeds
//! the Iceberg jobs; this one feeds the offline `shared.sqlite` derive, which
//! is Rust because the derivation it drives is Rust and moving that logic to
//! another language is the one thing the epic forbids. The two agree on the
//! key layout, the manifest shape and — the part that is a decision rather
//! than a format — [which run a rebuild reads](select_run). If those ever
//! drift, two jobs will disagree about what a date's data *is*, so both
//! spell the rule out rather than inferring it.

use crate::error::{LakeError, Result};
use crate::keys::{Dataset, Source};
use crate::manifest::{Manifest, PartRecord};
use crate::store::ObjectSource;

/// The manifest's file name, uncompressed and sitting beside its parts.
pub const MANIFEST_NAME: &str = "_manifest.json";

/// One `run=<ULID>` prefix and the manifest that describes it.
#[derive(Debug, Clone)]
pub struct Run {
    /// The run's ULID.
    pub run_id: String,
    /// The prefix every object of this run shares, ending in `/`.
    pub prefix: String,
    /// The manifest as landed. A run whose manifest is missing gets a
    /// synthesised one carrying `complete: false` and an error saying so —
    /// the process died before it could say what it had, and pretending the
    /// prefix is not there would be the same lie the manifest prevents.
    pub manifest: Manifest,
}

impl Run {
    /// A one-line summary, for a refusal that has to name what it found.
    pub fn describe(&self) -> String {
        let state = if self.manifest.complete {
            "complete".to_string()
        } else {
            format!(
                "INCOMPLETE ({})",
                self.manifest.error.as_deref().unwrap_or("no reason given")
            )
        };
        format!(
            "run={} {} part(s), {state}",
            self.run_id,
            self.manifest.parts.len()
        )
    }
}

/// The prefix holding every run of one `(source, dataset, ingest_date)`.
pub fn dataset_prefix(source: Source, dataset: Dataset, ingest_date: &str) -> String {
    format!(
        "raw/source={source}/dataset={dataset}/ingest_date={ingest_date}/",
        source = source.as_str(),
        dataset = dataset.as_str(),
    )
}

/// A landing zone, opened for reading.
pub struct RawZone {
    source: Box<dyn ObjectSource>,
}

impl RawZone {
    /// Read through `source`.
    pub fn new(source: Box<dyn ObjectSource>) -> Self {
        Self { source }
    }

    /// A short description of where this reads from, for progress output.
    pub fn describe(&self) -> String {
        self.source.describe()
    }

    /// Every run landed for one `(source, dataset, date)`, oldest first —
    /// ULIDs sort that way, which is the whole reason the run id is one.
    pub fn runs(&self, source: Source, dataset: Dataset, ingest_date: &str) -> Result<Vec<Run>> {
        let base = dataset_prefix(source, dataset, ingest_date);
        let mut names = self.source.child_dirs(&base)?;
        names.sort();
        names
            .iter()
            .map(|name| {
                let run_id = name.strip_prefix("run=").unwrap_or(name).to_string();
                let prefix = format!("{base}{name}/");
                let manifest = match self.source.get(&format!("{prefix}{MANIFEST_NAME}")) {
                    Ok(bytes) => serde_json::from_slice(&bytes)?,
                    Err(_) => {
                        let mut m = Manifest::new(source, dataset, ingest_date, &run_id, "");
                        m.error = Some("no _manifest.json — the run never finalized".to_string());
                        m
                    }
                };
                Ok(Run {
                    run_id,
                    prefix,
                    manifest,
                })
            })
            .collect()
    }

    /// The bytes a part held, decompressed and checked against the manifest.
    ///
    /// Both the length and the digest are the *uncompressed* ones, which is
    /// what the writer recorded. A mismatch is fatal: a lake that hands back
    /// something other than what it says it stored is worse than one that is
    /// simply empty, because everything downstream would believe it.
    pub fn payload(&self, part: &PartRecord) -> Result<Vec<u8>> {
        let stored = self.source.get(&part.key)?;
        let body = if part.key.ends_with(".zst") {
            zstd::decode_all(&stored[..])?
        } else {
            stored
        };
        if body.len() as u64 != part.bytes {
            return Err(LakeError::Raw(format!(
                "{}: the manifest says {} byte(s), the object holds {}",
                part.key,
                part.bytes,
                body.len()
            )));
        }
        let digest = crate::sink::sha256_hex(&body);
        if digest != part.sha256 {
            return Err(LakeError::Raw(format!(
                "{}: the manifest says sha256 {}, the object hashes to {digest}",
                part.key, part.sha256
            )));
        }
        Ok(body)
    }
}

/// Which of a date's runs a rebuild should read.
///
/// The landing zone can hold several runs for one date, because a retry after
/// a partial failure lands *beside* the first attempt rather than on it. That
/// is the point of `run=<ULID>` — and it makes "rebuild this date" a question
/// with more than one answer. The answer, identical to `raw.py::select_runs`:
///
/// * **The newest complete run wins, alone.** `complete: true` means every
///   fetch that prefix was going to get arrived, so it is by definition all
///   of that date's data. Reading an earlier run as well could only add
///   staler copies of the same bytes.
/// * **With no complete run, refuse.** Nothing in the landing zone can say
///   whether two incomplete runs *together* cover the day — the writer never
///   learns how many parts a dataset was owed. Quietly stitching them would
///   produce a catalog that looks like a day and is not one, which is the
///   exact failure `complete` exists to prevent.
///
/// There is deliberately no `--allow-incomplete` here, unlike the prices
/// build. That job writes one partition of one table and says in the snapshot
/// that the day is partial; this one writes the whole catalog the app serves,
/// where "smaller than it should be" reads as *cards that do not exist*.
pub fn select_run(runs: &[Run], what: &str) -> Result<Run> {
    if let Some(run) = runs.iter().rev().find(|r| r.manifest.complete) {
        return Ok(run.clone());
    }
    if runs.is_empty() {
        return Err(LakeError::Raw(format!(
            "no runs landed for {what}.\n\
             The derive builds the partition it was ASKED for and never falls back to the \
             newest available one — yesterday's raw quietly deriving today's catalog is the \
             failure this refusal exists to prevent."
        )));
    }
    let detail = runs
        .iter()
        .map(Run::describe)
        .collect::<Vec<_>>()
        .join("; ");
    Err(LakeError::Raw(format!(
        "no complete run for {what} — {detail}.\n\
         An incomplete run's parts are real bytes, but nothing in the landing zone can say \
         whether they add up to the whole day: the writer never learns how many parts a \
         dataset was owed. Re-run the fetch for that date."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::PartFormat;
    use crate::sink::RawLanding;
    use crate::store::DirStore;

    const STARTED: &str = "2026-08-11T04:51:02Z";

    /// Land two datasets through the real writer, then read them back through
    /// the real reader. The two halves share a format and nothing else, so a
    /// round trip is the only thing that proves they still agree.
    fn landed(root: &std::path::Path, ingest_date: &str, fail: bool) -> String {
        let sink = RawLanding::new(Box::new(DirStore::new(root)), ingest_date, STARTED);
        sink.land(
            Source::TCGCSV,
            Dataset::GROUPS,
            "https://tcgcsv.com/tcgplayer/3/groups",
            200,
            PartFormat::Json,
            br#"{"results":[{"groupId":1}]}"#,
        )
        .unwrap();
        sink.finalize(if fail { Some("http: 503") } else { None })
            .unwrap();
        sink.run_id().to_string()
    }

    fn zone(root: &std::path::Path) -> RawZone {
        RawZone::new(Box::new(DirStore::new(root)))
    }

    #[test]
    fn reads_back_exactly_what_was_landed() {
        let tmp = tempfile::tempdir().unwrap();
        let run = landed(tmp.path(), "2026-08-11", false);
        let zone = zone(tmp.path());

        let runs = zone
            .runs(Source::TCGCSV, Dataset::GROUPS, "2026-08-11")
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, run);
        assert_eq!(runs[0].manifest.started_at, STARTED);

        let chosen = select_run(&runs, "tcgcsv/groups 2026-08-11").unwrap();
        let part = &chosen.manifest.parts[0];
        assert_eq!(part.url, "https://tcgcsv.com/tcgplayer/3/groups");
        assert_eq!(
            zone.payload(part).unwrap(),
            br#"{"results":[{"groupId":1}]}"#
        );
    }

    #[test]
    fn a_date_that_landed_nothing_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let zone = zone(tmp.path());
        assert!(
            zone.runs(Source::TCGCSV, Dataset::GROUPS, "1999-01-09")
                .unwrap()
                .is_empty()
        );
    }

    /// The newest COMPLETE run, not the newest run. A retry that died after a
    /// good run must not win just by being later.
    #[test]
    fn the_newest_complete_run_wins_over_a_newer_failed_one() {
        let tmp = tempfile::tempdir().unwrap();
        let good = landed(tmp.path(), "2026-08-11", false);
        let bad = landed(tmp.path(), "2026-08-11", true);
        assert_ne!(good, bad);

        let runs = zone(tmp.path())
            .runs(Source::TCGCSV, Dataset::GROUPS, "2026-08-11")
            .unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(select_run(&runs, "x").unwrap().run_id, good);
    }

    #[test]
    fn a_date_with_no_complete_run_refuses_and_names_what_it_found() {
        let tmp = tempfile::tempdir().unwrap();
        let run = landed(tmp.path(), "2026-08-11", true);
        let runs = zone(tmp.path())
            .runs(Source::TCGCSV, Dataset::GROUPS, "2026-08-11")
            .unwrap();
        let err = select_run(&runs, "tcgcsv/groups 2026-08-11")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no complete run"), "{err}");
        assert!(err.contains(&run), "{err}");

        // And a date with nothing at all refuses differently — "landed
        // nothing" and "landed badly" are different operator problems.
        let none = select_run(&[], "tcgcsv/groups 1999-01-09")
            .unwrap_err()
            .to_string();
        assert!(none.contains("no runs landed"), "{none}");
        assert!(none.contains("1999-01-09"), "{none}");
    }

    /// The manifest's whole job is to be checkable without re-fetching. A part
    /// whose bytes no longer hash to what was recorded is a corrupt lake, and
    /// the derive must stop rather than build a catalog out of it.
    #[test]
    fn a_tampered_part_is_refused_by_its_own_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        landed(tmp.path(), "2026-08-11", false);
        let zone = zone(tmp.path());
        let runs = zone
            .runs(Source::TCGCSV, Dataset::GROUPS, "2026-08-11")
            .unwrap();
        let part = runs[0].manifest.parts[0].clone();

        // Same shape, different bytes: still valid zstd, still valid JSON.
        std::fs::write(
            tmp.path().join(&part.key),
            zstd::encode_all(&br#"{"results":[{"groupId":9}]}"#[..], 1).unwrap(),
        )
        .unwrap();

        let err = zone.payload(&part).unwrap_err().to_string();
        assert!(err.contains("sha256"), "{err}");
        assert!(err.contains(&part.key), "{err}");
    }

    /// A run that died before writing its manifest is still reported, as
    /// incomplete. An unreadable prefix that simply vanished from the listing
    /// would let a date look emptier than it is.
    #[test]
    fn a_run_with_no_manifest_reads_as_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let run = landed(tmp.path(), "2026-08-11", false);
        std::fs::remove_file(tmp.path().join(format!(
            "raw/source=tcgcsv/dataset=groups/ingest_date=2026-08-11/run={run}/{MANIFEST_NAME}"
        )))
        .unwrap();

        let runs = zone(tmp.path())
            .runs(Source::TCGCSV, Dataset::GROUPS, "2026-08-11")
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].manifest.complete);
        assert!(runs[0].describe().contains("never finalized"));
    }
}
