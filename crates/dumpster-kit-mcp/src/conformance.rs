//! The conformance suite. A site points it at its own running `/mcp` URL with a seeded fixture
//! tenant; it checks the v1 contract and prints one line per check.
//!
//! Contract result shapes the suite relies on: `search_cards` returns `{"results": [...],
//! "next_cursor": <string|null>}` and accepts `q`, `cursor` and `limit`; `query_help` returns the
//! text of every term, alias, operator and help string of the keyword file.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::{Keywords, V1_TOOLS};

pub struct Config {
    /// The full MCP endpoint, e.g. `https://dumpstar.example/mcp`.
    pub url: String,
    /// Value of the `Authorization` header for the fixture tenant, e.g. `Bearer …`.
    pub authorization: Option<String>,
    /// The site's `data/search_keywords.json`.
    pub keywords_path: PathBuf,
    /// A query matching at least two cards of the fixture tenant's catalog.
    pub paging_query: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub checks: Vec<(String, Result<(), String>)>,
}

impl Report {
    pub fn failures(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{n}: {e}")))
            .collect()
    }
    pub fn passed(&self) -> bool {
        self.failures().is_empty()
    }
    fn record(&mut self, name: &str, r: Result<(), String>) {
        self.checks.push((name.to_string(), r));
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (n, r) in &self.checks {
            match r {
                Ok(()) => writeln!(f, "ok   {n}")?,
                Err(e) => writeln!(f, "FAIL {n}: {e}")?,
            }
        }
        Ok(())
    }
}

struct Client<'a> {
    http: reqwest::Client,
    cfg: &'a Config,
    next_id: u64,
}

impl Client<'_> {
    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let mut req = self.http.post(&self.cfg.url).json(
            &json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}),
        );
        if let Some(a) = &self.cfg.authorization {
            req = req.header("Authorization", a);
        }
        let resp = req.send().await.map_err(|e| format!("{method}: {e}"))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("{method}: HTTP {status}, body is not JSON: {e}"))?;
        if let Some(e) = body.get("error") {
            return Err(format!("{method}: {e}"));
        }
        body.get("result")
            .cloned()
            .ok_or_else(|| format!("{method}: HTTP {status}, no result"))
    }

    /// The structured result of a tool call; an `isError` result is an `Err` with its text.
    async fn call(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let r = self
            .rpc("tools/call", json!({"name": name, "arguments": args}))
            .await?;
        let text = r["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if r["isError"].as_bool() == Some(true) {
            return Err(text);
        }
        r.get("structuredContent")
            .cloned()
            .or_else(|| serde_json::from_str(&text).ok())
            .ok_or_else(|| format!("{name}: result is not JSON"))
    }
}

pub async fn run(cfg: &Config) -> Report {
    let mut rep = Report::default();
    let mut c = Client {
        http: reqwest::Client::new(),
        cfg,
        next_id: 0,
    };

    let init = c.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "conformance", "version": "0"}})).await;
    rep.record(
        "initialize advertises tools",
        init.and_then(|r| {
            if r["capabilities"]["tools"].is_object() && r["protocolVersion"].is_string() {
                Ok(())
            } else {
                Err(format!("unexpected result {r}"))
            }
        }),
    );

    let list = c.rpc("tools/list", json!({})).await;
    let tools: Vec<Value> = list
        .as_ref()
        .ok()
        .and_then(|r| r["tools"].as_array().cloned())
        .unwrap_or_default();
    rep.record("tools/list answers", list.map(|_| ()));
    for name in V1_TOOLS {
        let found = tools.iter().find(|t| t["name"] == *name);
        rep.record(
            &format!("tool {name} exists with a schema"),
            match found {
                None => Err("missing from tools/list".into()),
                Some(t) if t["description"].as_str().is_none_or(str::is_empty) => {
                    Err("no description".into())
                }
                Some(t) if t["inputSchema"]["type"] != "object" => {
                    Err("inputSchema is not an object schema".into())
                }
                Some(_) => Ok(()),
            },
        );
    }
    let search = tools.iter().find(|t| t["name"] == "search_cards");
    rep.record(
        "search_cards takes q and cursor, q required",
        match search {
            None => Err("missing".into()),
            Some(t) => {
                let props = &t["inputSchema"]["properties"];
                let required = t["inputSchema"]["required"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|v| v == "q"));
                if props["q"].is_object() && props["cursor"].is_object() && required {
                    Ok(())
                } else {
                    Err("schema lacks q (required) or cursor".into())
                }
            }
        },
    );

    let keywords = Keywords::load(&cfg.keywords_path);
    rep.record(
        "search_cards description generated from the keyword file",
        match (&keywords, search) {
            (Err(e), _) => Err(e.clone()),
            (_, None) => Err("search_cards missing".into()),
            (Ok(k), Some(t)) => {
                let d = t["description"].as_str().unwrap_or_default();
                let mut missing: Vec<&str> = k
                    .operators()
                    .into_iter()
                    .filter(|o| !d.contains(o))
                    .collect();
                missing.extend(
                    k.0.iter()
                        .take(10)
                        .map(|e| e.canonical.as_str())
                        .filter(|n| !d.contains(n)),
                );
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(format!("description lacks {missing:?}"))
                }
            }
        },
    );

    let help = c.call("query_help", json!({})).await;
    rep.record(
        "query_help generated from the keyword file",
        match (&keywords, help) {
            (Err(e), _) => Err(e.clone()),
            (_, Err(e)) => Err(e),
            (Ok(k), Ok(h)) => {
                let text = h.to_string();
                let missing: Vec<&str> =
                    k.0.iter()
                        .flat_map(|e| {
                            std::iter::once(&e.canonical)
                                .chain(&e.aliases)
                                .chain(&e.operators)
                                .chain(std::iter::once(&e.help))
                        })
                        .filter(|s| !s.is_empty())
                        // JSON escaping may differ from the file's, so compare escaped forms.
                        .filter(|s| {
                            !text.contains(
                                &Value::String((*s).clone())
                                    .to_string()
                                    .trim_matches('"')
                                    .to_string(),
                            )
                        })
                        .map(String::as_str)
                        .collect();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(format!("query_help lacks {missing:?}"))
                }
            }
        },
    );

    let paging = async {
        let p1 = c
            .call("search_cards", json!({"q": cfg.paging_query, "limit": 1}))
            .await?;
        let cursor = p1["next_cursor"]
            .as_str()
            .ok_or("page 1 has no next_cursor; the fixture query must match at least two cards")?
            .to_string();
        let p2 = c
            .call(
                "search_cards",
                json!({"q": cfg.paging_query, "limit": 1, "cursor": cursor}),
            )
            .await?;
        if p2["results"].as_array().is_none_or(Vec::is_empty) {
            return Err("page 2 is empty".to_string());
        }
        if p1["results"] == p2["results"] {
            return Err("the cursor was ignored: page 2 equals page 1".to_string());
        }
        Ok(())
    };
    rep.record("search_cards passes the cursor through", paging.await);

    let unknown = c
        .rpc(
            "tools/call",
            json!({"name": "no_such_tool", "arguments": {}}),
        )
        .await;
    rep.record(
        "unknown tool is refused",
        if unknown.is_err() {
            Ok(())
        } else {
            Err("accepted".into())
        },
    );
    rep
}
