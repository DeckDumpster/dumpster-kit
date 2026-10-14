//! Every rule seen to fail: a fixture that breaks it, and a clean twin that does not.

use std::path::Path;

use dumpster_kit_ui::contrast::audit;
use dumpster_kit_ui::lint::{Kind, lint_source};
use dumpster_kit_ui::{CONTRAST_PAIRS_JSON, TOKENS_CSS};

fn kinds(src: &str) -> Vec<Kind> {
    lint_source(Path::new("x.svelte"), src)
        .into_iter()
        .map(|v| v.kind)
        .collect()
}

#[test]
fn raw_colours_fail() {
    for src in [
        "a { color: #fff; }",
        "a { color: #E94560; }",
        "a { background: rgba(0, 0, 0, 0.4); }",
        "a { color: hsl(10 50% 50%); }",
        "a { border: 1px solid white; }",
        "a { background-color: red }",
        "<path fill=\"#abc\" />",
        "<path stroke=\"black\" />",
        "const s = { color: 'tomato' };",
    ] {
        assert!(kinds(src).contains(&Kind::Colour), "{src}");
    }
}

#[test]
fn raw_space_type_radius_and_elevation_fail() {
    assert_eq!(kinds("a { padding: 0.4rem 0.6rem; }"), [Kind::Space]);
    assert_eq!(kinds("a { margin-inline-start: 6px }"), [Kind::Space]);
    assert_eq!(kinds("a { gap: 8px; }"), [Kind::Space]);
    assert_eq!(kinds("a { font-size: 13px; }"), [Kind::Type]);
    assert_eq!(kinds("a { border-top-left-radius: 4px; }"), [Kind::Radius]);
    assert_eq!(kinds("a { border-radius: 50%; }"), [Kind::Radius]);
    assert_eq!(
        kinds("a { box-shadow: 0 1px 2px var(--x); }"),
        [Kind::Elevation]
    );
    assert_eq!(
        kinds("a { padding: var(--space-2, 8px); }"),
        [Kind::Space],
        "a fallback is a raw value"
    );
    assert_eq!(
        kinds("a { padding: calc(var(--space-2) + 2px); }"),
        [Kind::Space]
    );
}

#[test]
fn reaching_into_the_reference_layer_fails() {
    assert_eq!(
        kinds("a { color: var(--pd-crimson-500); }"),
        [Kind::ReferenceLayer]
    );
}

#[test]
fn token_use_comments_and_non_decisions_pass() {
    for src in [
        "a { color: var(--color-text); background: var(--color-surface-panel); }",
        "a { padding: var(--space-2) var(--space-4); gap: var(--space-1); }",
        "a { padding: 0; margin: 0 auto; border-radius: var(--radius-md); }",
        "a { padding: calc(var(--space-4) * 2); font-size: var(--text-sm); }",
        "a { border: var(--space-px) solid var(--color-border); color: transparent; fill: currentColor; }",
        "a { --card-padding: 6px; }",
        "/* color: #fff; padding: 4px; */ a { color: var(--color-text); }",
        "<!-- background: red -->",
        "// color: #fff\n * padding: 4px",
        "<a href=\"#section\">x</a><a href=\"&#169;\">y</a>",
        "a { background-image: url(red-500.png); }",
    ] {
        assert_eq!(kinds(src), vec![], "{src}");
    }
}

#[test]
fn violations_carry_the_line() {
    let v = lint_source(
        Path::new("a.css"),
        "a {\n  color: var(--color-text);\n  padding: 4px;\n}\n",
    );
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].line, 3);
}

fn with(token: &str, value: &str) -> String {
    TOKENS_CSS
        .lines()
        .map(|l| {
            match l
                .trim_start()
                .strip_prefix(token)
                .and_then(|r| r.strip_prefix(':'))
            {
                Some(_) => format!("\t{token}: {value};"),
                None => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_pairing_below_aa_fails() {
    // #888 on #16213e is 4.48:1 — the failure the palette shipped before tokens.
    let bad = with("--pd-slate-300", "#888888");
    let problems = audit(&bad, CONTRAST_PAIRS_JSON);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("--color-text-subtle") && p.contains("needs 4.5")),
        "{problems:?}"
    );
}

#[test]
fn a_literal_in_the_semantic_layer_fails() {
    let bad = with("--color-accent", "#e94560");
    assert!(
        audit(&bad, CONTRAST_PAIRS_JSON)
            .iter()
            .any(|p| p.contains("--color-accent") && p.contains("alias"))
    );
}

#[test]
fn a_role_with_no_pairing_fails() {
    let bad = TOKENS_CSS.replace(
        ":root,",
        ":root {\n\t--color-brand-new: var(--pd-ink-800);\n}\n:root,",
    );
    assert!(
        audit(&bad, CONTRAST_PAIRS_JSON)
            .iter()
            .any(|p| p.contains("--color-brand-new"))
    );
}

#[test]
fn a_non_monotonic_ramp_fails() {
    let bad = with("--pd-ink-500", "#ffffff");
    assert!(
        audit(&bad, CONTRAST_PAIRS_JSON)
            .iter()
            .any(|p| p.contains("must be lighter than"))
    );
}

#[test]
fn an_unknown_token_in_a_pairing_fails() {
    let bad = CONTRAST_PAIRS_JSON.replacen("\"--color-text\"", "\"--color-nope\"", 1);
    assert!(!audit(TOKENS_CSS, &bad).is_empty());
}

#[test]
fn unbalanced_parens_do_not_panic() {
    assert_eq!(kinds("const v = token( ;\nlet x = a(b(c);"), vec![]);
    assert_eq!(kinds("a { padding: var(--space-2 ; }"), vec![]);
}
