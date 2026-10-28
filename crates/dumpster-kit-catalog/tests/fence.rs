//! The kit never names a game field. Fails on any known game attribute name in this crate.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    "aspects",
    "aspect",
    "supertype",
    "oracle_id",
    "attacks",
    "pokedex",
    "retreat_cost",
    "weakness",
    "resistance",
    "evolves_from",
    "mana_cost",
    "cmc",
    "colour_identity",
    "color_identity",
    "hit_points",
    "arena",
];

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "target") {
                files(&p, out);
            }
        } else if p
            .extension()
            .is_some_and(|x| x == "rs" || x == "toml" || x == "sql")
        {
            out.push(p);
        }
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// (file, line number, word) for every forbidden word, matched as a whole identifier, in any case.
fn scan(root: &Path, skip: &str) -> Vec<(PathBuf, usize, &'static str)> {
    let mut paths = Vec::new();
    files(root, &mut paths);
    let mut hits = Vec::new();
    for p in paths {
        if p.file_name().is_some_and(|n| n == skip) {
            continue;
        }
        for (i, line) in fs::read_to_string(&p).unwrap().lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            let b = lower.as_bytes();
            for w in FORBIDDEN {
                let mut from = 0;
                while let Some(at) = lower[from..].find(w) {
                    let s = from + at;
                    let e = s + w.len();
                    if (s == 0 || !is_word_byte(b[s - 1])) && (e == b.len() || !is_word_byte(b[e]))
                    {
                        hits.push((p.clone(), i + 1, *w));
                    }
                    from = e;
                }
            }
        }
    }
    hits
}

#[test]
fn the_crate_names_no_game_attribute() {
    let hits = scan(Path::new(env!("CARGO_MANIFEST_DIR")), "fence.rs");
    assert!(hits.is_empty(), "game attribute names in the kit: {hits:?}");
}

#[test]
fn the_fence_catches_a_planted_attribute() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("a.rs"),
        "let ok = 1;\nstruct S { Oracle_Id: String }\n",
    )
    .unwrap();
    fs::write(dir.path().join("b.sql"), "CREATE TABLE t (aspects TEXT);").unwrap();
    fs::write(
        dir.path().join("c.rs"),
        "let attacks_made_up = 1; // not a whole word\n",
    )
    .unwrap();
    let hits = scan(dir.path(), "fence.rs");
    let mut words: Vec<&str> = hits.iter().map(|h| h.2).collect();
    words.sort();
    assert_eq!(words, vec!["aspects", "oracle_id"]);
}
