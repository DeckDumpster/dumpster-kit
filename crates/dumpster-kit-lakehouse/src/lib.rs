//! `dumpster-kit-lakehouse` — what an offline derive needs that is not a site's catalog:
//! which raw runs a date reads ([`partition`]), answering an upstream request out of them
//! ([`replay`]), and comparing two catalogs row by row ([`diff`]).
//!
//! > Only lakehouse code reads `raw/`.
//!
//! These are *library* parts. The binary that wires them to a catalog — open the site's
//! `shared.sqlite`, replay a partition through the site's ingest, record the provenance rows —
//! names the site's database and ingest types, so it is the site's, not the kit's: the catalog
//! mapping is each consumer's part. A site's binary is a few dozen lines over these three
//! modules and [`dumpster_kit_derive`].

pub mod diff;
pub mod partition;
pub mod replay;
