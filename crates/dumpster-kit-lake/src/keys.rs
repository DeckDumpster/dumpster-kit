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

/// The upstream a payload came from — the `source=` partition value.
///
/// Open on purpose: the kit does not enumerate any one site's upstreams. A
/// consuming site defines its own as associated `const`s built with
/// [`Source::new`]. Card art is deliberately absent from the kit's own
/// consts, see the crate docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Source(&'static str);

#[allow(non_upper_case_globals)]
impl Source {
    /// `tcgcsv.com/tcgplayer` — groups, products, prices.
    pub const TCGCSV: Source = Source::new("tcgcsv");
    /// The pre-newtype spelling of [`Source::TCGCSV`], kept so call sites compile.
    pub const Tcgcsv: Source = Source::TCGCSV;

    /// A source with this `source=` partition value. These strings are on-disk
    /// layout: a change to one orphans every object already landed.
    pub const fn new(name: &'static str) -> Source {
        Source(name)
    }

    /// The `source=` partition value.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `dataset=` partition value — which upstream endpoint's payloads a
/// prefix holds. Open, like [`Source`]: a site defines its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dataset(&'static str);

#[allow(non_upper_case_globals)]
impl Dataset {
    /// TCGCSV `/groups`.
    pub const GROUPS: Dataset = Dataset::new("groups");
    /// TCGCSV `/{group}/products`.
    pub const PRODUCTS: Dataset = Dataset::new("products");
    /// TCGCSV `/{group}/prices`.
    pub const PRICES: Dataset = Dataset::new("prices");
    /// The pre-newtype spelling of [`Dataset::GROUPS`].
    pub const Groups: Dataset = Dataset::GROUPS;
    /// The pre-newtype spelling of [`Dataset::PRODUCTS`].
    pub const Products: Dataset = Dataset::PRODUCTS;
    /// The pre-newtype spelling of [`Dataset::PRICES`].
    pub const Prices: Dataset = Dataset::PRICES;

    /// The three TCGCSV datasets, as before the type was opened. It is no
    /// longer "every dataset": a site's own list is its requirement table.
    pub const ALL: &'static [Dataset] = &[Dataset::GROUPS, Dataset::PRODUCTS, Dataset::PRICES];

    /// A dataset with this `dataset=` partition value. On-disk layout, as
    /// with [`Source::new`].
    pub const fn new(name: &'static str) -> Dataset {
        Dataset(name)
    }

    /// The `dataset=` partition value.
    pub const fn as_str(self) -> &'static str {
        self.0
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
    /// A binary payload (an image, say) of the given media type. Landed as
    /// the raw bytes: not compressed, not wrapped, keyed by content hash.
    Bin(&'static str),
}

impl PartFormat {
    /// The media type a [`PartFormat::Bin`] part declares.
    pub fn media_type(self) -> Option<&'static str> {
        match self {
            PartFormat::Bin(media_type) => Some(media_type),
            _ => None,
        }
    }

    /// Whether the stored object is zstd-compressed (every format but `Bin`).
    pub fn is_compressed(self) -> bool {
        !matches!(self, PartFormat::Bin(_))
    }

    /// The extension, without a leading dot.
    pub fn as_str(self) -> &'static str {
        match self {
            PartFormat::Bin(media_type) => match media_type {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                "image/gif" => "gif",
                "image/avif" => "avif",
                "image/svg+xml" => "svg",
                _ => "bin",
            },
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
        "{prefix}part-{part:04}.{ext}{zst}",
        prefix = run_prefix(source, dataset, ingest_date, run),
        ext = format.as_str(),
        zst = if format.is_compressed() { ".zst" } else { "" },
    )
}

/// The key of a binary part: named by the SHA-256 of its bytes rather than a
/// part number, so the same bytes landed twice are one object.
pub fn blob_key(
    source: Source,
    dataset: Dataset,
    ingest_date: &str,
    run: &str,
    sha256: &str,
    format: PartFormat,
) -> String {
    format!(
        "{prefix}blob-{sha256}.{ext}",
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

    #[test]
    fn a_site_defines_its_own_consts_and_they_lay_out_like_the_kit_s() {
        const SOURCE: Source = Source::new("other-site");
        const DATASET: Dataset = Dataset::new("listings");
        assert_eq!(
            run_prefix(SOURCE, DATASET, "2026-08-11", RUN_A),
            format!("raw/source=other-site/dataset=listings/ingest_date=2026-08-11/run={RUN_A}/")
        );
        assert_eq!(Source::TCGCSV.as_str(), "tcgcsv");
        assert_eq!(
            Dataset::ALL,
            &[Dataset::GROUPS, Dataset::PRODUCTS, Dataset::PRICES]
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
