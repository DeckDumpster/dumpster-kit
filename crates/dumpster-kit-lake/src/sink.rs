//! [`RawLanding`] — one refresh's worth of landed bytes.
//!
//! A sink is created once per the site's `data refresh` / the site's `setup`
//! invocation, handed to every upstream client, and finalized when the fetch
//! phase ends — whether it ended by finishing or by failing.
//!
//! It holds one ULID for the whole invocation, so every prefix a run touches
//! carries the same `run=`, and a manifest per `(source, dataset)`, so the
//! parts of one endpoint are described by the file sitting next to them.

use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::keys::{Dataset, PartFormat, Source};
use crate::manifest::{FailureRecord, Manifest, PartRecord};
use crate::store::ObjectStore;

/// zstd level for landed payloads. Archival storage read rarely and written
/// once a night: worth more than the streaming default, not worth the long
/// tail above it. At the measured ~33 MB/day of JSON this is a couple of
/// seconds of CPU for roughly an 8x reduction.
///
/// Every part is compressed, including an already-gzipped
/// tarball, where zstd finds nothing and earns nothing. That is deliberate:
/// it buys whoever writes the table-build job (`pd-1ojt`) a single
/// decompression path over `raw/` instead of a per-extension branch, and the
/// cost is one wasted second on the site's `setup`, which is not the nightly
/// path. Do not confuse this with the site's server's decision to skip
/// compressing `image/*` — that one is on a hot request path and pays per
/// response.
const ZSTD_LEVEL: i32 = 10;

/// The landing zone for one invocation.
pub struct RawLanding {
    store: Box<dyn ObjectStore>,
    run: String,
    ingest_date: String,
    started_at: String,
    state: Mutex<Vec<DatasetState>>,
}

struct DatasetState {
    source: Source,
    dataset: Dataset,
    manifest: Manifest,
}

impl RawLanding {
    /// Open a landing zone writing to `store`, stamping everything with a
    /// fresh run ULID.
    ///
    /// `ingest_date` is the `YYYY-MM-DD` partition and `started_at` is the
    /// run's clock, RFC 3339. Both are passed in rather than read from the
    /// clock here, and for the same reason: a backfill lands under the date
    /// it is reconstructing, and the *deriving* side has to be able to
    /// reproduce the timestamps the importing side stamped into its rows.
    /// See [`Manifest::started_at`](crate::Manifest::started_at).
    pub fn new(store: Box<dyn ObjectStore>, ingest_date: &str, started_at: &str) -> Self {
        Self::with_run(
            store,
            ingest_date,
            started_at,
            &ulid::Ulid::generate().to_string(),
        )
    }

    /// Open a landing zone with an explicit run id. Tests use this to make
    /// two runs collide on purpose and prove that they cannot.
    pub fn with_run(
        store: Box<dyn ObjectStore>,
        ingest_date: &str,
        started_at: &str,
        run: &str,
    ) -> Self {
        Self {
            store,
            run: run.to_string(),
            ingest_date: ingest_date.to_string(),
            started_at: started_at.to_string(),
            state: Mutex::new(Vec::new()),
        }
    }

    /// This invocation's run ULID.
    pub fn run_id(&self) -> &str {
        &self.run
    }

    /// The `ingest_date=` partition everything lands under.
    pub fn ingest_date(&self) -> &str {
        &self.ingest_date
    }

    /// The run's clock, as every manifest of this run records it.
    pub fn started_at(&self) -> &str {
        &self.started_at
    }

    /// A short description of the destination, for progress output.
    pub fn describe(&self) -> String {
        format!("{} run={}", self.store.describe(), self.run)
    }

    /// Land one upstream payload exactly as it arrived.
    ///
    /// `body` is the response bytes before any parsing. They are hashed,
    /// compressed and stored (a [`PartFormat::Bin`] part is stored as the raw
    /// bytes, under its content hash, and landing the same bytes again adds
    /// nothing), and a [`PartRecord`] describing them is added
    /// to this `(source, dataset)`'s manifest.
    pub fn land(
        &self,
        source: Source,
        dataset: Dataset,
        url: &str,
        status: u16,
        format: PartFormat,
        body: &[u8],
    ) -> Result<()> {
        let mut state = self.state.lock().expect("landing state poisoned");
        let entry = self.entry(&mut state, source, dataset);
        let part = entry.manifest.parts.len() as u32;

        let sha256 = sha256_hex(body);
        let key = if format.is_compressed() {
            crate::keys::part_key(source, dataset, &self.ingest_date, &self.run, part, format)
        } else {
            crate::keys::blob_key(
                source,
                dataset,
                &self.ingest_date,
                &self.run,
                &sha256,
                format,
            )
        };
        if !format.is_compressed() && entry.manifest.parts.iter().any(|p| p.key == key) {
            return Ok(());
        }
        let record = PartRecord {
            key: key.clone(),
            url: url.to_string(),
            status,
            bytes: body.len() as u64,
            sha256,
        };

        // Compress and store before recording: a part in the manifest that
        // is not in the bucket would be the one lie this file must never
        // tell.
        let stored = if format.is_compressed() {
            zstd::encode_all(body, ZSTD_LEVEL)?
        } else {
            body.to_vec()
        };
        self.store.put(&key, stored)?;
        entry.manifest.parts.push(record);
        Ok(())
    }

    /// Record a fetch that failed, and flush this dataset's manifest at
    /// once.
    ///
    /// Flushing immediately rather than waiting for [`finalize`] is
    /// deliberate: a failing upstream is exactly when the process is most
    /// likely to be killed before it tidies up, and the evidence of a short
    /// run is worth more than the PUT it costs.
    ///
    /// [`finalize`]: RawLanding::finalize
    pub fn record_failure(
        &self,
        source: Source,
        dataset: Dataset,
        url: &str,
        status: Option<u16>,
        error: &str,
    ) -> Result<()> {
        let mut state = self.state.lock().expect("landing state poisoned");
        let entry = self.entry(&mut state, source, dataset);
        entry.manifest.failures.push(FailureRecord {
            url: url.to_string(),
            status,
            error: error.to_string(),
        });
        entry.manifest.error = Some(error.to_string());
        let manifest = entry.manifest.clone();
        let key = crate::keys::manifest_key(source, dataset, &self.ingest_date, &self.run);
        drop(state);
        self.store.put(&key, manifest.to_json()?)
    }

    /// Write every manifest and close the run.
    ///
    /// `error` is `Some` when the fetch phase ended by failing; every
    /// manifest then says `complete: false` and carries the text, because a
    /// run that stopped early must not be readable as a whole one.
    ///
    /// That is conservative *across* datasets on purpose — a dataset whose
    /// own fetches all succeeded is still marked incomplete when some other
    /// dataset failed. It has to be: a run that died at group 200 of 450
    /// leaves a `products` prefix with 200 parts, no failure of its own, and
    /// nothing here knows it was owed 450. Erring the other way would let
    /// exactly that prefix read as whole.
    pub fn finalize(&self, error: Option<&str>) -> Result<()> {
        let mut state = self.state.lock().expect("landing state poisoned");
        let finalized_at = chrono::Utc::now().to_rfc3339();
        let mut written = Vec::new();
        for entry in state.iter_mut() {
            entry.manifest.finalized_at = finalized_at.clone();
            entry.manifest.complete = error.is_none() && entry.manifest.failures.is_empty();
            if let Some(error) = error {
                entry.manifest.error = Some(error.to_string());
            }
            written.push((
                crate::keys::manifest_key(
                    entry.source,
                    entry.dataset,
                    &self.ingest_date,
                    &self.run,
                ),
                entry.manifest.to_json()?,
            ));
        }
        drop(state);

        for (key, body) in written {
            self.store.put(&key, body)?;
        }
        Ok(())
    }

    /// A snapshot of the manifests as they stand, for progress reporting.
    pub fn manifests(&self) -> Vec<Manifest> {
        self.state
            .lock()
            .expect("landing state poisoned")
            .iter()
            .map(|e| e.manifest.clone())
            .collect()
    }

    fn entry<'a>(
        &self,
        state: &'a mut Vec<DatasetState>,
        source: Source,
        dataset: Dataset,
    ) -> &'a mut DatasetState {
        // Linear scan over at most six datasets — a map would be more
        // machinery than the search saves, and this preserves first-touch
        // order for the progress summary.
        if let Some(i) = state
            .iter()
            .position(|e| e.source == source && e.dataset == dataset)
        {
            return &mut state[i];
        }
        state.push(DatasetState {
            source,
            dataset,
            manifest: Manifest::new(
                source,
                dataset,
                &self.ingest_date,
                &self.run,
                &self.started_at,
            ),
        });
        state.last_mut().expect("just pushed")
    }
}

/// Lowercase-hex SHA-256, the form the manifest records.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    const STARTED: &str = "2026-08-11T04:51:02Z";
    use crate::store::DirStore;
    use std::path::Path;

    fn read_manifest(root: &Path, key: &str) -> Manifest {
        serde_json::from_slice(&std::fs::read(root.join(key)).unwrap()).unwrap()
    }

    #[test]
    fn lands_bytes_and_describes_them_in_the_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::with_run(
            Box::new(DirStore::new(tmp.path())),
            "2026-08-11",
            STARTED,
            "01K2CJ1N0000000000000000AA",
        );
        let body = br#"{"results":[1,2,3]}"#;
        sink.land(
            Source::TCGCSV,
            Dataset::GROUPS,
            "https://tcgcsv.com/tcgplayer/3/groups",
            200,
            PartFormat::Json,
            body,
        )
        .unwrap();
        sink.finalize(None).unwrap();

        let key = crate::keys::manifest_key(
            Source::TCGCSV,
            Dataset::GROUPS,
            "2026-08-11",
            "01K2CJ1N0000000000000000AA",
        );
        let manifest = read_manifest(tmp.path(), &key);
        assert!(manifest.complete);
        assert_eq!(manifest.error, None);
        assert_eq!(manifest.parts.len(), 1);
        // The run's clock, recorded so a later derive from these bytes can
        // stamp the same timestamps into the same rows.
        assert_eq!(manifest.started_at, STARTED);

        let part = &manifest.parts[0];
        assert_eq!(part.status, 200);
        assert_eq!(part.bytes, body.len() as u64);
        assert_eq!(part.url, "https://tcgcsv.com/tcgplayer/3/groups");

        // The stored object decompresses back to exactly what was fetched,
        // and the recorded hash is the hash of those bytes.
        let stored = std::fs::read(tmp.path().join(&part.key)).unwrap();
        let round_tripped = zstd::decode_all(&stored[..]).unwrap();
        assert_eq!(round_tripped, body);
        assert_eq!(part.sha256, sha256_hex(&round_tripped));
    }

    /// Every part's recorded SHA-256 must match the bytes actually stored —
    /// the manifest's whole job is to be checkable without re-fetching.
    #[test]
    fn every_recorded_hash_matches_the_stored_object() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), "2026-08-11", STARTED);
        for group in 0..5 {
            sink.land(
                Source::TCGCSV,
                Dataset::PRICES,
                &format!("https://tcgcsv.com/tcgplayer/3/{group}/prices"),
                200,
                PartFormat::Json,
                format!(r#"{{"results":[{group}]}}"#).as_bytes(),
            )
            .unwrap();
        }
        sink.finalize(None).unwrap();

        let manifest = read_manifest(
            tmp.path(),
            &crate::keys::manifest_key(
                Source::TCGCSV,
                Dataset::PRICES,
                "2026-08-11",
                sink.run_id(),
            ),
        );
        assert_eq!(manifest.parts.len(), 5);
        for part in &manifest.parts {
            let raw =
                zstd::decode_all(&std::fs::read(tmp.path().join(&part.key)).unwrap()[..]).unwrap();
            assert_eq!(part.sha256, sha256_hex(&raw), "{}", part.key);
            assert_eq!(part.bytes, raw.len() as u64, "{}", part.key);
        }
    }

    /// The property `run=<ULID>` exists for: a retry after a partial failure
    /// leaves the first attempt's objects untouched.
    #[test]
    fn a_retry_on_the_same_date_never_overwrites_the_first_attempt() {
        let tmp = tempfile::tempdir().unwrap();

        let first = RawLanding::new(Box::new(DirStore::new(tmp.path())), "2026-08-11", STARTED);
        first
            .land(
                Source::TCGCSV,
                Dataset::PRICES,
                "https://tcgcsv.com/tcgplayer/3/1/prices",
                200,
                PartFormat::Json,
                b"first attempt",
            )
            .unwrap();
        first
            .finalize(Some("http: 503 Service Unavailable"))
            .unwrap();

        let second = RawLanding::new(Box::new(DirStore::new(tmp.path())), "2026-08-11", STARTED);
        second
            .land(
                Source::TCGCSV,
                Dataset::PRICES,
                "https://tcgcsv.com/tcgplayer/3/1/prices",
                200,
                PartFormat::Json,
                b"second attempt",
            )
            .unwrap();
        second.finalize(None).unwrap();

        assert_ne!(first.run_id(), second.run_id());

        let manifest_of = |run: &str| {
            read_manifest(
                tmp.path(),
                &crate::keys::manifest_key(Source::TCGCSV, Dataset::PRICES, "2026-08-11", run),
            )
        };
        let a = manifest_of(first.run_id());
        let b = manifest_of(second.run_id());

        // Same part number, same date, different objects — both still there.
        assert_eq!(
            a.parts[0].key.rsplit('/').next(),
            Some("part-0000.json.zst")
        );
        assert_eq!(
            b.parts[0].key.rsplit('/').next(),
            Some("part-0000.json.zst")
        );
        assert_ne!(a.parts[0].key, b.parts[0].key);
        assert_eq!(
            zstd::decode_all(&std::fs::read(tmp.path().join(&a.parts[0].key)).unwrap()[..])
                .unwrap(),
            b"first attempt"
        );
        assert_eq!(
            zstd::decode_all(&std::fs::read(tmp.path().join(&b.parts[0].key)).unwrap()[..])
                .unwrap(),
            b"second attempt"
        );

        // …and the failed run still says it failed.
        assert!(!a.complete);
        assert!(b.complete);
    }

    /// A run that dies partway leaves a manifest that says so, rather than
    /// a short prefix that reads as whole.
    #[test]
    fn a_run_that_stops_early_is_marked_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), "2026-08-11", STARTED);
        sink.land(
            Source::TCGCSV,
            Dataset::PRICES,
            "https://tcgcsv.com/tcgplayer/3/1/prices",
            200,
            PartFormat::Json,
            b"one good page",
        )
        .unwrap();
        sink.record_failure(
            Source::TCGCSV,
            Dataset::PRICES,
            "https://tcgcsv.com/tcgplayer/3/2/prices",
            Some(503),
            "http: 503 Service Unavailable",
        )
        .unwrap();

        let key =
            crate::keys::manifest_key(Source::TCGCSV, Dataset::PRICES, "2026-08-11", sink.run_id());

        // Already on disk before finalize ran — the failure flushed it.
        let flushed = read_manifest(tmp.path(), &key);
        assert!(!flushed.complete);
        assert_eq!(flushed.failures.len(), 1);
        assert_eq!(flushed.failures[0].status, Some(503));

        sink.finalize(Some("http: 503 Service Unavailable"))
            .unwrap();
        let final_manifest = read_manifest(tmp.path(), &key);
        assert!(!final_manifest.complete);
        assert_eq!(final_manifest.parts.len(), 1);
        assert!(final_manifest.error.unwrap().contains("503"));
    }

    /// A recorded failure poisons its own dataset's manifest even if the
    /// invocation as a whole goes on to succeed.
    #[test]
    fn a_failure_marks_only_its_own_dataset() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), "2026-08-11", STARTED);
        sink.land(
            Source::TCGCSV,
            Dataset::GROUPS,
            "https://tcgcsv.com/tcgplayer/79/groups",
            200,
            PartFormat::Json,
            b"sets",
        )
        .unwrap();
        sink.record_failure(
            Source::TCGCSV,
            Dataset::PRICES,
            "https://tcgcsv.com/tcgplayer/3/2/prices",
            None,
            "http: connection reset",
        )
        .unwrap();
        sink.finalize(None).unwrap();

        let m = |source, dataset| {
            read_manifest(
                tmp.path(),
                &crate::keys::manifest_key(source, dataset, "2026-08-11", sink.run_id()),
            )
        };
        assert!(m(Source::TCGCSV, Dataset::GROUPS).complete);
        assert!(!m(Source::TCGCSV, Dataset::PRICES).complete);
    }

    /// …but an invocation that failed marks every dataset it touched,
    /// including ones whose own fetches all succeeded. A `products` prefix
    /// holding 200 of an owed 450 parts has no failure of its own to record,
    /// so "the run died" is the only signal that can save it from reading as
    /// whole.
    #[test]
    fn an_invocation_failure_marks_every_touched_dataset() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), "2026-08-11", STARTED);
        sink.land(
            Source::TCGCSV,
            Dataset::GROUPS,
            "u",
            200,
            PartFormat::Json,
            b"all of the groups",
        )
        .unwrap();
        sink.land(
            Source::TCGCSV,
            Dataset::PRODUCTS,
            "u",
            200,
            PartFormat::Json,
            b"some of the products",
        )
        .unwrap();
        sink.finalize(Some("http: connection reset")).unwrap();

        for dataset in [Dataset::GROUPS, Dataset::PRODUCTS] {
            let m = read_manifest(
                tmp.path(),
                &crate::keys::manifest_key(Source::TCGCSV, dataset, "2026-08-11", sink.run_id()),
            );
            assert!(!m.complete, "{dataset} must not read as whole");
            assert!(m.failures.is_empty(), "{dataset} had no failure of its own");
            assert_eq!(m.error.as_deref(), Some("http: connection reset"));
        }
    }

    #[test]
    fn parts_are_numbered_per_dataset_not_per_run() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), "2026-08-11", STARTED);
        sink.land(
            Source::TCGCSV,
            Dataset::GROUPS,
            "u",
            200,
            PartFormat::Json,
            b"g",
        )
        .unwrap();
        sink.land(
            Source::TCGCSV,
            Dataset::PRODUCTS,
            "u",
            200,
            PartFormat::Json,
            b"p",
        )
        .unwrap();
        sink.finalize(None).unwrap();

        for (dataset, tag) in [(Dataset::GROUPS, "groups"), (Dataset::PRODUCTS, "products")] {
            let m = read_manifest(
                tmp.path(),
                &crate::keys::manifest_key(Source::TCGCSV, dataset, "2026-08-11", sink.run_id()),
            );
            assert!(m.parts[0].key.contains(&format!("dataset={tag}/")));
            assert!(m.parts[0].key.ends_with("part-0000.json.zst"));
        }
    }

    #[test]
    fn sha256_hex_is_the_known_answer() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_png_part_is_stored_byte_identical_under_its_hash_and_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::with_run(
            Box::new(DirStore::new(tmp.path())),
            "2026-08-11",
            STARTED,
            "01K2CJ1N0000000000000000AA",
        );
        let png: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00not really an image\xff\xfe";
        for _ in 0..2 {
            sink.land(
                Source::TCGCSV,
                Dataset::PRODUCTS,
                "https://up/img.png",
                200,
                PartFormat::Bin("image/png"),
                png,
            )
            .unwrap();
        }
        sink.finalize(None).unwrap();

        let sha = sha256_hex(png);
        let key = crate::keys::blob_key(
            Source::TCGCSV,
            Dataset::PRODUCTS,
            "2026-08-11",
            "01K2CJ1N0000000000000000AA",
            &sha,
            PartFormat::Bin("image/png"),
        );
        assert!(key.ends_with(&format!("blob-{sha}.png")));
        assert_eq!(std::fs::read(tmp.path().join(&key)).unwrap(), png);

        let manifest = read_manifest(
            tmp.path(),
            &crate::keys::manifest_key(
                Source::TCGCSV,
                Dataset::PRODUCTS,
                "2026-08-11",
                "01K2CJ1N0000000000000000AA",
            ),
        );
        assert_eq!(manifest.parts.len(), 1);
        assert_eq!(manifest.parts[0].key, key);
        assert_eq!(manifest.parts[0].sha256, sha);
        assert_eq!(manifest.parts[0].media_type(), Some("image/png"));
    }
}
