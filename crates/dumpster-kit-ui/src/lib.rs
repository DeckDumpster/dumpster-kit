//! The kit's design tokens (`assets/tokens.css`), the Svelte primitives (`assets/components/`) and
//! the two checks that make them binding.
//!
//! * [`lint`] fails on a raw colour, space, type, radius or elevation value anywhere outside the
//!   token file, and on a reach into the reference layer (`--pd-*`).
//! * [`contrast`] audits the token file against the pairing manifest: every pairing a site ships
//!   must clear WCAG AA, and the two-layer split must hold.
//!
//! A consumer runs both from its own CI (`dumpster-kit-ui lint --tokens <file> <dir>…`), so a raw
//! value outside tokens fails the consumer's build, not only the kit's.

pub mod contrast;
pub mod lint;
pub mod tokens;

use std::path::PathBuf;

/// The shipped token file.
pub const TOKENS_CSS: &str = include_str!("../assets/tokens.css");
/// Every foreground/background pairing the shipped token file intends to paint.
pub const CONTRAST_PAIRS_JSON: &str = include_str!("../assets/contrast-pairs.json");

/// Where `assets/` lives in this checkout (tokens, manifest, components, primitive tests).
pub fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}
