//! tcgcsv client, parameterised by TCGplayer category. It owns the URL layout, the typed
//! group/product/price records, raw landing and the `last-updated.txt` short-circuit.
//! Transport and storage belong to the consumer: it supplies a [`Fetch`] (with whatever
//! retry it wants) and, optionally, a [`Landing`] that keeps the bytes.

mod client;
mod error;
mod record;

pub use client::{Fetch, Kind, Landing, Payload, Snapshot, Sync, TcgcsvClient, stamp_day};
pub use error::{Error, Result};
pub use record::{
    Category, Group, Price, Product, parse_categories, parse_groups, parse_prices, parse_products,
};

/// The origin tcgcsv.com serves from; pass to [`TcgcsvClient::new`].
pub const DEFAULT_BASE: &str = "https://tcgcsv.com";
