//! The derivation's clock — one instant, passed in, never read here.
//!
//! Before this existed, the site's `data refresh` called `Utc::now()` five times
//! on its way down: once for the `ingest_date` partition and again inside the
//! TCGCSV import (a timestamp and an `observed_date`). Several reads of one
//! clock in one job, and every one of them stamped a value into a row.
//!
//! That is fine while the job is the only thing producing those rows, and
//! impossible the moment a second job has to produce the *same* rows from the
//! bytes the first one landed. So the clock is read once, at the top, and
//! carried:
//!
//! - the online refresh reads it from the actual clock, lands it in every
//!   manifest ([`Manifest::started_at`](dumpster_kit_lake::Manifest::started_at)),
//!   and stamps it into its rows;
//! - the offline derive reads it back out of the manifest and stamps the same
//!   values into the same rows, which is what makes "row-identical" mean
//!   *identical* rather than "identical apart from the timestamps".
//!
//! [`observed_date`](DeriveClock::observed_date) is derived from that same
//! instant and stays distinct from the `ingest_date` partition, deliberately.
//! They are the same day for almost every run and differ for exactly the one
//! that crossed UTC midnight — which is the run where taking the partition
//! for the observation date would file yesterday's prices under today.

use chrono::{DateTime, Utc};

/// When a derivation's inputs were fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeriveClock {
    /// RFC 3339, the value written to every `fetched_at`-shaped column.
    fetched_at: String,
    /// `YYYY-MM-DD`, the value written to `prices.observed_at` and
    /// `sealed_prices.observed_at`.
    observed_date: String,
}

impl DeriveClock {
    /// The clock as it stands now. Only the *landing* side may call this —
    /// the deriving side takes the instant the landing recorded.
    pub fn now() -> Self {
        Self::at(Utc::now())
    }

    /// A clock pinned to `instant`.
    pub fn at(instant: DateTime<Utc>) -> Self {
        Self {
            fetched_at: instant.to_rfc3339(),
            observed_date: instant.format("%Y-%m-%d").to_string(),
        }
    }

    /// The clock a landed run recorded, parsed from a manifest's
    /// `started_at`.
    ///
    /// An empty or unparseable value is an error naming the partition rather
    /// than a fallback to today: a manifest with no clock in it was landed
    /// before the field existed, and a derive that invented one would produce
    /// a catalog whose timestamps quietly disagree with the run that fetched
    /// it. That is exactly the class of silent wrongness the epic is about.
    pub fn from_manifest(started_at: &str, what: &str) -> anyhow::Result<Self> {
        if started_at.trim().is_empty() {
            anyhow::bail!(
                "{what}: the run's manifest records no started_at, so the clock its rows were \
                 stamped with cannot be recovered.\n\
                 That partition was landed before the offline derive existed. Re-land the date \
                 (the site's data refresh) and derive from the new run, or derive a date whose \
                 manifests carry the field."
            );
        }
        let parsed = DateTime::parse_from_rfc3339(started_at.trim()).map_err(|e| {
            anyhow::anyhow!("{what}: started_at {started_at:?} is not RFC 3339: {e}")
        })?;
        Ok(Self::at(parsed.with_timezone(&Utc)))
    }

    /// RFC 3339 — what a `fetched_at` column gets.
    pub fn fetched_at(&self) -> &str {
        &self.fetched_at
    }

    /// `YYYY-MM-DD` — what an `observed_at` column gets.
    pub fn observed_date(&self) -> &str {
        &self.observed_date
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_observed_date_is_the_instant_s_day_not_a_second_reading() {
        let clock = DeriveClock::from_manifest("2026-08-11T23:59:59Z", "x").unwrap();
        assert_eq!(clock.observed_date(), "2026-08-11");
        // …and it survives the round trip that matters: land at 23:59:59 on
        // the 11th, derive whenever, still the 11th.
        assert_eq!(
            DeriveClock::from_manifest(clock.fetched_at(), "x").unwrap(),
            clock
        );
    }

    /// An offset is not UTC. A run landed at 00:30+02:00 observed the 11th in
    /// UTC, and the row it wrote says so.
    #[test]
    fn a_non_utc_offset_is_converted_not_truncated() {
        let clock = DeriveClock::from_manifest("2026-08-12T00:30:00+02:00", "x").unwrap();
        assert_eq!(clock.observed_date(), "2026-08-11");
    }

    #[test]
    fn a_clockless_manifest_refuses_and_says_what_to_do() {
        let err = DeriveClock::from_manifest("   ", "tcgcsv/groups 2026-08-01")
            .unwrap_err()
            .to_string();
        assert!(err.contains("tcgcsv/groups 2026-08-01"), "{err}");
        assert!(err.contains("data refresh"), "{err}");
        assert!(DeriveClock::from_manifest("not a date", "x").is_err());
    }
}
