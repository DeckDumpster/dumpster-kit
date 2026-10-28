//! The catalog machinery every game shares. A game implements [`Game`] in its own repo and
//! supplies only its schema, record types, finish model and crosswalk rules; the kit owns the
//! append-only prices, the source crosswalk store, the tcgcsv category mirror and the alarms.
//! Nothing here names a game attribute.

mod alarm;
mod error;
mod game;
mod ingest;
mod mirror;
mod prices;
mod schema;

pub use alarm::{Alarm, MISSING_PRICE_DAY, UNMATCHED_PRODUCT, alarms, raise};
pub use error::{Error, Result};
pub use game::{ApplyReport, FinishPlacement, Game, Match, SearchTerm};
pub use ingest::{IngestReport, TCGCSV, ingest_tcgcsv};
pub use mirror::mirror_categories;
pub use prices::{PriceRow, check_missing_days, record_prices};
pub use schema::{KIT_SCHEMA, install};
