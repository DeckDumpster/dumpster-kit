//! The conformance corpora are plain data files under `corpus/` in this crate, embedded at compile
//! time so a published crate carries them. A consumer's CI reads them through this crate, so it
//! runs the same files the kit does.

use std::path::PathBuf;

use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Directory holding one subdirectory per corpus, inside this crate's own directory.
pub fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

/// One string and the verdict every enforcer of its rule must reach.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Case {
    pub value: String,
    pub valid: bool,
}

/// One token the Access layer must refuse, by the name the kit's test JWKS server mints it under.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct NegativeJwt {
    pub name: String,
    pub describes: String,
    pub status: u16,
}

#[derive(Deserialize)]
struct File<T> {
    cases: Vec<T>,
}

/// Parse an embedded corpus file. A corpus that cannot be parsed panics: a consumer whose corpus
/// silently loaded as empty would be passing a test that checks nothing.
fn load<T: DeserializeOwned>(name: &str, text: &str) -> Vec<T> {
    let parsed: File<T> = serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(!parsed.cases.is_empty(), "{name} has no cases");
    parsed.cases
}

/// Handles, and whether each is one.
pub fn handle_cases() -> Vec<Case> {
    load(
        "handles/cases.json",
        include_str!("../corpus/handles/cases.json"),
    )
}

/// `database_id`s, and whether each is canonical.
pub fn ulid_cases() -> Vec<Case> {
    load(
        "ulids/cases.json",
        include_str!("../corpus/ulids/cases.json"),
    )
}

/// Emails, and whether each is already in normalised form.
pub fn email_cases() -> Vec<Case> {
    load(
        "emails/cases.json",
        include_str!("../corpus/emails/cases.json"),
    )
}

/// One catalog scenario: a fake game and the steps to script against it. The steps stay as JSON
/// because only the catalog runner interprets them.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CatalogCase {
    pub name: String,
    pub game: String,
    pub steps: Vec<serde_json::Value>,
}

/// Catalog scenarios for the kit's fake games.
pub fn catalog_cases() -> Vec<CatalogCase> {
    load(
        "catalog/cases.json",
        include_str!("../corpus/catalog/cases.json"),
    )
}

/// Tokens that must all be refused.
pub fn negative_jwts() -> Vec<NegativeJwt> {
    load(
        "jwt/negative.json",
        include_str!("../corpus/jwt/negative.json"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_root_exists() {
        assert!(corpus_root().is_dir(), "{:?}", corpus_root());
    }

    #[test]
    fn every_corpus_loads_and_has_both_verdicts() {
        for (name, cases) in [
            ("handles", handle_cases()),
            ("ulids", ulid_cases()),
            ("emails", email_cases()),
        ] {
            assert!(cases.iter().any(|c| c.valid), "{name}: no accepted case");
            assert!(cases.iter().any(|c| !c.valid), "{name}: no rejected case");
        }
        assert!(negative_jwts().iter().all(|j| j.status == 401));
    }

    #[test]
    fn corpus_rows_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in handle_cases() {
            assert!(
                seen.insert(c.value.clone()),
                "duplicate handle {:?}",
                c.value
            );
        }
        let names: Vec<_> = negative_jwts().into_iter().map(|j| j.name).collect();
        let set: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(set.len(), names.len(), "duplicate jwt name");
    }
}
