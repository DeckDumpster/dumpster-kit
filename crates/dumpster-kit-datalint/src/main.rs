use std::path::PathBuf;
use std::process::ExitCode;

use dumpster_kit_datalint::{Config, run};

const USAGE: &str =
    "usage: datalint [--root DIR] [--data-dir REL] [--allowlist REL] [--min-rows N]";

fn main() -> ExitCode {
    let mut cfg = Config::new(PathBuf::from("."));
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let Some(value) = args.next() else {
            eprintln!("{flag} needs a value\n{USAGE}");
            return ExitCode::from(2);
        };
        match flag.as_str() {
            "--root" => cfg.root = PathBuf::from(value),
            "--data-dir" => cfg.data_dir = value,
            "--allowlist" => cfg.allowlist = value,
            "--min-rows" => match value.parse() {
                Ok(n) => cfg.min_rows = n,
                Err(_) => {
                    eprintln!("--min-rows takes a number\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("unknown argument {flag}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    match run(&cfg) {
        Ok(problems) if problems.is_empty() => ExitCode::SUCCESS,
        Ok(problems) => {
            for p in &problems {
                eprintln!("{p}");
            }
            eprintln!(
                "datalint: {} problem(s) (law-no-data-lives-in-code)",
                problems.len()
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("datalint: {e}");
            ExitCode::from(2)
        }
    }
}
