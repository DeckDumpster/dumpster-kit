use std::fs;
use std::path::Path;

use dumpster_kit_datalint::{Config, run};

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

const MAP_RS: &str = "pub fn cats() -> Vec<(&'static str, &'static str)> {\n    vec![\n        (\"sv1\", \"modern\"),\n        (\"sv2\", \"modern\"),\n        (\"swsh1\", \"sword\"),\n        (\"swsh2\", \"sword\"),\n        (\"sm1\", \"sun\"),\n        (\"sm2\", \"sun\"),\n    ]\n}\n";

#[test]
fn map_in_code_fails_naming_file_and_line() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "src/sets.rs", MAP_RS);
    let problems = run(&Config::new(t.path().into())).unwrap();
    assert_eq!(problems.len(), 1);
    assert_eq!(
        (problems[0].file.as_str(), problems[0].line),
        ("src/sets.rs", 3)
    );
    assert!(problems[0].message.contains("data/remedial"));
}

#[test]
fn python_dict_and_ts_map_fail() {
    let t = tempfile::tempdir().unwrap();
    let rows = |f: &str| {
        (0..6)
            .map(|i| f.replace('N', &i.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    write(
        t.path(),
        "a.py",
        &format!("X = {{\n{}\n}}\n", rows("    \"setN\": \"cat\",")),
    );
    write(
        t.path(),
        "b.ts",
        &format!("const x = {{\n{}\n}};\n", rows("  'setN': 'cat',")),
    );
    assert_eq!(run(&Config::new(t.path().into())).unwrap().len(), 2);
}

#[test]
fn csv_loaded_by_code_passes() {
    let t = tempfile::tempdir().unwrap();
    write(
        t.path(),
        "src/sets.rs",
        "pub fn cats() -> &'static str {\n    include_str!(\"../data/remedial/set-categories.csv\")\n}\n",
    );
    write(
        t.path(),
        "data/remedial/set-categories.csv",
        "sv1,modern\nsv2,modern\n",
    );
    write(
        t.path(),
        "data/remedial/set-categories.provenance.md",
        "hand-built 2026-10-15 from tcgcsv\n",
    );
    assert!(run(&Config::new(t.path().into())).unwrap().is_empty());
}

#[test]
fn dataset_without_provenance_fails() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "data/remedial/set-categories.csv", "sv1,modern\n");
    let problems = run(&Config::new(t.path().into())).unwrap();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("provenance"));
}

#[test]
fn tests_dir_and_short_lists_are_exempt() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "tests/sets.rs", MAP_RS);
    write(
        t.path(),
        "src/few.rs",
        "const A: &[(&str, &str)] = &[\n    (\"a\", \"b\"),\n    (\"c\", \"d\"),\n];\n",
    );
    assert!(run(&Config::new(t.path().into())).unwrap().is_empty());
}

#[test]
fn allowlist_covers_then_goes_stale() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "src/sets.rs", MAP_RS);
    write(t.path(), ".datalint-allow", "src/sets.rs # reviewed\n");
    assert!(run(&Config::new(t.path().into())).unwrap().is_empty());
    write(t.path(), "src/sets.rs", "fn main() {}\n");
    let problems = run(&Config::new(t.path().into())).unwrap();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("stale"));
}
