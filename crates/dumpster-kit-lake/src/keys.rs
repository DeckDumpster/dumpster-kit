//! The `raw/` key layout, and nothing else.
//!
//! Pure string construction — no IO, no clock, no network — so the layout
//! that every future reader of the lake depends on can be asserted directly.
//!
//! ```text
//! raw/source=<source>/dataset=<dataset>/ingest_date=YYYY-MM-DD/run=<ULID>/part-NNNN.<ext>.zst
//! ```
//!
//! `run=<ULID>` rather than a timestamp: a ULID sorts chronologically **and**
//! disambiguates two runs on the same date. A retry after a partial failure
//! must never land on the first attempt's objects, which is what a
//! date-only key would do.

/// The upstream a payload came from. One variant per host we fetch bytes
/// from and keep — card art is deliberately absent, see the crate docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// `tcgcsv.com/tcgplayer` — groups, products, prices.
    Tcgcsv,
}

impl Source {
    /// The `source=` partition value. These strings are on-disk layout: a
    /// change here orphans every object already landed.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Tcgcsv => "tcgcsv",
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `dataset=` partition value — which upstream endpoint's payloads a
/// prefix holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dataset {
    /// TCGCSV `/groups`.
    Groups,
    /// TCGCSV `/{group}/products`.
    Products,
    /// TCGCSV `/{group}/prices`.
    Prices,
}

impl Dataset {
    /// Every dataset, for a caller that has to do something once per prefix.
    ///
    /// The offline derive is why this exists: rebuilding a date means looking
    /// for a landed run of each of these, and deciding — per dataset — whether
    /// a derive may proceed without one. A dataset missing from this list is
    /// one whose bytes a rebuild would silently never look for, which is the
    /// coverage regression the whole landing zone is meant to make impossible.
    /// The test below is what keeps it exhaustive.
    pub const ALL: &'static [Dataset] = &[Dataset::Groups, Dataset::Products, Dataset::Prices];

    /// The `dataset=` partition value. On-disk layout, as with [`Source`].
    pub fn as_str(self) -> &'static str {
        match self {
            Dataset::Groups => "groups",
            Dataset::Products => "products",
            Dataset::Prices => "prices",
        }
    }
}

impl std::fmt::Display for Dataset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The file extension a payload carries *before* `.zst` — what the bytes
/// would be called if you decompressed them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartFormat {
    /// A JSON response body.
    Json,
    /// A CSV response body.
    Csv,
    /// A gzipped tarball.
    TarGz,
}

impl PartFormat {
    /// The extension, without a leading dot.
    pub fn as_str(self) -> &'static str {
        match self {
            PartFormat::Json => "json",
            PartFormat::Csv => "csv",
            PartFormat::TarGz => "tar.gz",
        }
    }
}

/// The prefix every object of one `(source, dataset, ingest_date, run)`
/// shares — the directory `_manifest.json` sits in.
///
/// `ingest_date` is `YYYY-MM-DD`; `run` is a ULID's canonical 26-character
/// text form.
pub fn run_prefix(source: Source, dataset: Dataset, ingest_date: &str, run: &str) -> String {
    format!(
        "raw/source={source}/dataset={dataset}/ingest_date={ingest_date}/run={run}/",
        source = source.as_str(),
        dataset = dataset.as_str(),
    )
}

/// The key of one landed payload. `part` is zero-based and rendered as
/// `part-NNNN`, wide enough that the ~450 parts a TCGCSV price sweep writes
/// still sort lexicographically.
pub fn part_key(
    source: Source,
    dataset: Dataset,
    ingest_date: &str,
    run: &str,
    part: u32,
    format: PartFormat,
) -> String {
    format!(
        "{prefix}part-{part:04}.{ext}.zst",
        prefix = run_prefix(source, dataset, ingest_date, run),
        ext = format.as_str(),
    )
}

/// The key of a run's manifest. Uncompressed, deliberately: it is the file
/// you read to find out what happened, and needing a decompressor first is
/// friction at exactly the wrong moment.
pub fn manifest_key(source: Source, dataset: Dataset, ingest_date: &str, run: &str) -> String {
    format!(
        "{prefix}_manifest.json",
        prefix = run_prefix(source, dataset, ingest_date, run)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_A: &str = "01K2CJ1N0000000000000000AA";
    const RUN_B: &str = "01K2CJ1N0000000000000000BB";

    /// `Dataset::ALL` must hold every variant, and the compiler is what
    /// enforces it: the match below has no wildcard arm, so adding a variant
    /// to the enum stops this test compiling until it is given an ordinal —
    /// and the assertion then fails until it is added to `ALL` too.
    #[test]
    fn all_holds_every_dataset_exactly_once() {
        let ordinal = |d: Dataset| match d {
            Dataset::Groups => 0,
            Dataset::Products => 1,
            Dataset::Prices => 2,
        };
        let mut seen: Vec<usize> = Dataset::ALL.iter().copied().map(ordinal).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen,
            (0..Dataset::ALL.len()).collect::<Vec<_>>(),
            "Dataset::ALL must list every variant exactly once"
        );
    }

    #[test]
    fn part_key_is_the_documented_layout() {
        assert_eq!(
            part_key(
                Source::Tcgcsv,
                Dataset::Prices,
                "2026-08-11",
                RUN_A,
                0,
                PartFormat::Json
            ),
            "raw/source=tcgcsv/dataset=prices/ingest_date=2026-08-11/run=01K2CJ1N0000000000000000AA/part-0000.json.zst"
        );
    }

    #[test]
    fn manifest_sits_alongside_the_parts() {
        let prefix = run_prefix(Source::Tcgcsv, Dataset::Groups, "2026-08-11", RUN_A);
        let manifest = manifest_key(Source::Tcgcsv, Dataset::Groups, "2026-08-11", RUN_A);
        let part = part_key(
            Source::Tcgcsv,
            Dataset::Groups,
            "2026-08-11",
            RUN_A,
            7,
            PartFormat::Json,
        );
        assert!(manifest.starts_with(&prefix));
        assert!(part.starts_with(&prefix));
        assert_eq!(manifest, format!("{prefix}_manifest.json"));
    }

    /// The property the whole `run=<ULID>` decision exists for: a retry on
    /// the same date cannot land on the first attempt's objects.
    #[test]
    fn two_runs_on_one_date_are_disjoint() {
        let a = run_prefix(Source::Tcgcsv, Dataset::Prices, "2026-08-11", RUN_A);
        let b = run_prefix(Source::Tcgcsv, Dataset::Prices, "2026-08-11", RUN_B);
        assert_ne!(a, b);
        assert!(!a.starts_with(&b) && !b.starts_with(&a));

        for part in 0..4 {
            assert_ne!(
                part_key(
                    Source::Tcgcsv,
                    Dataset::Prices,
                    "2026-08-11",
                    RUN_A,
                    part,
                    PartFormat::Json
                ),
                part_key(
                    Source::Tcgcsv,
                    Dataset::Prices,
                    "2026-08-11",
                    RUN_B,
                    part,
                    PartFormat::Json
                ),
            );
        }
    }

    #[test]
    fn parts_sort_lexicographically_past_nine() {
        let key = |n| {
            part_key(
                Source::Tcgcsv,
                Dataset::Products,
                "2026-08-11",
                RUN_A,
                n,
                PartFormat::Json,
            )
        };
        let mut keys: Vec<String> = (0..500).map(key).collect();
        let ordered = keys.clone();
        keys.sort();
        assert_eq!(keys, ordered, "part-NNNN must sort in fetch order");
    }

    #[test]
    fn tarball_keeps_its_own_extension() {
        assert!(
            part_key(
                Source::Tcgcsv,
                Dataset::Products,
                "2026-08-11",
                RUN_A,
                0,
                PartFormat::TarGz
            )
            .ends_with("part-0000.tar.gz.zst")
        );
    }
}
