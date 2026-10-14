//! `dumpster-kit-ui lint --tokens <tokens.css> <dir>…` and
//! `dumpster-kit-ui contrast <tokens.css> <pairs.json>`. Exit 1 on any finding, 2 on misuse.

use std::path::PathBuf;
use std::process::ExitCode;

use dumpster_kit_ui::{contrast, lint};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("lint") => {
            let (mut tokens, mut roots) = (Vec::new(), Vec::new());
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "--tokens" {
                    match it.next() {
                        Some(p) => tokens.push(PathBuf::from(p)),
                        None => return usage(),
                    }
                } else {
                    roots.push(PathBuf::from(a));
                }
            }
            if tokens.is_empty() || roots.is_empty() {
                return usage();
            }
            match lint::lint_tree(&roots, &tokens) {
                Ok(v) if v.is_empty() => ExitCode::SUCCESS,
                Ok(v) => {
                    v.iter().for_each(|v| eprintln!("{v}"));
                    eprintln!("{} raw value(s) outside the token layer", v.len());
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("lint: {e}");
                    ExitCode::from(2)
                }
            }
        }
        Some("contrast") if args.len() == 3 => {
            let read = |p: &str| std::fs::read_to_string(p).map_err(|e| eprintln!("{p}: {e}"));
            let (Ok(css), Ok(json)) = (read(&args[1]), read(&args[2])) else {
                return ExitCode::from(2);
            };
            let problems = contrast::audit(&css, &json);
            problems.iter().for_each(|p| eprintln!("{p}"));
            if problems.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: dumpster-kit-ui lint --tokens <tokens.css> <dir>…\n       dumpster-kit-ui contrast <tokens.css> <pairs.json>"
    );
    ExitCode::from(2)
}
