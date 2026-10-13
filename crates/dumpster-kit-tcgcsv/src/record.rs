use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub group_id: i64,
    pub name: String,
    /// None when tcgcsv sends none or an empty one.
    pub abbreviation: Option<String>,
    pub published_on: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub product_id: i64,
    pub group_id: i64,
    pub name: String,
    /// `extendedData.Number`; None for a sealed product.
    pub number: Option<String>,
    pub image_url: Option<String>,
    /// Zero means `image_url` resolves to a 403.
    pub image_count: i64,
    pub url: Option<String>,
}

impl Product {
    pub fn is_single_card(&self) -> bool {
        self.number.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Price {
    pub product_id: i64,
    pub sub_type_name: String,
    pub low: Option<f64>,
    pub mid: Option<f64>,
    pub high: Option<f64>,
    pub market: Option<f64>,
    pub direct_low: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGroup {
    group_id: i64,
    name: String,
    abbreviation: Option<String>,
    published_on: Option<String>,
}

#[derive(Deserialize)]
struct Datum {
    name: String,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProduct {
    product_id: i64,
    group_id: i64,
    name: String,
    image_url: Option<String>,
    #[serde(default)]
    image_count: i64,
    url: Option<String>,
    #[serde(default)]
    extended_data: Vec<Datum>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPrice {
    product_id: i64,
    sub_type_name: Option<String>,
    low_price: Option<f64>,
    mid_price: Option<f64>,
    high_price: Option<f64>,
    market_price: Option<f64>,
    direct_low_price: Option<f64>,
}

fn results<T: for<'de> Deserialize<'de>>(bytes: &[u8], what: &str) -> Result<Vec<T>> {
    let envelope: Value = serde_json::from_slice(bytes)?;
    let arr = envelope
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Source(format!("{what} response has no results")))?;
    arr.iter()
        .map(|v| {
            T::deserialize(v).map_err(|e| Error::Source(format!("{what} unreadable ({e}): {v}")))
        })
        .collect()
}

pub fn parse_groups(bytes: &[u8]) -> Result<Vec<Group>> {
    Ok(results::<RawGroup>(bytes, "groups")?
        .into_iter()
        .map(|g| Group {
            group_id: g.group_id,
            name: g.name,
            abbreviation: g.abbreviation.filter(|s| !s.is_empty()),
            published_on: g.published_on,
        })
        .collect())
}

pub fn parse_products(bytes: &[u8]) -> Result<Vec<Product>> {
    Ok(results::<RawProduct>(bytes, "products")?
        .into_iter()
        .map(|p| Product {
            number: p
                .extended_data
                .iter()
                .find(|d| d.name == "Number")
                .and_then(|d| d.value.as_deref())
                .map(|s| s.trim().to_string()),
            product_id: p.product_id,
            group_id: p.group_id,
            name: p.name,
            image_url: p.image_url,
            image_count: p.image_count,
            url: p.url,
        })
        .collect())
}

pub fn parse_prices(bytes: &[u8]) -> Result<Vec<Price>> {
    results::<RawPrice>(bytes, "prices")?
        .into_iter()
        .map(|r| {
            Ok(Price {
                product_id: r.product_id,
                sub_type_name: r.sub_type_name.ok_or_else(|| {
                    Error::Source(format!("price for {} has no subTypeName", r.product_id))
                })?,
                low: r.low_price,
                mid: r.mid_price,
                high: r.high_price,
                market: r.market_price,
                direct_low: r.direct_low_price,
            })
        })
        .collect()
}
