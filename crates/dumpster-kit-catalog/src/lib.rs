//! Game-agnostic catalog machinery. The kit never names a game field: a game supplies its own
//! schema, payload and apply through [`Game`]; everything here is raw landing, the payload
//! ledger, the apply job, the single owner of `shared.sqlite`, alarms and sync_state.

pub mod alarms;
mod catalog;
mod ddl;
mod error;
pub mod fake;
mod game;
mod landing;

pub use catalog::{ALARM_APPLY_FAILED, ALARM_FETCH_FAILED, Catalog, RunContext, SourceOutcome};
pub use ddl::{KIT_DDL, KIT_SCHEMA_VERSION};
pub use error::{Error, Result};
pub use game::{ApplyReport, FinishPlacement, Game, Match, Source, TableChange};
pub use landing::{Landed, RawLanding, hash_hex};
