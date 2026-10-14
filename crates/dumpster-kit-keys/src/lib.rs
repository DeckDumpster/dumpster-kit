//! `dumpstar-keys` — key custody for the tenant zone (`pd-ulds`).
//!
//! The tenant zone (`pd-uz8q`) carries tenant-keyed holdings and valuations
//! offline. Deleting an account there is a **partition drop**; this crate is
//! the defence in depth beside it — crypto-shredding, so that bytes which
//! survive a drop somewhere (an older copy, a backup, a mistake) are bytes
//! nobody holds a key for.
//!
//! ```text
//!   ~/.config/dumpstar/<instance>/tenant-master.key      one master key, mode 600
//!            │                                          (beside litestream.env)
//!            │  HKDF-SHA256(salt, master, database_id)
//!            ▼
//!   per-tenant key                                    derived, never stored
//!            ▲
//!            │  refused when …
//!   registry.sqlite : tenant_key(database_id -> active | tombstoned)
//! ```
//!
//! Four decisions, all of them the bead's rather than this code's:
//!
//! * **One master key, held offline.** A file on the box at mode 600, in the
//!   same host-config directory as the Litestream credentials, backed up the
//!   same way they are — the operator's password manager, `deploy/KEYS.md`.
//!   The trade is not hidden: destroying that one file destroys everything,
//!   which is why [`master`] refuses to overwrite it and why the destruction
//!   path cannot reach it.
//! * **Per-tenant keys derived, not stored** — [`derive`]. No key service, no
//!   per-tenant secret to rotate or lose.
//! * **A registry of key state** — [`state`], a table in `registry.sqlite`,
//!   which is already replicated off-box and is the first thing restored
//!   after a total loss. Absence is not permission: an unregistered id is
//!   refused, so a registry restored empty is loud rather than permissive.
//! * **Backup and destruction are different paths** — [`backup`] and
//!   [`destroy`], and `tests/separation.rs` is what keeps them different.
//!
//! ## The property everything here is arranged around
//!
//! > A lost key is indistinguishable from a deleted tenant, by design.
//!
//! True of the ciphertext, and it must not become true of the *system*. See
//! [`error`], where the distinction is a type, and [`derive`], where the
//! order of two lookups is what preserves it.

pub mod backup;
pub mod derive;
pub mod destroy;
pub mod error;
pub mod master;
pub mod state;

#[cfg(test)]
mod test_support;

pub use derive::{TENANT_KEY_LEN, TenantKey, tenant_key};
pub use error::{KeyError, Result};
pub use master::{MASTER_KEY_LEN, MasterKey};
pub use state::{KeyState, TenantKeyState};
