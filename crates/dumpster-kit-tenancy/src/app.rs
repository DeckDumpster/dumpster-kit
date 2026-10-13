//! The one thing that differs between sites: their name.
//!
//! Everything an application used to spell as `PKDUMP_*` or `DUMPSTAR_*` — the data-directory
//! variable, the active-tenant variable, the three Access variables, the default directory and
//! the CLI an operator is told to run — derives from one prefix.

use crate::error::{Result, TenancyError};

/// An application's identity: `PKDUMP`, `DUMPSTAR`, and so on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPrefix(String);

impl AppPrefix {
    /// `prefix` is upper-case ASCII letters, digits and `_`, starting with a letter. It becomes
    /// part of environment variable names, so anything else is refused here rather than
    /// producing a variable nobody can set.
    pub fn new(prefix: &str) -> Result<Self> {
        let ok = !prefix.is_empty()
            && prefix.as_bytes()[0].is_ascii_uppercase()
            && prefix
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if !ok {
            return Err(TenancyError::Env(format!(
                "app prefix {prefix:?} must be upper-case ASCII letters, digits and `_`, \
                 starting with a letter"
            )));
        }
        Ok(AppPrefix(prefix.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `<PREFIX>_<suffix>`.
    pub fn var(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.0)
    }

    /// The command name an operator runs: the prefix, lower-cased.
    pub fn cli(&self) -> String {
        self.0.to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_derive_from_the_prefix() {
        let app = AppPrefix::new("PKDUMP").unwrap();
        assert_eq!(app.var("HOME"), "PKDUMP_HOME");
        assert_eq!(app.var("ACCESS_AUD"), "PKDUMP_ACCESS_AUD");
        assert_eq!(app.cli(), "pkdump");
    }

    #[test]
    fn a_prefix_that_cannot_be_a_variable_is_refused() {
        for bad in ["", "pkdump", "1ABC", "A-B", "A B", "A=B", "ÄB"] {
            assert!(AppPrefix::new(bad).is_err(), "{bad:?}");
        }
    }
}
