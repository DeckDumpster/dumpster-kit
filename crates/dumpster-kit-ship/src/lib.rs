//! `dumpstar-ship` — the shipper (`pd-dxn3`, item 4 of the inbound-leg epic).
//!
//! It moves rows from the ownership outbox (`pd-5m54`) into the tenant zone
//! (`pd-uz8q`), encrypted under the per-tenant key the key-custody registry
//! permits (`pd-ulds`). It is the seam between all three, and it is the only
//! thing in this workspace that writes under `tenant/`.
//!
//! ```text
//!  tenants/<database_id>.sqlite                         the tenant zone
//!  ┌──────────────────────────┐                ┌───────────────────────────┐
//!  │ collection               │                │ tenant/database_id=<id>/  │
//!  │   └─ triggers ──▶ outbox │ ──▶ shipper ──▶│   dataset=holdings/       │
//!  │ outbox_cursor            │      ▲         │     as_of=<date>/         │
//!  │ outbox_gap               │      │         │       part-seq-A-B        │
//!  └──────────────────────────┘   tenant key   └───────────────────────────┘
//!                                (derived, or refused by a tombstone)
//! ```
//!
//! ## Five decisions, none of them incidental
//!
//! ### 1. The shipper reads no clock
//!
//! Every partition value comes out of the data: `as_of` is the UTC date of
//! the event's own `occurred_at`, and a part is a maximal run of consecutive
//! outbox rows sharing that date. So re-shipping a range on a later day lands
//! in the *same* partition it landed in the first time, and no run has to be
//! told what day it is to be correct. (Contrast the transform tier, whose
//! `--date` is a genuine choice of which day to compute — there the scheduler
//! is the one component allowed to know. Here there is nothing to choose.)
//!
//! The one timestamp this crate writes is `shipped_at` on the cursor row,
//! which is operational and addresses nothing.
//!
//! ### 2. The object key is the sequence range, never an ordinal
//!
//! ```text
//! tenant/database_id=<id>/dataset=holdings/as_of=<date>/part-seq-<from>-<to>.parquet.enc
//! ```
//!
//! At-least-once delivery means a part will sometimes be shipped twice — a
//! crash between the PUT and the cursor write is the normal way, and it is
//! deliberately *not* prevented, because the alternative (advance the cursor
//! first) loses events instead of repeating them. An ordinal part number
//! would make the retry a second, different object holding the same rows.
//! Addressing a part by the rows it carries makes the retry address the
//! object it is retrying, which is what turns at-least-once into
//! at-least-once *and idempotent*. See [`plan`].
//!
//! ### 3. The encryption is deterministic, so the retry is byte-identical
//!
//! AES-256-GCM under the derived tenant key, with a nonce derived from the
//! object key and the plaintext rather than drawn at random ([`cipher`]).
//! A random nonce would make the retry in decision 2 write *different bytes*
//! to the same key — which is still idempotent in content but not in the
//! object store, and not provable by comparing what is there. The usual
//! danger of a non-random nonce is reuse across different plaintexts; here
//! the nonce is a function of the plaintext, so two parts share a nonce only
//! if they are the same part.
//!
//! ### 4. The cursor and the gap ledger live in the tenant's own database
//!
//! Beside the outbox they point into ([`cursor`]), so a restore of that
//! database restores the log and the position in it together, and so the
//! whole of a tenant's shipping state is dropped by the same deletion that
//! drops the tenant. Both tables are excluded from the portable JSON envelope
//! for the reason the outbox itself is: they are transport state, not
//! collection state.
//!
//! ### 5. A sequence gap is recorded, alarmed, and shipped past
//!
//! The outbox's `seq` is gap-free by construction — `AUTOINCREMENT`, written
//! by a trigger inside the mutation's own transaction, never reused even
//! across a trim. So a gap is not a curiosity, it is the proof that an event
//! was **lost**, and the offline copy is therefore incomplete in a way
//! nothing downstream could otherwise notice.
//!
//! What the shipper does about it is deliberate: it records the missing range
//! in [`cursor::GAP_TABLE`] *before* the cursor moves past it, prints it, and
//! ends the run at [`Outcome::Gap`] — which the wrapper turns into an alarm.
//! It does **not** stop shipping. The rows that are missing are already gone;
//! withholding the rows that are still there would be a second loss caused by
//! the detection of the first. "Detected and alarmed" is the requirement;
//! "detected and stuck" would be a self-inflicted outage on a tenant whose
//! data is already incomplete.
//!
//! ## What it deliberately does not do
//!
//! * **It does not branch on where an outbox row came from.** Item 5
//!   (backfill/redrive) emits synthetic events *through the outbox*, marked
//!   with a provenance column; a consumer that treated them differently would
//!   make the backfill a second code path instead of the same one. Every row
//!   is shipped identically.
//! * **It does not ship a tombstoned tenant.** [`dumpster_kit_keys::tenant_key`]
//!   refuses to derive the key, and that refusal is the authority — this
//!   crate does not second-guess it or carry its own idea of who is deleted.
//! * **It does not register anybody.** An id nothing is recorded about is
//!   skipped and named, because absence is not permission (`pd-ulds`).
//!
//! ## The other direction: reading the zone back (`pd-szh2`)
//!
//! [`zone`] is the inverse — list a tenant's holdings parts, open them under
//! the same derived key, decode them into the same [`Event`], and reduce them
//! with the same [`outbox::project`]. It exists because Phase 3
//! values a collection from the zone rather than from `collection`, and it
//! lives in this crate rather than beside the transform for the reason
//! everything else here does: the envelope, the key derivation and the
//! resolution rule each have exactly one implementation, and it is this one.
//!
//! Writing is still the *only* thing that touches `tenant/` with a `put` —
//! the reader holds an [`dumpster_kit_lake::ObjectSource`], which has none.

pub mod cipher;
pub mod cursor;
pub mod encode;
pub mod error;
pub mod outbox;
pub mod plan;
pub mod run;
#[cfg(test)]
mod test_support;
pub mod zone;

pub use error::{Result, ShipError};
pub use plan::{Event, Gap, Part, plan};
pub use run::{Outcome, Report, ShipContext, Status, Tenant, TenantOutcome, ship_all, ship_one};
pub use zone::{ZoneHoldings, materialize, read};
