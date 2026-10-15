//! law-no-data-lives-in-code, as a check.
//!
//! Scans `.rs/.py/.ts/.js/.svelte` files for *tables*: a run of at least `min_rows`
//! consecutive lines that are each a data row (a leading string literal followed only
//! by literals, separators and brackets: `("sv1", "scarlet"),`, `"sv1": "x",`,
//! `"sv1" => "x",`, `"sv1",`). Each hit names file, line and where the data belongs.
//!
//! Exempt: `tests/` and fixture directories, test-named files, the data directory.
//! Anything else needs an entry in the allowlist (`path` per line, `#` comments). An
//! entry that no longer matches a table is itself an error, so the list only shrinks.
//! Every dataset file under the data directory needs provenance beside it:
//! `<file>.provenance.md`, `<stem>.provenance.md`, or a `PROVENANCE.md` in its directory.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct Config {
    pub root: PathBuf,
    /// Repo-relative directory remedial datasets live in.
    pub data_dir: String,
    /// Repo-relative allowlist file.
    pub allowlist: String,
    pub min_rows: usize,
}

impl Config {
    pub fn new(root: PathBuf) -> Self {
        Config {
            root,
            data_dir: "data/remedial".into(),
            allowlist: ".datalint-allow".into(),
            min_rows: 5,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

const SOURCE_EXT: &[&str] = &["rs", "py", "ts", "js", "svelte"];
const DATA_EXT: &[&str] = &["csv", "json", "yaml", "yml", "tsv"];
const SKIP_DIRS: &str = ".git target node_modules tests test fixtures __tests__ dist build .venv";

pub fn run(cfg: &Config) -> io::Result<Vec<Problem>> {
    let allow = read_allowlist(&cfg.root.join(&cfg.allowlist))?;
    let mut used = BTreeSet::new();
    let mut problems = Vec::new();
    let mut files = Vec::new();
    walk(&cfg.root, &cfg.root, &cfg.data_dir, &mut files)?;
    files.sort();
    for rel in &files {
        let Ok(text) = fs::read_to_string(cfg.root.join(rel)) else {
            continue;
        };
        let tables = find_tables(&text, cfg.min_rows);
        if tables.is_empty() {
            continue;
        }
        if allow.contains(rel) {
            used.insert(rel.clone());
            continue;
        }
        for (line, rows) in tables {
            problems.push(Problem {
                file: rel.clone(),
                line,
                message: format!(
                    "reference data written as a literal table ({rows} rows); move it to {}/<name>.csv|json|yaml with a .provenance.md beside it and load it from code",
                    cfg.data_dir
                ),
            });
        }
    }
    for entry in &allow {
        if !used.contains(entry) {
            problems.push(Problem {
                file: cfg.allowlist.clone(),
                line: 1,
                message: format!("stale allowlist entry `{entry}` matches no table; delete it (the list only shrinks)"),
            });
        }
    }
    check_provenance(cfg, &mut problems)?;
    Ok(problems)
}

fn read_allowlist(path: &Path) -> io::Result<BTreeSet<String>> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(t
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(e) => Err(e),
    }
}

fn walk(root: &Path, dir: &Path, data_dir: &str, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if entry.file_type()?.is_dir() {
            if SKIP_DIRS.split(' ').any(|d| d == name) || rel == data_dir {
                continue;
            }
            walk(root, &path, data_dir, out)?;
        } else if is_source(&name) {
            out.push(rel);
        }
    }
    Ok(())
}

fn is_source(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    SOURCE_EXT.contains(&ext)
        && !stem.ends_with("_test")
        && !stem.ends_with(".test")
        && !stem.ends_with(".spec")
        && !stem.starts_with("test_")
        && stem != "conftest"
}

/// (1-based first line, row count) of every table in `text`.
fn find_tables(text: &str, min_rows: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut rows = 0;
    let flush = |start: usize, rows: usize, out: &mut Vec<(usize, usize)>| {
        if rows >= min_rows {
            out.push((start, rows));
        }
    };
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") || t.starts_with('#') {
            continue;
        }
        if is_row(t) {
            if rows == 0 {
                start = i + 1;
            }
            rows += 1;
        } else {
            flush(start, rows, &mut out);
            rows = 0;
        }
    }
    flush(start, rows, &mut out);
    out
}

/// A data row: optional opening brackets, a string literal, then only literals,
/// separators and brackets, with at least one separator after the first literal.
fn is_row(t: &str) -> bool {
    let t = t.trim_start_matches(['(', '[', '{', ' ']);
    let mut chars = t.chars().peekable();
    let Some(&q) = chars.peek() else { return false };
    if q != '"' && q != '\'' {
        return false;
    }
    let mut in_str = false;
    let mut quote = q;
    let mut literals = 0;
    let mut separators = 0;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                in_str = true;
                quote = c;
                literals += 1;
            }
            ',' | ':' => separators += 1,
            '=' | '>' => separators += 1,
            '(' | ')' | '[' | ']' | '{' | '}' | ' ' | '\t' | '_' | '-' | '.' | ';' => {}
            '0'..='9' => {}
            '/' if chars.peek() == Some(&'/') => break,
            _ => return false,
        }
    }
    !in_str && literals >= 1 && separators >= 1
}

fn check_provenance(cfg: &Config, problems: &mut Vec<Problem>) -> io::Result<()> {
    let dir = cfg.root.join(&cfg.data_dir);
    let Ok(read) = fs::read_dir(&dir) else {
        return Ok(());
    };
    for entry in read {
        let path = entry?.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some((stem, ext)) = name.rsplit_once('.') else {
            continue;
        };
        if !DATA_EXT.contains(&ext) || stem.ends_with(".provenance") {
            continue;
        }
        let beside = [
            format!("{name}.provenance.md"),
            format!("{stem}.provenance.md"),
            "PROVENANCE.md".into(),
        ];
        if !beside.iter().any(|b| dir.join(b).is_file()) {
            problems.push(Problem {
                file: format!("{}/{name}", cfg.data_dir),
                line: 1,
                message: format!("dataset has no provenance; add {}/{stem}.provenance.md (source, date, how derived)", cfg.data_dir),
            });
        }
    }
    Ok(())
}
