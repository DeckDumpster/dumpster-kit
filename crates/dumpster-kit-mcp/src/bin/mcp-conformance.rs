//! `mcp-conformance --url U --keywords F --query Q [--authorization H]`: runs the suite against
//! a running site. Exits non-zero on any failed check.

use dumpster_kit_mcp::conformance::{Config, run};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let (mut url, mut keywords, mut query, mut authorization) = (None, None, None, None);
    while let Some(flag) = args.next() {
        let value = args.next();
        match flag.as_str() {
            "--url" => url = value,
            "--keywords" => keywords = value,
            "--query" => query = value,
            "--authorization" => authorization = value,
            _ => {}
        }
    }
    let (Some(url), Some(keywords), Some(query)) = (url, keywords, query) else {
        eprintln!(
            "usage: mcp-conformance --url URL --keywords search_keywords.json --query Q [--authorization H]"
        );
        std::process::exit(2);
    };
    let report = run(&Config {
        url,
        authorization,
        keywords_path: keywords.into(),
        paging_query: query,
    })
    .await;
    print!("{report}");
    std::process::exit(if report.passed() { 0 } else { 1 });
}
