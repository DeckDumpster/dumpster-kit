use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use crate::limits::{Limits, RateLimiter};
use crate::provider::{
    ContextResolver, McpAuthError, ToolContext, ToolDef, ToolError, ToolProvider,
};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const RATE_LIMITED: i64 = -32029;

struct App {
    provider: Arc<dyn ToolProvider>,
    resolver: Arc<dyn ContextResolver>,
    limits: Limits,
    limiter: RateLimiter,
    server_name: String,
}

/// The MCP endpoint as a router serving `POST /`; nest it at `/mcp`. GET answers 405 (this
/// server opens no SSE stream); every response is `application/json`.
pub fn router(
    server_name: &str,
    provider: Arc<dyn ToolProvider>,
    resolver: Arc<dyn ContextResolver>,
    limits: Limits,
) -> Router {
    let app = Arc::new(App {
        provider,
        resolver,
        limiter: RateLimiter::new(&limits),
        limits,
        server_name: server_name.to_string(),
    });
    Router::new()
        .route(
            "/",
            post(handle).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .with_state(app)
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn auth_response(e: McpAuthError) -> Response {
    let status = if e.forbidden {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::UNAUTHORIZED
    };
    let mut resp = json_response(status, rpc_error(Value::Null, -32001, e.message));
    if let Some(Ok(v)) = e.www_authenticate.map(|v| HeaderValue::from_str(&v)) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    resp
}

async fn handle(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    // Authenticate first: an unauthenticated caller learns nothing, not even the tool list.
    let ctx = match app.resolver.resolve(&headers).await {
        Ok(c) => c,
        Err(e) => return auth_response(e),
    };
    if body.len() > app.limits.max_request_bytes {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            rpc_error(Value::Null, INVALID_REQUEST, "request too large"),
        );
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, PARSE_ERROR, format!("parse error: {e}")),
            );
        }
    };
    if !msg.is_object() {
        return json_response(
            StatusCode::BAD_REQUEST,
            rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "batches are not supported; send one JSON-RPC message per POST",
            ),
        );
    }
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // A response or malformed message: nothing to answer.
        return StatusCode::ACCEPTED.into_response();
    };
    let Some(id) = msg.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response(); // notification
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let (status, reply) = dispatch(&app, &ctx, method, params, id).await;
    json_response(status, reply)
}

async fn dispatch(
    app: &Arc<App>,
    ctx: &ToolContext,
    method: &str,
    params: Value,
    id: Value,
) -> (StatusCode, Value) {
    let ok = |result: Value| {
        (
            StatusCode::OK,
            json!({"jsonrpc": "2.0", "id": id.clone(), "result": result}),
        )
    };
    let err = |code, m: String| (StatusCode::OK, rpc_error(id.clone(), code, m));
    match method {
        "initialize" => ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": app.server_name, "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": app.provider.tools()})),
        "tools/call" => {
            let Some(name) = params
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                return err(INVALID_PARAMS, "tools/call needs a tool name".into());
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let tools = app.provider.tools();
            let Some(def) = tools.iter().find(|t| t.name == name) else {
                return err(INVALID_PARAMS, format!("unknown tool '{name}'"));
            };
            if let Err(m) = validate_args(def, &args) {
                return err(INVALID_PARAMS, m);
            }
            if let Err(wait) = app.limiter.check(&ctx.grant) {
                let secs = wait.as_secs() + 1;
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    rpc_error(
                        id,
                        RATE_LIMITED,
                        format!("rate limit exceeded for this grant; retry in {secs}s"),
                    ),
                );
            }
            let (provider, ctx2) = (app.provider.clone(), ctx.clone());
            let outcome =
                tokio::task::spawn_blocking(move || provider.call(&ctx2, &name, args)).await;
            ok(tool_result(
                &app.limits,
                outcome.unwrap_or_else(|e| Err(ToolError::Internal(e.to_string()))),
            ))
        }
        other => err(METHOD_NOT_FOUND, format!("method not found: {other}")),
    }
}

fn error_result(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn tool_result(limits: &Limits, outcome: Result<Value, ToolError>) -> Value {
    match outcome {
        Ok(value) => {
            let text = value.to_string();
            if text.len() > limits.max_result_bytes {
                return error_result(format!(
                    "result is {} bytes, over the {}-byte cap; nothing was truncated. Narrow the query or page with a smaller limit.",
                    text.len(),
                    limits.max_result_bytes
                ));
            }
            json!({"content": [{"type": "text", "text": text}], "structuredContent": value, "isError": false})
        }
        Err(ToolError::Failed(m)) => error_result(m),
        Err(ToolError::Internal(m)) => {
            eprintln!("dumpster-kit-mcp: internal tool error: {m}");
            error_result("internal error".into())
        }
    }
}

/// Checks `args` against the schema's `required` and the primitive `type` of each declared property.
fn validate_args(def: &ToolDef, args: &Value) -> Result<(), String> {
    let obj = args
        .as_object()
        .ok_or_else(|| format!("arguments of '{}' must be an object", def.name))?;
    for r in def
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !obj.contains_key(r) {
            return Err(format!("'{}' needs argument '{r}'", def.name));
        }
    }
    let props = def
        .input_schema
        .get("properties")
        .and_then(Value::as_object);
    for (k, v) in obj {
        let Some(ty) = props
            .and_then(|p| p.get(k))
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let good = match ty {
            "string" => v.is_string(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "boolean" => v.is_boolean(),
            "array" => v.is_array(),
            "object" => v.is_object(),
            _ => true,
        };
        if !good && !v.is_null() {
            return Err(format!("argument '{k}' of '{}' must be {ty}", def.name));
        }
    }
    Ok(())
}
