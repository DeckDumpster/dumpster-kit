//! `dumpster-kit-lake` — the raw landing zone.
//!
//! Every byte a site fetches from an upstream lands here, immutably,
//! before anything parses it. Today's refresh fetches, parses into
//! `shared.sqlite`, and throws the bytes away; when a parser turns out to be
//! wrong the only recovery is to re-fetch whatever upstream serves *today*,
//! and yesterday's catalog is gone. This project has shipped two parser
//! defects in one day, so "the parser was wrong" is the normal case.
//!
//! ```text
//! raw/source=<tcgcsv>/
//!     dataset=<groups|products|prices>/
//!     ingest_date=YYYY-MM-DD/
//!     run=<ULID>/
//!         part-NNNN.<json|csv|tar.gz>.zst
//!         _manifest.json
//! ```
//!
//! Three things about that layout are load-bearing:
//!
//! - **`run=<ULID>`, not a timestamp.** A ULID sorts chronologically *and*
//!   disambiguates two runs on the same date, so a retry after a partial
//!   failure cannot land on the first attempt's objects.
//! - **A `_manifest.json` per run and dataset**, recording the upstream URL,
//!   HTTP status, byte count and SHA-256 of every part. "Did we actually get
//!   everything" is answerable without re-fetching a byte.
//! - **Nothing here parses.** The bytes stored are the bytes received.
//!
//! ## What is deliberately not landed
//!
//! Card art and set symbols. The retention
//! arithmetic behind this bucket (~4.1 MB/day compressed, ~1.5 GB/year, and
//! therefore no lifecycle rule at all) is for JSON only; landing card art
//! would change it completely. That is its own decision, not something that
//! creeps in under "everything we fetch".
//!
//! ## Retention
//!
//! **There is deliberately no lifecycle rule on `raw/`.** Indefinite
//! retention is the decision, measured rather than assumed: at ~4.1 MB/day
//! compressed that is ~1.5 GB/year and roughly $0.03/month in year one —
//! cheaper than revisiting the decision, and far cheaper than losing the
//! ability to rebuild a date. If you are here to tidy up an unmanaged
//! prefix: this is the one that is meant to be unmanaged.
//!
//! ## Where it writes
//!
//! A bucket named by `~/.config/<app>/lake.env` ([`LakeEnv`] derives the names from one `AppPrefix`) — host configuration, no
//! default, deliberately a *different* bucket from the Litestream backup
//! bucket. See [`config`].
//!
//! ## The other zone in that bucket
//!
//! Everything above describes the **catalog zone**: `raw/` and the `lake/`
//! warehouse beside it, cross-tenant, shared, retained indefinitely. The
//! same bucket also holds the **tenant zone** under `tenant/` — holdings and
//! valuations, always tenant-keyed, retained 90 days, reachable only by
//! credentials that reach nothing else. It is a different object under
//! different governance that happens to share a bucket, and [`tenant`] is
//! where its layout, its retention and its credential rule live.
//!
//! This crate says where those objects go and hands out the handle that
//! reaches them ([`open_tenant_zone`]) — it does not decide what is in one.
//! The shipper is its own crate (`dumpster-kit-ship`), because filling a tenant
//! part means reading a tenant's SQLite, and this crate deliberately links no
//! SQLite at all: that is what makes "no lake write path can open a tenant
//! database" structural rather than reviewed (`pd-cgi9` §1).

pub mod config;
mod error;
pub mod keys;
pub mod manifest;
pub mod reader;
pub mod sink;
pub mod store;
pub mod tenant;

pub use config::{Backend, LakeConfig, LakeEnv};
pub use error::{LakeError, Result};
pub use keys::{Dataset, PartFormat, Source};
pub use manifest::{Manifest, PartRecord};
pub use reader::{RawZone, Run, select_run};
pub use sink::RawLanding;
pub use store::{DirStore, ObjectPurge, ObjectSource, ObjectStore, S3Store};
pub use tenant::{
    Dataset as TenantDataset, PART_SUFFIX, RETENTION_DAYS, TENANT_ROOT, TenantZoneConfig,
    dataset_prefix, part_key, partition_prefix, range_part_key, tenant_prefix,
};

/// Build the landing zone this host is configured for, for **writing**.
///
/// `ingest_date` is the `YYYY-MM-DD` partition every object lands under and
/// `started_at` is the run's clock (RFC 3339), recorded in every manifest so
/// a later derive can reproduce the timestamps this run stamps into its rows.
/// Fails, naming `lake.env`, when no destination is configured — an
/// unconfigured lake refuses rather than silently landing nothing.
pub fn open(env: &LakeEnv, ingest_date: &str, started_at: &str) -> Result<RawLanding> {
    let store: Box<dyn ObjectStore> = match config(env)? {
        Backend::Dir(path) => Box::new(DirStore::new(path)),
        Backend::S3 {
            bucket,
            region,
            prefix,
            endpoint,
        } => Box::new(S3Store::connect(
            &bucket,
            &region,
            &prefix,
            endpoint.as_deref(),
        )?),
    };
    Ok(RawLanding::new(store, ingest_date, started_at))
}

/// Build the landing zone this host is configured for, for **reading**.
///
/// The same `lake.env`, resolved the same way — one file configures both
/// halves, so a derive can never end up reading a different lake from the one
/// the refresh landed into. What differs is the handle: this one has no
/// `put`, because `raw/` is immutable and the job that reads it must not be
/// able to rewrite the evidence.
pub fn open_reader(env: &LakeEnv) -> Result<RawZone> {
    let source: Box<dyn ObjectSource> = match config(env)? {
        Backend::Dir(path) => Box::new(DirStore::new(path)),
        Backend::S3 {
            bucket,
            region,
            prefix,
            endpoint,
        } => Box::new(S3Store::connect(
            &bucket,
            &region,
            &prefix,
            endpoint.as_deref(),
        )?),
    };
    Ok(RawZone::new(source))
}

/// A handle on the **tenant zone**, for writing.
///
/// The bucket is the lake's — one bucket, separate prefixes — but the
/// identity is not: [`TenantZoneConfig`] names a profile that reaches
/// `tenant/` and nothing else, and refuses to be the catalog's. So this is
/// the one entry point in the crate that does not take its credentials from
/// whatever the process happens to be configured with.
///
/// Write-only, like [`open`] and for the same reason: the shipper knows its
/// own key space and has no business reading anybody's holdings back.
/// [`open_tenant_zone_reader`] is the other half, for the paths that do.
pub fn open_tenant_zone(env: &LakeEnv) -> Result<(Box<dyn ObjectStore>, TenantZoneConfig)> {
    let zone = TenantZoneConfig::load(env)?;
    let store: Box<dyn ObjectStore> = match config(env)? {
        Backend::Dir(path) => Box::new(DirStore::new(path)),
        Backend::S3 {
            bucket,
            region,
            prefix,
            endpoint,
        } => Box::new(S3Store::connect_as(
            &bucket,
            &region,
            &prefix,
            endpoint.as_deref(),
            Some(&zone.profile),
        )?),
    };
    Ok((store, zone))
}

/// A handle on the **tenant zone**, for reading.
///
/// Not the shipper's — it never reads. This is what the decrypt path and the
/// deletion sweep hold, and it has no `put` for the mirror-image reason
/// [`open_reader`] does not: a job whose business is reading a tenant's data
/// must not be able to write to it.
pub fn open_tenant_zone_reader(env: &LakeEnv) -> Result<(Box<dyn ObjectSource>, TenantZoneConfig)> {
    let zone = TenantZoneConfig::load(env)?;
    let source: Box<dyn ObjectSource> = match config(env)? {
        Backend::Dir(path) => Box::new(DirStore::new(path)),
        Backend::S3 {
            bucket,
            region,
            prefix,
            endpoint,
        } => Box::new(S3Store::connect_as(
            &bucket,
            &region,
            &prefix,
            endpoint.as_deref(),
            Some(&zone.profile),
        )?),
    };
    Ok((source, zone))
}

/// A handle on the **tenant zone**, for the deletion sweep.
///
/// The third of three, and the narrowest: it can enumerate a prefix and
/// remove a key, and it can do nothing else. No `get`, so the job that
/// deletes a tenant's holdings never reads them; no `put`, so it cannot
/// write. That is not tidiness — a deletion is the one operation that runs
/// against data the operator has undertaken to stop holding, and the handle
/// it runs through should not be able to do anything with that data except
/// stop it existing.
///
/// The credentials are the tenant profile's, like the other two: `tenant/` is
/// the only prefix it reaches, and `s3:DeleteObject` there is already in
/// `deploy/policies/tenant-zone/tenant-credentials.json` — the sweep needs no
/// grant the shipper does not already have. Confinement to ONE tenant's
/// prefix is a level up, in `dumpster_kit_erase::sweep`.
pub fn open_tenant_zone_purge(env: &LakeEnv) -> Result<(Box<dyn ObjectPurge>, TenantZoneConfig)> {
    let zone = TenantZoneConfig::load(env)?;
    let purge: Box<dyn ObjectPurge> = match config(env)? {
        Backend::Dir(path) => Box::new(DirStore::new(path)),
        Backend::S3 {
            bucket,
            region,
            prefix,
            endpoint,
        } => Box::new(S3Store::connect_as(
            &bucket,
            &region,
            &prefix,
            endpoint.as_deref(),
            Some(&zone.profile),
        )?),
    };
    Ok((purge, zone))
}

fn config(env: &LakeEnv) -> Result<Backend> {
    Ok(LakeConfig::load(env)?.backend)
}
