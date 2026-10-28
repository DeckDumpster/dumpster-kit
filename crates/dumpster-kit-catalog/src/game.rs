use dumpster_kit_tcgcsv::{Price, Product};
use rusqlite::Transaction;

use crate::error::Result;

/// Where a game's finish lives. `OnPrinting`: finish is part of what the printing is, so
/// prices key by printing id alone. `OnCopy`: one printing exists in several finishes and a
/// price (and a collection row) names which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishPlacement {
    OnPrinting,
    OnCopy,
}

/// What a game's crosswalk decides about one tcgcsv product/price pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// `finish` is `Some` exactly when the game's placement is `OnCopy`.
    Printing {
        printing_id: i64,
        finish: Option<String>,
    },
    /// A sealed product; the kit records it in `sealed_products`.
    Sealed,
    /// Nothing in the game's catalog; the kit raises the alarm.
    Unmatched,
}

/// A vocabulary key the game's search accepts, and the column it filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTerm {
    pub key: &'static str,
    pub column: &'static str,
}

/// Rows changed per table by one apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub rows_changed: Vec<(String, u64)>,
}

/// Implemented in each game's own repo.
pub trait Game {
    const ID: &'static str;
    /// The game's own DDL, as typed columns.
    const SCHEMA: &'static str;
    const FINISH: FinishPlacement;
    type Payload;

    /// Source names the apply job fetches, each landed under `raw/<game>/<source>/<day>/`.
    fn sources() -> Vec<&'static str>;
    fn parse(source: &str, raw: &[u8]) -> Result<Self::Payload>;
    fn apply(tx: &Transaction<'_>, payload: &Self::Payload) -> Result<ApplyReport>;
    fn crosswalk(product: &Product, price: &Price) -> Match;
    fn search_terms() -> Vec<SearchTerm>;
    fn tcgcsv_category() -> i64;
}
