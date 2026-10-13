//! The conformance corpora are plain data files under `conformance/` at the kit root.
//! A consumer's CI finds them through this crate, so it runs the same files the kit does.

use std::path::PathBuf;

use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Directory holding one subdirectory per corpus.
pub fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
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

/// Read `conformance/<dir>/<file>`. A corpus that cannot be read or parsed panics: a consumer
/// whose corpus silently loaded as empty would be passing a test that checks nothing.
fn load<T: DeserializeOwned>(dir: &str, file: &str) -> Vec<T> {
    let path = corpus_root().join(dir).join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let parsed: File<T> =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(!parsed.cases.is_empty(), "{} has no cases", path.display());
    parsed.cases
}

/// Handles, and whether each is one.
pub fn handle_cases() -> Vec<Case> {
    load("handles", "cases.json")
}

/// `database_id`s, and whether each is canonical.
pub fn ulid_cases() -> Vec<Case> {
    load("ulids", "cases.json")
}

/// Emails, and whether each is already in normalised form.
pub fn email_cases() -> Vec<Case> {
    load("emails", "cases.json")
}

/// Tokens that must all be refused.
pub fn negative_jwts() -> Vec<NegativeJwt> {
    load("jwt", "negative.json")
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
