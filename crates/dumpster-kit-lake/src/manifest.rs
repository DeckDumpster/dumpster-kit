//! `_manifest.json` — the answer to "did we actually get everything?",
//! readable without re-fetching a byte.
//!
//! One manifest per `(source, dataset, ingest_date, run)` prefix, sitting
//! alongside the parts it describes. It records, per part, the upstream URL,
//! the HTTP status, the uncompressed byte count and the SHA-256 of those
//! uncompressed bytes — so the stored objects can be verified against it
//! offline.
//!
//! It also records whether the run finished. A run that dies partway leaves
//! `"complete": false` and the error text that stopped it; a run that never
//! reached its finalizer leaves no manifest at all. Both are legible. What
//! must never happen is a manifest that looks whole while the prefix is
//! short.

use serde::{Deserialize, Serialize};

use crate::keys::{Dataset, Source};

/// One landed payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartRecord {
    /// The object key, starting at `raw/` — the key [`crate::keys`] built,
    /// **without** any configured bucket prefix. The store adds that at PUT
    /// time and a reader applies its own, so recording it here would make
    /// every key double-prefixed for anyone who moved the lake.
    pub key: String,
    /// The upstream URL this payload came from, query string and all.
    pub url: String,
    /// The HTTP status the upstream answered with.
    pub status: u16,
    /// Length of the payload **before** zstd compression.
    pub bytes: u64,
    /// SHA-256 of the payload before zstd compression, lowercase hex.
    pub sha256: String,
}

impl PartRecord {
    /// The media type of a binary part, read from its key's extension; `None`
    /// for a compressed part. A binary part is stored as the raw bytes with
    /// no zstd layer, and an extension outside the known image types reads
    /// back as `application/octet-stream`.
    pub fn media_type(&self) -> Option<&'static str> {
        if self.key.ends_with(".zst") {
            return None;
        }
        let extension = self.key.rsplit('.').next()?;
        Some(crate::keys::image_media_type(extension).unwrap_or("application/octet-stream"))
    }
}

/// A fetch that failed. Recorded in place of the part it would have been, so
/// a gap in the part numbering is never something you have to infer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureRecord {
    /// The upstream URL that failed.
    pub url: String,
    /// The HTTP status, when the request got far enough to have one.
    pub status: Option<u16>,
    /// What went wrong, as rendered by the fetching code.
    pub error: String,
}

/// The whole manifest for one run of one `(source, dataset)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// The `source=` partition value.
    pub source: String,
    /// The `dataset=` partition value.
    pub dataset: String,
    /// The `ingest_date=` partition value, `YYYY-MM-DD`.
    pub ingest_date: String,
    /// The `run=` partition value — the run's ULID.
    pub run: String,
    /// When the run STARTED fetching, RFC 3339 — the instant the derivation
    /// that consumed these bytes stamped into every row it wrote.
    ///
    /// This is what makes a rebuild from `raw/` reproduce the original rows
    /// exactly rather than approximately: `sets.ptcgio_fetched_at`,
    /// `tcgcsv_products.fetched_at` and `prices.observed_at` are all derived
    /// from the run's clock, so an offline derive that reads this back
    /// produces the same values a second time. Taking `ingest_date` for the
    /// same purpose would be the "old data looks new" bug the epic names:
    /// they are the same day for almost every run and different for exactly
    /// the run that crossed UTC midnight.
    ///
    /// `#[serde(default)]` because manifests landed before this field existed
    /// are already in the bucket and must still parse. Empty is not a clock,
    /// and the offline derive refuses such a partition by name rather than
    /// inventing one.
    #[serde(default)]
    pub started_at: String,
    /// When the manifest was written, RFC 3339.
    pub finalized_at: String,
    /// Whether every fetch this prefix was going to receive arrived.
    /// `false` means the run stopped early — read `error`.
    pub complete: bool,
    /// Why the run stopped, when `complete` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Every payload landed under this prefix, in fetch order.
    pub parts: Vec<PartRecord>,
    /// Every fetch that failed under this prefix, in fetch order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<FailureRecord>,
}

impl Manifest {
    /// An empty manifest for a run that has landed nothing yet.
    ///
    /// `started_at` is the run's clock — see the field.
    pub fn new(
        source: Source,
        dataset: Dataset,
        ingest_date: &str,
        run: &str,
        started_at: &str,
    ) -> Self {
        Self {
            source: source.as_str().to_string(),
            dataset: dataset.as_str().to_string(),
            ingest_date: ingest_date.to_string(),
            run: run.to_string(),
            started_at: started_at.to_string(),
            finalized_at: String::new(),
            complete: false,
            error: None,
            parts: Vec::new(),
            failures: Vec::new(),
        }
    }

    /// Total uncompressed bytes landed under this prefix.
    pub fn total_bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.bytes).sum()
    }

    /// Serialize for storage. Pretty-printed and newline-terminated: this is
    /// the file a human opens when a refresh looks wrong.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut out = serde_json::to_vec_pretty(self)?;
        out.push(b'\n');
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STARTED: &str = "2026-08-11T04:51:02Z";

    fn part(key: &str, bytes: u64) -> PartRecord {
        PartRecord {
            key: key.to_string(),
            url: "https://tcgcsv.com/tcgplayer/3/groups".to_string(),
            status: 200,
            bytes,
            sha256: "0".repeat(64),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let mut m = Manifest::new(
            Source::TCGCSV,
            Dataset::GROUPS,
            "2026-08-11",
            "01K2CJ1N00",
            STARTED,
        );
        m.parts.push(part("raw/…/part-0000.json.zst", 12));
        m.complete = true;
        m.finalized_at = "2026-08-11T04:53:17Z".to_string();

        let bytes = m.to_json().unwrap();
        assert_eq!(m, serde_json::from_slice::<Manifest>(&bytes).unwrap());
    }

    #[test]
    fn an_incomplete_run_carries_its_error() {
        let mut m = Manifest::new(
            Source::TCGCSV,
            Dataset::PRICES,
            "2026-08-11",
            "01K2CJ1N00",
            STARTED,
        );
        m.parts.push(part("raw/…/part-0000.json.zst", 12));
        m.failures.push(FailureRecord {
            url: "https://tcgcsv.com/tcgplayer/3/17/prices".to_string(),
            status: Some(503),
            error: "http: 503 Service Unavailable".to_string(),
        });
        m.error = Some("http: 503 Service Unavailable".to_string());

        let back: Manifest = serde_json::from_slice(&m.to_json().unwrap()).unwrap();
        assert!(!back.complete);
        assert_eq!(back.failures.len(), 1);
        assert!(back.error.unwrap().contains("503"));
    }

    #[test]
    fn a_clean_manifest_omits_the_empty_fields() {
        let mut m = Manifest::new(
            Source::TCGCSV,
            Dataset::GROUPS,
            "2026-08-11",
            "01K2CJ1N00",
            STARTED,
        );
        m.complete = true;
        let text = String::from_utf8(m.to_json().unwrap()).unwrap();
        assert!(!text.contains("\"error\""));
        assert!(!text.contains("\"failures\""));
    }

    /// Manifests written before `started_at` existed are already in the
    /// bucket. They must still parse — the reader is what decides that a
    /// clockless partition cannot be derived from, and it can only decide
    /// that if it can read the file at all.
    #[test]
    fn a_manifest_from_before_started_at_still_parses() {
        let legacy = br#"{
          "source": "tcgcsv", "dataset": "groups", "ingest_date": "2026-08-11",
          "run": "01K2CJ1N00", "finalized_at": "2026-08-11T04:53:17Z",
          "complete": true, "parts": []
        }"#;
        let m: Manifest = serde_json::from_slice(legacy).unwrap();
        assert!(m.complete);
        assert_eq!(m.started_at, "");
    }

    #[test]
    fn total_bytes_sums_the_parts() {
        let mut m = Manifest::new(
            Source::TCGCSV,
            Dataset::PRODUCTS,
            "2026-08-11",
            "01K2CJ1N00",
            STARTED,
        );
        m.parts.push(part("a", 10));
        m.parts.push(part("b", 32));
        assert_eq!(m.total_bytes(), 42);
    }
}
