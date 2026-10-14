//! Answering an upstream request out of `raw/`.
//!
//! A raw partition is a **URL → bytes** map and always was: the manifest
//! records the URL of every part beside its key and digest, precisely so the
//! landing zone can be read back without a parser. So replay is a lookup, not
//! a reimplementation — [`RawReplay`] resolves the URL a client was about to
//! request and hands back the body that URL returned when it was landed. The
//! derivation above it does not know, and must not know, which happened:
//! comparing two runs of one derivation is what makes "row-identical" a claim
//! about *provenance*. Comparing two implementations would only ever be a
//! claim about the second one.
//!
//! ## A miss is fatal
//!
//! A URL the partition has no record of means **raw coverage has regressed** —
//! the derivation grew an input the landing zone does not capture, or an
//! upstream's origin moved. That is a refusal, full stop.
//!
//! A URL the partition has a *failure* record for is a different fact and gets
//! a different sentence (pd-llbq). Nothing regressed there: that request was
//! made, it exhausted its retries, and the landing zone wrote down what it
//! answered. There is nothing to replay because there was nothing to land, and
//! the derivation fails at exactly the request the fetch failed at — which is
//! what makes a partial night replay to the catalog the online refresh built
//! rather than to a different one. Which of the two it is decides whether the
//! job exits 1 or 2, so telling an operator "re-land the date" for a night
//! upstream was down would be advice that cannot work.
//!
//! Item 2 of the epic shipped with a temporary fallback for that case: reach
//! the live upstream, say so loudly, and let the run finish. It was there to
//! keep the offline derive usable while row-identity was still unproven, and
//! item 4 deleted it once pd-vves proved it against the real bucket. What is
//! left is the behaviour `--no-upstream-fallback` used to select, now the only
//! behaviour there is — and it is structural rather than a policy this module
//! chooses: [`ReplaySource::missing`] returns the error itself, so a replaying
//! wire has no path to the network for this module to decline to take.
//!
//! The reason it could not stay is that a fallback makes the landing zone
//! decorative. A derive that reaches upstream produces a correct catalog with a
//! LINEAGE that is not reproducible — every gate passes, every row looks right,
//! and we would find out on the day an upstream is down, which is the day the
//! lake was bought for. Loudness was a mitigation for that, not a fix: it
//! depends on somebody reading the log.
//!
//! The one phase that still fetches on the offline path is set-symbol
//! normalisation, and it does not come through here at all —
//! `symbols::normalize_all_symbols` has no [`Wire`](dumpstar_ingest::landing::Wire)
//! in its signature and builds its own client, because images are deliberately
//! outside `raw/`. See `dumpstar-derive`'s crate docs, and
//! `row_identical.rs::a_cold_derive_fetches_set_symbols_live_and_is_not_refused_for_it`
//! for the gate that holds that apart from this rule.

use std::collections::HashMap;

use dumpster_kit_lake::{PartRecord, RawZone};

use crate::partition::Chosen;

/// A URL-keyed view of one date's chosen runs.
pub struct RawReplay {
    zone: RawZone,
    /// URL → the part that URL's response landed as. Built once, at
    /// construction, from the manifests of the runs [`crate::partition::choose`]
    /// selected; the payload itself is read (and verified against the
    /// manifest's SHA-256) only when it is asked for.
    index: HashMap<String, PartRecord>,
    /// URL → what the landing zone recorded when that URL's fetch failed.
    ///
    /// Built from the same manifests as `index`, from their `failures` rather
    /// than their `parts`. It answers nothing — a failed fetch has no bytes —
    /// but it is what lets [`RawReplay::missing`] say "upstream was down when
    /// this was fetched" instead of "the landing zone stopped covering this".
    failed: HashMap<String, String>,
}

impl RawReplay {
    /// Index `chosen`'s parts by URL.
    ///
    /// Two parts claiming one URL is a corrupt partition rather than a
    /// preference to resolve: within a single run a URL is fetched once, and
    /// [`choose`](crate::partition::choose) picks exactly one run per dataset,
    /// so a duplicate means the manifest disagrees with itself.
    pub fn new(zone: RawZone, chosen: &[Chosen]) -> anyhow::Result<Self> {
        let mut index: HashMap<String, PartRecord> = HashMap::new();
        let mut failed: HashMap<String, String> = HashMap::new();
        for c in chosen {
            for failure in &c.run.manifest.failures {
                failed.insert(
                    failure.url.clone(),
                    match failure.status {
                        Some(status) => format!("HTTP {status}: {}", failure.error),
                        None => failure.error.clone(),
                    },
                );
            }
            for part in &c.run.manifest.parts {
                if let Some(prior) = index.insert(part.url.clone(), part.clone())
                    && prior.key != part.key
                {
                    anyhow::bail!(
                        "run={} records two different parts for one URL: {} is both {} and {}.\n\
                         A URL is fetched once per run, so this manifest disagrees with itself \
                         and nothing here can decide which body that URL actually returned.",
                        c.run.run_id,
                        part.url,
                        prior.key,
                        part.key
                    );
                }
            }
        }
        Ok(Self {
            zone,
            index,
            failed,
        })
    }

    /// How many distinct URLs this partition can answer.
    pub fn urls(&self) -> usize {
        self.index.len()
    }
}

/// A URL the partition cannot answer, with the diagnosis a site should show.
///
/// There is deliberately no variant that reaches the network: a miss is the end of the run.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    /// The partition has no body for this URL (see [`RawReplay::missing`]).
    #[error("{0}")]
    Miss(String),
    /// The landing zone failed, or returned bytes that do not match its manifest.
    #[error(transparent)]
    Lake(#[from] dumpster_kit_lake::LakeError),
}

impl RawReplay {
    /// The body this URL returned when it was landed, or `None` when the partition has no
    /// record of the URL at all.
    pub fn body(&self, url: &str) -> Result<Option<Vec<u8>>, ReplayError> {
        match self.index.get(url) {
            // `payload` verifies the bytes against the length and SHA-256 the
            // manifest recorded. A lake that hands back something other than
            // what it says it stored is worse than an empty one, because
            // everything downstream would believe it.
            Some(part) => Ok(Some(self.zone.payload(part)?)),
            None => Ok(None),
        }
    }

    /// The error for a URL [`body`](Self::body) had no record of.
    ///
    /// It returns the failure rather than a `Result`, because there is no success to report:
    /// a miss means the landing zone no longer covers the derivation's inputs, and the run
    /// stops. A site implementing its own replay seam adapts [`body`](Self::body) and this.
    pub fn missing(&self, url: &str) -> ReplayError {
        // The partition WROTE DOWN that this fetch failed. Nothing regressed,
        // and re-landing the date cannot help: that response never existed.
        if let Some(why) = self.failed.get(url) {
            return ReplayError::Miss(format!(
                "{url} was fetched when this partition was landed and the fetch FAILED \
                 ({why}), so there is nothing to replay.\n\
                 This is a PARTIAL night rather than a gap in raw/: the derivation stops at the \
                 same request the original fetch stopped at, which is what makes it reproduce \
                 that night's catalog rather than a different one. Re-landing the date cannot \
                 help — that response never existed."
            ));
        }
        ReplayError::Miss(format!(
            "raw/ has no record of {url}.\n\
             The landing zone no longer covers this derivation's inputs: either an \
             endpoint was added without landing it, or the upstream's origin moved. \
             Re-land the date (the site's data refresh) and derive again."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumpster_kit_lake::{Dataset, DirStore, PartFormat, RawLanding, Source};

    const STARTED: &str = "2026-08-11T04:51:02+00:00";
    const DATE: &str = "2026-08-11";
    const URL: &str = "https://tcgcsv.com/tcgplayer/3/groups";
    const BODY: &[u8] = br#"{"results":[{"groupId":1}]}"#;

    fn landed(root: &std::path::Path) -> RawZone {
        let sink = RawLanding::new(Box::new(DirStore::new(root)), DATE, STARTED);
        sink.land(
            Source::Tcgcsv,
            Dataset::Groups,
            URL,
            200,
            PartFormat::Json,
            BODY,
        )
        .unwrap();
        sink.finalize(None).unwrap();
        RawZone::new(Box::new(DirStore::new(root)))
    }

    /// The one run this fixture lands, as a plan. `partition::choose` is
    /// deliberately not used here — it refuses a date missing three of the
    /// four required datasets, which is its own test's business, not this
    /// module's.
    fn replay(root: &std::path::Path) -> RawReplay {
        let zone = landed(root);
        let runs = zone.runs(Source::Tcgcsv, Dataset::Groups, DATE).unwrap();
        let chosen = vec![Chosen {
            source: Source::Tcgcsv,
            dataset: Dataset::Groups,
            run: dumpster_kit_lake::select_run(&runs, "tcgcsv/groups").unwrap(),
        }];
        RawReplay::new(zone, &chosen).unwrap()
    }

    #[test]
    fn a_landed_url_replays_the_bytes_that_were_landed() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path());
        assert_eq!(replay.urls(), 1);
        assert_eq!(replay.body(URL).unwrap().as_deref(), Some(BODY));
    }

    /// A URL the partition has no record of stops the run, and the refusal
    /// says what regressed and what to do about it. There is no second
    /// behaviour to select any more: the fallback that used to make this a
    /// warning is gone (item 4).
    #[test]
    fn a_miss_refuses_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path());
        assert_eq!(
            replay
                .body("https://tcgcsv.com/tcgplayer/3/9/prices")
                .unwrap(),
            None,
            "a URL outside the partition has no body to hand back"
        );
        let err = replay.missing("https://up/3/9/prices").to_string();
        assert!(
            err.contains("raw/ has no record of https://up/3/9/prices"),
            "{err}"
        );
        assert!(err.contains("data refresh"), "{err}");
    }

    /// A URL the partition recorded a FAILURE for is not a gap in raw/, and
    /// the two must not read alike: one is upstream having a bad night and the
    /// job exits 2, the other is coverage regressing and the job exits 1
    /// (pd-llbq). "Re-land the date" is advice that cannot work for the first.
    #[test]
    fn a_url_whose_fetch_failed_says_so_rather_than_blaming_coverage() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), DATE, STARTED);
        sink.land(
            Source::Tcgcsv,
            Dataset::Groups,
            URL,
            200,
            PartFormat::Json,
            BODY,
        )
        .unwrap();
        sink.record_failure(
            Source::Tcgcsv,
            Dataset::Prices,
            "https://up/3/1/prices",
            Some(502),
            "http 502 after 4 attempts",
        )
        .unwrap();
        sink.finalize(None).unwrap();

        let zone = RawZone::new(Box::new(DirStore::new(tmp.path())));
        let runs = zone.runs(Source::Tcgcsv, Dataset::Prices, DATE).unwrap();
        let replay = RawReplay::new(
            zone,
            &[Chosen {
                source: Source::Tcgcsv,
                dataset: Dataset::Prices,
                // The run is INCOMPLETE, so `select_run` is not what picks it.
                run: runs
                    .last()
                    .expect("the failed run landed a manifest")
                    .clone(),
            }],
        )
        .unwrap();

        assert_eq!(replay.urls(), 0, "a failed fetch landed no part");
        let err = replay.missing("https://up/3/1/prices").to_string();
        assert!(err.contains("the fetch FAILED"), "{err}");
        assert!(err.contains("502"), "{err}");
        assert!(err.contains("PARTIAL"), "{err}");
        assert!(!err.contains("no record of"), "{err}");

        // …and a URL that really is outside the partition still gets the
        // other sentence. One message for both would be the bug.
        let gap = replay.missing("https://up/3/9/prices").to_string();
        assert!(gap.contains("raw/ has no record of"), "{gap}");
        // The advice the other sentence gives, and the one it cannot: landing
        // is not a flag since pd-lunn, so `dumpstar data refresh` IS the re-land.
        assert!(gap.contains("data refresh"), "{gap}");
    }

    /// The manifest's digest is not decoration. A part whose bytes changed
    /// under it must stop the derive, not feed it a plausible catalog.
    #[test]
    fn a_tampered_payload_fails_the_replay() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path());
        // The SAME LENGTH, different bytes — otherwise the length check fires
        // first and the digest is never consulted. A corruption that changes
        // a value without changing a size is the one only the hash can catch.
        let key = &replay.index[URL].key;
        std::fs::write(
            tmp.path().join(key),
            zstd::encode_all(&br#"{"results":[{"groupId":9}]}"#[..], 1).unwrap(),
        )
        .unwrap();

        let err = replay.body(URL).unwrap_err().to_string();
        assert!(err.contains("sha256"), "{err}");
    }

    /// Two parts for one URL is a manifest that disagrees with itself. There
    /// is no right answer to pick, so there is no picking.
    #[test]
    fn one_url_cannot_have_two_bodies() {
        let tmp = tempfile::tempdir().unwrap();
        let zone = landed(tmp.path());
        let runs = zone.runs(Source::Tcgcsv, Dataset::Groups, DATE).unwrap();
        let mut run = dumpster_kit_lake::select_run(&runs, "tcgcsv/groups").unwrap();
        let mut twin = run.manifest.parts[0].clone();
        twin.key = format!("{}-twin", twin.key);
        run.manifest.parts.push(twin);

        let err = match RawReplay::new(
            zone,
            &[Chosen {
                source: Source::Tcgcsv,
                dataset: Dataset::Groups,
                run,
            }],
        ) {
            Ok(_) => panic!("a manifest with two bodies for one URL must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("two different parts for one URL"), "{err}");
    }
}
