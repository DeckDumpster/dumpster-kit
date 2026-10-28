use std::collections::BTreeMap;

use dumpster_kit_tcgcsv::{Price, Product};
use rusqlite::Transaction;

use crate::error::Result;
use crate::search::Vocabulary;

/// Where a game's finish lives: on the printing row itself, or chosen per owned copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishPlacement {
    OnPrinting,
    OnCopy,
}

/// One upstream a game reads. `name` keys raw landing, the ledger and sync_state;
/// `location` is what the [`dumpster_kit_tcgcsv::Fetch`] is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub location: String,
}

/// What the crosswalk made of one tcgcsv product (and price row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    Printing {
        printing_id: String,
        /// `Some` exactly when the game's placement is `OnCopy`: the finish the price is for.
        finish: Option<String>,
    },
    Sealed {
        sealed_id: String,
    },
    Unmatched {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TableChange {
    pub inserted: u64,
    pub updated: u64,
    pub deleted: u64,
}

impl TableChange {
    pub fn total(&self) -> u64 {
        self.inserted + self.updated + self.deleted
    }
}

/// Rows changed per table by one [`Game::apply`]. A game reports only rows that actually
/// changed: re-applying a payload it already holds must report zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub tables: BTreeMap<String, TableChange>,
}

impl ApplyReport {
    pub fn inserted(&mut self, table: &str, n: u64) {
        self.tables.entry(table.to_string()).or_default().inserted += n;
    }
    pub fn updated(&mut self, table: &str, n: u64) {
        self.tables.entry(table.to_string()).or_default().updated += n;
    }
    pub fn deleted(&mut self, table: &str, n: u64) {
        self.tables.entry(table.to_string()).or_default().deleted += n;
    }
    pub fn total(&self) -> u64 {
        self.tables.values().map(TableChange::total).sum()
    }
}

/// A game, implemented in the game's own repository. The kit never sees a game's attributes:
/// they live in `SCHEMA` as typed columns and in `Payload`, both owned by the game.
pub trait Game {
    /// Keys raw landing, the ledger and the schema record. Lowercase `[a-z0-9_-]`.
    const ID: &'static str;
    /// Bump when `SCHEMA` changes; a file recording another version is rebuilt from raw landing.
    const SCHEMA_VERSION: u32;
    /// The game's own DDL, run after the kit's. It only ever runs
    /// against an empty file.
    const SCHEMA: &'static str;
    const FINISH: FinishPlacement;
    type Payload;

    fn sources() -> Vec<Source>;
    /// Raw bytes of one source to the game's typed payload. Pure.
    fn parse(source: &str, raw: &[u8]) -> Result<Self::Payload>;
    /// Write the payload through `tx`, in the caller's transaction, reporting changed rows.
    fn apply(tx: &Transaction<'_>, payload: &Self::Payload) -> Result<ApplyReport>;
    fn crosswalk(product: &Product, price: Option<&Price>) -> Match;
    /// The game's search vocabulary, as data: the kit compiles queries against it.
    fn search_terms() -> Vocabulary;
    fn tcgcsv_category() -> i64;
}
