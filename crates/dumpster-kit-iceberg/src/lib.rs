//! `dumpster-kit-iceberg` — the Rust client for the Iceberg lake.
//!
//! Nessie speaks the Iceberg REST catalog spec at `/iceberg/`, so the client
//! is iceberg-rust's stock [`RestCatalog`](iceberg_catalog_rest::RestCatalog).
//! The server owns storage configuration (warehouse, S3 endpoint); the
//! environment only names the endpoint, the Nessie ref and, for a test
//! substrate, S3 overrides. Env names are the Python lake's, unchanged, so one
//! host configuration serves both clients during the cutover.
//!
//! The two price tables are **read and written unchanged**: [`prices`] holds
//! the schemas and partition specs field-for-field as `pkdump_lake.prices`
//! created them (same field ids, same identity partition on `observed_date`),
//! so a table either client created is one both can load.
//!
//! ## Day replacement
//!
//! iceberg-rust 0.10 has no overwrite or delete action, only append. The
//! Python job replaces a day in one commit; [`prices::append_day`] cannot, so
//! it refuses a day that already has rows rather than doubling it. Replacing a
//! day stays with the Python job until iceberg-rust grows the action.

mod catalog;
pub mod credentials;
mod error;
pub mod prices;

pub use catalog::{LakeCatalogConfig, open};
pub use error::{Error, Result};
