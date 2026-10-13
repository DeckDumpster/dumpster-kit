//! The tenancy kernel: everything that decides whose data a request may touch.
//!
//! * [`paths`] — the data-directory layout and the two validators (handle, ULID `database_id`).
//! * [`registry`] — handle → `database_id`, identity bindings, and `schema_registry.sql`.
//! * [`connection`] — the one tenant-connection opener: read-only `ATTACH`, [`assert_isolated`],
//!   per-file `user_version` gating.
//! * [`access`] — Cloudflare Access JWT verification; [`VerifiedIdentity`] has no public
//!   constructor.
//! * [`tenant`] — resolution of a verified identity to a [`TenantId`] (no public constructor)
//!   and to its isolated connection.
//!
//! An application's identity (`PKDUMP`, `DUMPSTAR`, …) is the one [`AppPrefix`] it passes in; its
//! collection schema is the [`TenantDbSpec`] it supplies. Nothing else about a site is known
//! here. The conformance corpora live in `conformance/` and are loaded through
//! `dumpster-kit-conformance`.
//!
//! With the `testing` feature the crate also exports [`testing`]: an in-process RS256 JWKS
//! server that mints a valid token and every entry of the negative-JWT corpus.

pub mod access;
pub mod app;
pub mod connection;
pub mod error;
pub mod http;
pub mod paths;
pub mod registry;
pub mod schema_version;
pub mod tenant;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use access::{AccessConfig, AccessState, VerifiedIdentity};
pub use app::AppPrefix;
pub use connection::{
    BUSY_TIMEOUT, TenantDbSpec, assert_isolated, attach_shared_readonly, connect_tenant,
    init_tenant_schema, open_registry, open_tenant,
};
pub use error::{Result, TenancyError};
pub use http::HttpError;
pub use paths::{
    HANDLE_RULE, TENANTS_DIR, current_user, database_id_of, home, legacy_user_db_path,
    registry_db_path, shared_db_path, tenant_db_file, tenant_db_path, tenant_db_path_for_id,
    tenants_dir, validate_database_id, validate_tenant_name,
};
pub use schema_version::{SchemaSpec, SchemaState};
pub use tenant::{TenantId, Tenants};
