//! `dumpster-kit-erase` — **the deletion path** (`pd-qbrf`, item 9 of the
//! inbound-leg epic).
//!
//! Deleting an account from the tenant zone, end to end, and then proving it.
//!
//! ```text
//!   registry.sqlite                          the tenant zone
//!   ┌─────────────────────────┐      ┌──────────────────────────────────┐
//!   │ tenant_key(<id>)        │      │ tenant/database_id=<id>/         │
//!   │   state -> tombstoned  ─┼──1──▶│   dataset=holdings/   …          │
//!   └─────────────────────────┘      │   dataset=valuations/ …          │
//!                │                   └──────────────┬───────────────────┘
//!                │                          2. dropped, object by object
//!                └───────────── 3. every read path attempted, all refused
//! ```
//!
//! ## Deletion is two acts, and each is the other's backstop
//!
//! The epic's design calls for defence in depth, and these are the two
//! depths:
//!
//! * **The partition drop** is the erasure. `database_id` is the FIRST
//!   partition component of the tenant zone, so one prefix covers a tenant's
//!   holdings and their valuations together, and removing it is a delete per
//!   object rather than a file rewrite. (That is also why the zone is plain
//!   Parquet and not Iceberg.)
//! * **The tombstone** is the crypto-shredding. It stops this system deriving
//!   that tenant's key ever again, which makes anything the drop *missed* —
//!   a compacted file, an older snapshot, a copy somewhere nobody remembered
//!   — ciphertext nobody holds a key for. The bounded blast radius is the
//!   point: the drop has to find every copy, and the tombstone does not.
//!
//! Neither is sufficient. A drop without a tombstone leaves a live key, so
//! any surviving copy is readable. A tombstone without a drop leaves the
//! objects sitting there, and the design says the drop is the erasure. The
//! verification insists on both, separately, by name.
//!
//! ## The bar is "proven, not asserted"
//!
//! [`verify`] is the module that meets it, and the property it is arranged
//! around is that a check which cannot be *run* must never report as one that
//! *passed*. Two vacuity traps get explicit guards, both described there: a
//! box with no master key would find every tenant on it beautifully
//! unreadable, and a "stray copy" that was never encrypted does not open
//! either.
//!
//! It is also seen in the failing direction before it is trusted in the
//! passing one. Run against a tenant who has not been deleted, `verify`
//! reports NOT PROVEN and opens the stray copy to show why.
//!
//! ## Where it runs
//!
//! **Offline**, like `dumpster-kit-ship` and `dumpster-kit-lake-derive`, and for the same
//! reason: it needs the tenant-zone credentials and the master key, and
//! nothing that serves a request may hold either. the site's online CLI does not
//! depend on this crate, so the binary that runs the site's `serve` does not link
//! a tenant-zone deleter. `deploy/erase.sh` is the wrapper and
//! `deploy/DELETION.md` is the runbook.
//!
//! ## What is deliberately somebody else's job
//!
//! The **online** half of removing an account — releasing the handle,
//! deleting the collection database, dropping the Litestream replica — is
//! the site's `tenant detach` and the site's `tenant purge`, in the online CLI, on
//! their own schedule (`deploy/TENANTS.md`). Conflating the two would put a
//! tenant-zone credential in the binary that serves requests, which is the
//! coupling the whole zone split exists to prevent.

pub mod delete;
pub mod error;
pub mod sweep;
pub mod verify;

#[cfg(test)]
mod test_support;

pub use delete::{Deletion, delete};
pub use error::{EraseError, Result};
pub use sweep::{Dropped, Sweep};
pub use verify::{Check, Proof, StrayCopy, Verdict, verify};
