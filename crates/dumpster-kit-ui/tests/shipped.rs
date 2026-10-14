//! The acceptance: what the kit ships passes its own checks.

use dumpster_kit_ui::{CONTRAST_PAIRS_JSON, TOKENS_CSS, assets_dir, contrast, lint, tokens};

#[test]
fn every_shipped_pairing_passes_wcag_aa() {
    let tokens = tokens::parse(TOKENS_CSS);
    let manifest = serde_json::from_str(CONTRAST_PAIRS_JSON).unwrap();
    let measured = contrast::measure(&tokens, &manifest).unwrap();
    assert!(
        measured.len() > 100,
        "the manifest should pair the whole palette, got {}",
        measured.len()
    );
    let below: Vec<_> = measured
        .iter()
        .filter(|m| m.ratio + 1e-9 < m.needs)
        .collect();
    assert!(below.is_empty(), "{below:#?}");
}

#[test]
fn the_token_file_and_manifest_audit_clean() {
    assert_eq!(
        contrast::audit(TOKENS_CSS, CONTRAST_PAIRS_JSON),
        Vec::<String>::new()
    );
}

#[test]
fn no_raw_value_outside_tokens_in_the_shipped_components() {
    let dir = assets_dir();
    let found = lint::lint_tree(&[dir.join("components"), dir.join("tokens.ts")], &[]).unwrap();
    assert!(
        found.is_empty(),
        "{}",
        found
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_token_file_itself_is_the_only_place_raw_values_live() {
    let dir = assets_dir();
    let tokens = dir.join("tokens.css");
    assert!(
        !lint::lint_tree(std::slice::from_ref(&dir), &[])
            .unwrap()
            .is_empty()
    );
    let outside = lint::lint_tree(&[dir], &[tokens]).unwrap();
    assert!(outside.is_empty(), "{outside:#?}");
}

#[test]
fn an_unreadable_root_is_an_error_not_a_clean_result() {
    assert!(lint::lint_tree(&["/nonexistent-dumpster-kit-ui".into()], &[]).is_err());
}
