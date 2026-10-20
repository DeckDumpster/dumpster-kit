use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use dumpster_kit_mcp::conformance::{Config, run};
use dumpster_kit_mcp::{
    ContextResolver, Keywords, Limits, McpAuthError, ResolveFuture, ToolContext, ToolDef,
    ToolError, ToolProvider, V1_TOOLS, router,
};
use serde_json::{Value, json};

fn keywords_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search_keywords.json")
}

struct Fixture {
    keywords: Keywords,
    broken_help: bool,
}

impl ToolProvider for Fixture {
    fn tools(&self) -> Vec<ToolDef> {
        let k = &self.keywords;
        V1_TOOLS
            .iter()
            .map(|n| {
                if *n == "search_cards" {
                    ToolDef {
                        name: n.to_string(),
                        description: k.tool_description("Search the card catalog.", 10, &["a:vigilance t:unit cost<=3"]),
                        input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}, "cursor": {"type": "string"}, "limit": {"type": "integer"}}, "required": ["q"]}),
                    }
                } else {
                    ToolDef { name: n.to_string(), description: format!("{n} (fixture)"), input_schema: json!({"type": "object", "properties": {}}) }
                }
            })
            .collect()
    }

    fn call(&self, ctx: &ToolContext, name: &str, args: Value) -> Result<Value, ToolError> {
        assert_eq!(ctx.tenant, "fixture");
        match name {
            "query_help" if self.broken_help => Ok(json!({"terms": []})),
            "query_help" => Ok(self.keywords.help_value()),
            "search_cards" => {
                let start: usize = args
                    .get("cursor")
                    .and_then(Value::as_str)
                    .map_or(0, |c| c.parse().unwrap());
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(2) as usize;
                let results: Vec<usize> = (start..(start + limit).min(5)).collect();
                let next = (start + limit < 5).then(|| (start + limit).to_string());
                Ok(json!({"results": results, "next_cursor": next}))
            }
            "get_card" => Ok(json!({"blob": "x".repeat(2000)})),
            "get_deck" => Err(ToolError::Failed("no such deck".into())),
            _ => Ok(json!({})),
        }
    }
}

struct Token;
impl ContextResolver for Token {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap) -> ResolveFuture<'a> {
        Box::pin(async move {
            match headers.get("authorization").and_then(|v| v.to_str().ok()) {
                Some("Bearer ok") => Ok(ToolContext {
                    tenant: "fixture".into(),
                    grant: "g1".into(),
                }),
                _ => Err(McpAuthError::unauthorized(
                    "sign in",
                    Some("Bearer resource_metadata=\"/.well-known/x\"".into()),
                )),
            }
        })
    }
}

async fn serve(broken_help: bool, limits: Limits) -> String {
    let provider = Fixture {
        keywords: Keywords::load(&keywords_path()).unwrap(),
        broken_help,
    };
    let app = axum::Router::new().nest(
        "/mcp",
        router("fixture", Arc::new(provider), Arc::new(Token), limits),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn cfg(url: String) -> Config {
    Config {
        url,
        authorization: Some("Bearer ok".into()),
        keywords_path: keywords_path(),
        paging_query: "t:unit".into(),
    }
}

async fn rpc(url: &str, auth: Option<&str>, body: Value) -> (u16, Value, Option<String>) {
    let mut req = reqwest::Client::new().post(url).json(&body);
    if let Some(a) = auth {
        req = req.header("Authorization", a);
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    let challenge = r
        .headers()
        .get("www-authenticate")
        .map(|v| v.to_str().unwrap().to_string());
    (status, r.json().await.unwrap_or(Value::Null), challenge)
}

fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}})
}

#[tokio::test]
async fn fixture_provider_passes_the_suite() {
    let report = run(&cfg(serve(false, Limits::default()).await)).await;
    assert!(report.passed(), "{report}");
    assert!(report.checks.len() > V1_TOOLS.len());
}

#[tokio::test]
async fn suite_fails_when_query_help_is_not_generated_from_the_keyword_file() {
    let report = run(&cfg(serve(true, Limits::default()).await)).await;
    assert!(
        report.failures().iter().any(|f| f.contains("query_help")),
        "{report}"
    );
}

#[tokio::test]
async fn unauthenticated_request_gets_the_challenge() {
    let url = serve(false, Limits::default()).await;
    let (status, _, challenge) = rpc(
        &url,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, 401);
    assert!(challenge.unwrap().starts_with("Bearer"));
}

#[tokio::test]
async fn oversized_result_is_refused_not_truncated() {
    let limits = Limits {
        max_result_bytes: 500,
        ..Limits::default()
    };
    let url = serve(false, limits).await;
    let (_, body, _) = rpc(&url, Some("Bearer ok"), call("get_card", json!({}))).await;
    assert_eq!(body["result"]["isError"], true);
    assert!(
        body["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("nothing was truncated")
    );
    assert!(body["result"].get("structuredContent").is_none());
}

#[tokio::test]
async fn rate_limit_is_per_grant() {
    let limits = Limits {
        calls_per_window: 2,
        window: Duration::from_secs(3600),
        ..Limits::default()
    };
    let url = serve(false, limits).await;
    for _ in 0..2 {
        assert_eq!(
            rpc(&url, Some("Bearer ok"), call("list_decks", json!({})))
                .await
                .0,
            200
        );
    }
    let (status, body, _) = rpc(&url, Some("Bearer ok"), call("list_decks", json!({}))).await;
    assert_eq!(status, 429);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("rate limit")
    );
}

#[tokio::test]
async fn protocol_errors_and_tool_errors_are_mcp_shaped() {
    let url = serve(false, Limits::default()).await;
    let a = Some("Bearer ok");
    let (_, b, _) = rpc(&url, a, call("search_cards", json!({}))).await;
    assert_eq!(b["error"]["code"], -32602); // required q missing
    let (_, b, _) = rpc(&url, a, call("search_cards", json!({"q": 3}))).await;
    assert_eq!(b["error"]["code"], -32602);
    let (_, b, _) = rpc(
        &url,
        a,
        json!({"jsonrpc": "2.0", "id": 1, "method": "nope"}),
    )
    .await;
    assert_eq!(b["error"]["code"], -32601);
    let (_, b, _) = rpc(&url, a, call("get_deck", json!({}))).await;
    assert_eq!(b["result"]["isError"], true);
    let (status, _, _) = rpc(&url, a, json!([])).await;
    assert_eq!(status, 400);
    let (status, _, _) = rpc(
        &url,
        a,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(status, 202);
}
