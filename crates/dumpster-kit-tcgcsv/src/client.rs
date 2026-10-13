use crate::error::{Error, Result};
use crate::record::{Group, Price, Product, parse_groups, parse_prices, parse_products};

/// Where bytes come from. The consumer owns retry, timeouts and politeness.
pub trait Fetch {
    fn get(&self, url: &str) -> Result<Vec<u8>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Groups,
    Products,
    Prices,
    LastUpdated,
}

/// One response as received, handed to [`Landing`] before anything parses it.
#[derive(Debug, Clone)]
pub struct Payload<'a> {
    pub kind: Kind,
    pub category: i64,
    pub group_id: Option<i64>,
    pub url: &'a str,
    /// The day `last-updated.txt` names, when the client has read it.
    pub as_of: Option<&'a str>,
    pub bytes: &'a [u8],
}

/// Where raw bytes are kept.
pub trait Landing {
    fn land(&self, payload: &Payload<'_>) -> Result<()>;
}

/// Everything one category published at one `last-updated.txt` stamp.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub stamp: String,
    pub day: String,
    pub groups: Vec<Group>,
    pub products: Vec<(i64, Vec<Product>)>,
    pub prices: Vec<(i64, Vec<Price>)>,
}

#[derive(Debug, Clone)]
pub enum Sync {
    /// The stamp equals the one already seen; nothing but the stamp was fetched.
    Unchanged {
        stamp: String,
    },
    Fresh(Snapshot),
}

/// `last-updated.txt` is an ISO timestamp, e.g. `2026-10-12T20:05:44+0000`;
/// the day it names is the day its prices describe.
pub fn stamp_day(stamp: &str) -> Option<&str> {
    let d = stamp.trim().get(..10)?;
    let ok = d.bytes().enumerate().all(|(i, b)| {
        if i == 4 || i == 7 {
            b == b'-'
        } else {
            b.is_ascii_digit()
        }
    });
    ok.then_some(d)
}

pub struct TcgcsvClient<'a> {
    fetch: &'a dyn Fetch,
    landing: Option<&'a dyn Landing>,
    base: String,
    category: i64,
}

impl<'a> TcgcsvClient<'a> {
    pub fn new(fetch: &'a dyn Fetch, base: &str, category: i64) -> Self {
        Self {
            fetch,
            landing: None,
            base: base.trim_end_matches('/').to_string(),
            category,
        }
    }

    pub fn landing_in(mut self, landing: &'a dyn Landing) -> Self {
        self.landing = Some(landing);
        self
    }

    pub fn category(&self) -> i64 {
        self.category
    }

    fn stamp_url(&self) -> String {
        format!("{}/last-updated.txt", self.base)
    }

    fn url(&self, tail: &str) -> String {
        format!("{}/tcgplayer/{}{tail}", self.base, self.category)
    }

    fn get(
        &self,
        kind: Kind,
        group_id: Option<i64>,
        url: &str,
        as_of: Option<&str>,
    ) -> Result<Vec<u8>> {
        let bytes = self.fetch.get(url)?;
        if let Some(landing) = self.landing {
            landing.land(&Payload {
                kind,
                category: self.category,
                group_id,
                url,
                as_of,
                bytes: &bytes,
            })?;
        }
        Ok(bytes)
    }

    pub fn fetch_groups(&self) -> Result<Vec<Group>> {
        parse_groups(&self.get(Kind::Groups, None, &self.url("/groups"), None)?)
    }

    pub fn fetch_products(&self, group_id: i64) -> Result<Vec<Product>> {
        let url = self.url(&format!("/{group_id}/products"));
        parse_products(&self.get(Kind::Products, Some(group_id), &url, None)?)
    }

    pub fn fetch_prices(&self, group_id: i64) -> Result<Vec<Price>> {
        let url = self.url(&format!("/{group_id}/prices"));
        parse_prices(&self.get(Kind::Prices, Some(group_id), &url, None)?)
    }

    /// Read the stamp without landing it; [`sync`](Self::sync) lands it last.
    fn read_stamp(&self) -> Result<(String, Vec<u8>, String)> {
        let bytes = self.fetch.get(&self.stamp_url())?;
        let stamp = String::from_utf8_lossy(&bytes).trim().to_string();
        let day = stamp_day(&stamp)
            .ok_or_else(|| Error::Source(format!("last-updated.txt unreadable: {stamp:?}")))?
            .to_string();
        Ok((stamp, bytes, day))
    }

    /// Fetch the whole category, unless `seen` (the stamp of the last completed
    /// sync) is still current, in which case only the stamp is fetched.
    ///
    /// The stamp is landed last, so a sync that dies partway leaves no record that
    /// the stamp was seen and the next one starts over.
    pub fn sync(&self, seen: Option<&str>) -> Result<Sync> {
        let (stamp, stamp_bytes, day) = self.read_stamp()?;
        if seen == Some(stamp.as_str()) {
            return Ok(Sync::Unchanged { stamp });
        }
        let as_of = Some(day.as_str());
        let groups_url = self.url("/groups");
        let groups = parse_groups(&self.get(Kind::Groups, None, &groups_url, as_of)?)?;
        let mut products = Vec::with_capacity(groups.len());
        let mut prices = Vec::with_capacity(groups.len());
        for g in &groups {
            let id = g.group_id;
            let url = self.url(&format!("/{id}/products"));
            products.push((
                id,
                parse_products(&self.get(Kind::Products, Some(id), &url, as_of)?)?,
            ));
            let url = self.url(&format!("/{id}/prices"));
            prices.push((
                id,
                parse_prices(&self.get(Kind::Prices, Some(id), &url, as_of)?)?,
            ));
        }
        if let Some(landing) = self.landing {
            landing.land(&Payload {
                kind: Kind::LastUpdated,
                category: self.category,
                group_id: None,
                url: &self.stamp_url(),
                as_of,
                bytes: &stamp_bytes,
            })?;
        }
        Ok(Sync::Fresh(Snapshot {
            stamp,
            day,
            groups,
            products,
            prices,
        }))
    }
}
