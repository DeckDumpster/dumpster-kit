//! Two invented games with different [`FinishPlacement`]s, for testing the kit with no real game
//! in sight. Their payload is `\t`-separated lines: `thing\t<id>\t<name>\t<size>` and
//! `variant\t<thing id>\t<label>`.

use dumpster_kit_tcgcsv::{Price, Product};
use rusqlite::{Transaction, params};

use crate::error::{Error, Result};
use crate::game::{ApplyReport, FinishPlacement, Game, Match, Source};
use crate::search::{SearchTerm, TermKind, Vocabulary};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FakePayload {
    pub things: Vec<(String, String, i64)>,
    pub variants: Vec<(String, String)>,
}

fn parse(source: &str, raw: &[u8]) -> Result<FakePayload> {
    let bad = |m: String| Error::Parse {
        source_name: source.into(),
        message: m,
    };
    let text = std::str::from_utf8(raw).map_err(|e| bad(e.to_string()))?;
    let mut p = FakePayload::default();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["thing", id, name, size] => p.things.push((
                id.to_string(),
                name.to_string(),
                size.parse()
                    .map_err(|_| bad(format!("line {}: size", n + 1)))?,
            )),
            ["variant", id, label] => p.variants.push((id.to_string(), label.to_string())),
            _ => return Err(bad(format!("line {}: unrecognised", n + 1))),
        }
    }
    Ok(p)
}

fn apply(
    tx: &Transaction<'_>,
    p: &FakePayload,
    things: &str,
    variants: &str,
) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();
    for (id, name, size) in &p.things {
        let exists: bool = tx.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM {things} WHERE thing_id = ?1)"),
            [id],
            |r| r.get(0),
        )?;
        let n = tx.execute(
            &format!(
                "INSERT INTO {things} (thing_id, name, size) VALUES (?1, ?2, ?3)
                 ON CONFLICT (thing_id) DO UPDATE SET name = ?2, size = ?3
                 WHERE name IS NOT ?2 OR size IS NOT ?3
                 "
            ),
            params![id, name, size],
        )?;
        if n > 0 {
            if exists {
                report.updated(things, 1)
            } else {
                report.inserted(things, 1)
            }
        }
    }
    for (id, label) in &p.variants {
        let n = tx.execute(
            &format!("INSERT OR IGNORE INTO {variants} (thing_id, label) VALUES (?1, ?2)"),
            params![id, label],
        )?;
        report.inserted(variants, n as u64);
    }
    Ok(report)
}

/// Finish lives on the variant row.
pub struct FakeOnPrinting;
/// Finish is chosen per owned copy; variants carry none.
pub struct FakeOnCopy;

impl Game for FakeOnPrinting {
    const ID: &'static str = "fakeprint";
    const SCHEMA_VERSION: u32 = 1;
    const SCHEMA: &'static str = "
        CREATE TABLE fp_things (thing_id TEXT PRIMARY KEY, name TEXT NOT NULL, size INTEGER NOT NULL) STRICT;
        CREATE TABLE fp_variants (thing_id TEXT NOT NULL REFERENCES fp_things (thing_id), label TEXT NOT NULL,
            PRIMARY KEY (thing_id, label)) STRICT;";
    const FINISH: FinishPlacement = FinishPlacement::OnPrinting;
    type Payload = FakePayload;

    fn sources() -> Vec<Source> {
        vec![Source {
            name: "feed".into(),
            location: "fake://feed".into(),
        }]
    }
    fn parse(source: &str, raw: &[u8]) -> Result<FakePayload> {
        parse(source, raw)
    }
    fn apply(tx: &Transaction<'_>, p: &FakePayload) -> Result<ApplyReport> {
        apply(tx, p, "fp_things", "fp_variants")
    }
    fn crosswalk(product: &Product, _price: Option<&Price>) -> Match {
        match &product.number {
            Some(n) => Match::Printing {
                printing_id: format!("fp-{n}"),
                finish: None,
            },
            None => Match::Unmatched {
                reason: "no number".into(),
            },
        }
    }
    fn search_terms() -> Vocabulary {
        Vocabulary::new(
            "name",
            vec![SearchTerm::new("size", TermKind::Number, "size", "size in units").alias("s")],
        )
        .expect("valid vocabulary")
    }
    fn tcgcsv_category() -> i64 {
        9001
    }
}

impl Game for FakeOnCopy {
    const ID: &'static str = "fakecopy";
    const SCHEMA_VERSION: u32 = 1;
    const SCHEMA: &'static str = "
        CREATE TABLE fc_things (thing_id TEXT PRIMARY KEY, name TEXT NOT NULL, size INTEGER NOT NULL) STRICT;
        CREATE TABLE fc_variants (thing_id TEXT NOT NULL REFERENCES fc_things (thing_id), label TEXT NOT NULL,
            PRIMARY KEY (thing_id, label)) STRICT;";
    const FINISH: FinishPlacement = FinishPlacement::OnCopy;
    type Payload = FakePayload;

    fn sources() -> Vec<Source> {
        vec![
            Source {
                name: "feed".into(),
                location: "fake://feed".into(),
            },
            Source {
                name: "extras".into(),
                location: "fake://extras".into(),
            },
        ]
    }
    fn parse(source: &str, raw: &[u8]) -> Result<FakePayload> {
        parse(source, raw)
    }
    fn apply(tx: &Transaction<'_>, p: &FakePayload) -> Result<ApplyReport> {
        apply(tx, p, "fc_things", "fc_variants")
    }
    fn crosswalk(product: &Product, price: Option<&Price>) -> Match {
        match &product.number {
            Some(n) => Match::Printing {
                printing_id: format!("fc-{n}"),
                finish: Some(
                    price
                        .and_then(|p| p.sub_type_name.clone())
                        .unwrap_or_default(),
                ),
            },
            None => Match::Sealed {
                sealed_id: format!("fc-sealed-{}", product.product_id),
            },
        }
    }
    fn search_terms() -> Vocabulary {
        Vocabulary::new("name", vec![]).expect("valid vocabulary")
    }
    fn tcgcsv_category() -> i64 {
        9002
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{SearchError, compile};
    use rusqlite::Connection;

    #[test]
    fn fake_terms_compile_and_query() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(FakeOnPrinting::SCHEMA).unwrap();
        db.execute_batch(
            "INSERT INTO fp_things VALUES ('a','Blue Gizmo',3),('b','Red Gizmo',5),('c','Wobble',9);",
        )
        .unwrap();
        let c = compile("gizmo size>=4", &FakeOnPrinting::search_terms()).unwrap();
        let mut st = db
            .prepare(&format!("SELECT thing_id FROM fp_things WHERE {}", c.sql))
            .unwrap();
        let ids: Vec<String> = st
            .query_map(c.params().as_slice(), |r| r.get(0))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert_eq!(ids, ["b"]);
    }

    #[test]
    fn unknown_term_names_the_known_terms() {
        let e = compile("hp>3", &FakeOnPrinting::search_terms()).unwrap_err();
        assert!(matches!(&e, SearchError::UnknownTerm { known, .. } if known.contains("size")));
    }
}
