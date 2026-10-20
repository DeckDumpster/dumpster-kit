use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

/// One entry of a site's `search_keywords.json`. Other fields in the file are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct Keyword {
    pub canonical: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub operators: Vec<String>,
    #[serde(default)]
    pub help: String,
}

/// The site's search vocabulary, read from the same file its own search help reads, so the
/// tool description and `query_help` cannot drift from the UI.
#[derive(Debug, Clone)]
pub struct Keywords(pub Vec<Keyword>);

impl Keywords {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text)
            .map(Keywords)
            .map_err(|e| e.to_string())
    }

    /// Operators in order of first appearance.
    pub fn operators(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for op in self.0.iter().flat_map(|k| &k.operators) {
            if !out.contains(&op.as_str()) {
                out.push(op);
            }
        }
        out
    }

    /// The `search_cards` description: the grammar summary, the first `top` terms (file order is
    /// usefulness order) and the site's worked examples.
    pub fn tool_description(&self, intro: &str, top: usize, examples: &[&str]) -> String {
        let mut s = format!(
            "{intro}\n\nOperators: {}\n\nMost useful terms:\n",
            self.operators().join(" ")
        );
        for k in self.0.iter().take(top) {
            s.push_str(&format!("- {}", k.canonical));
            let aliases: Vec<&str> = k
                .aliases
                .iter()
                .filter(|a| **a != k.canonical)
                .map(String::as_str)
                .collect();
            if !aliases.is_empty() {
                s.push_str(&format!(" (also {})", aliases.join(", ")));
            }
            s.push_str(&format!(": {}\n", k.help));
        }
        s.push_str("\nExamples:\n");
        for e in examples {
            s.push_str(&format!("- {e}\n"));
        }
        s.push_str("\nCall query_help for every term.");
        s
    }

    /// The `query_help` result: every term with its aliases, operators and help.
    pub fn help_value(&self) -> Value {
        json!({
            "operators": self.operators(),
            "terms": self.0.iter().map(|k| json!({
                "term": k.canonical, "aliases": k.aliases, "operators": k.operators, "help": k.help,
            })).collect::<Vec<_>>(),
        })
    }

    /// Up to three valid names nearest to `unknown` (aliases included).
    pub fn nearest(&self, unknown: &str) -> Vec<String> {
        let unknown = unknown.to_lowercase();
        let mut names: Vec<(usize, &str)> = self
            .0
            .iter()
            .flat_map(|k| std::iter::once(&k.canonical).chain(&k.aliases))
            .map(|n| (distance(&unknown, &n.to_lowercase()), n.as_str()))
            .filter(|(d, n)| {
                *d <= 2.max(n.len() / 3) || n.starts_with(&unknown) || unknown.starts_with(n)
            })
            .collect();
        names.sort();
        names.dedup_by_key(|(_, n)| *n);
        names
            .into_iter()
            .take(3)
            .map(|(_, n)| n.to_string())
            .collect()
    }

    /// `unknown term 'aspect:', did you mean 'a:'?`, or the plain refusal when nothing is near.
    pub fn unknown_term_message(&self, unknown: &str) -> String {
        let near = self.nearest(unknown);
        if near.is_empty() {
            return format!("unknown term '{unknown}:'; call query_help for the valid terms");
        }
        let list = near
            .iter()
            .map(|n| format!("'{n}:'"))
            .collect::<Vec<_>>()
            .join(" or ");
        format!("unknown term '{unknown}:', did you mean {list}?")
    }
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != *cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw() -> Keywords {
        Keywords::parse(
            r#"[{"canonical":"aspect","aliases":["a"],"operators":[":","="],"help":"Aspect"},
                {"canonical":"cost","aliases":["c"],"operators":[":","<="],"help":"Cost"}]"#,
        )
        .unwrap()
    }

    #[test]
    fn names_the_nearest_term() {
        assert_eq!(
            kw().unknown_term_message("aspec"),
            "unknown term 'aspec:', did you mean 'aspect:' or 'a:'?"
        );
        assert!(kw().unknown_term_message("zzzzzzzz").contains("query_help"));
    }

    #[test]
    fn description_lists_operators_and_terms() {
        let d = kw().tool_description("Search.", 10, &["a:x"]);
        assert!(
            d.contains("Operators: : = <=")
                && d.contains("- cost (also c): Cost")
                && d.contains("- a:x")
        );
    }
}
