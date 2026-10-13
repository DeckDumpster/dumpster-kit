//! The conformance corpora are plain data files under `conformance/` at the kit root.
//! A consumer's CI finds them through this crate, so it runs the same files the kit does.

use std::path::PathBuf;

/// Directory holding one subdirectory per corpus.
pub fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_root_exists() {
        assert!(corpus_root().is_dir(), "{:?}", corpus_root());
    }
}
