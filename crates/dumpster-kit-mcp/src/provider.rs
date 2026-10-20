use std::future::Future;
use std::pin::Pin;

use axum::http::HeaderMap;
use serde::Serialize;
use serde_json::Value;

/// One tool as `tools/list` reports it.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema (an object schema) for the arguments.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Who is calling: the tenant the agent acts as and the grant its token was issued under. The
/// grant is the rate-limit key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContext {
    pub tenant: String,
    pub grant: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The agent can act on this message (a parse error naming the nearest terms, a copy that is
    /// not owned). Returned to the agent as an `isError` result.
    #[error("{0}")]
    Failed(String),
    /// Not for the agent: logged by the caller's tracing, the agent sees a generic message.
    #[error("{0}")]
    Internal(String),
}

/// Implemented once per site.
pub trait ToolProvider: Send + Sync + 'static {
    fn tools(&self) -> Vec<ToolDef>;
    /// Runs on a blocking thread, so it may use synchronous database access. The returned value
    /// becomes both the text content and `structuredContent` of the result.
    fn call(&self, ctx: &ToolContext, name: &str, args: Value) -> Result<Value, ToolError>;
}

/// A request that could not be tied to a tenant. Becomes HTTP 401 with `www_authenticate` (the
/// OAuth protected-resource challenge) when set, else HTTP 403 when `forbidden`.
#[derive(Debug, Clone)]
pub struct McpAuthError {
    pub message: String,
    pub www_authenticate: Option<String>,
    pub forbidden: bool,
}

impl McpAuthError {
    pub fn unauthorized(message: impl Into<String>, www_authenticate: Option<String>) -> Self {
        Self {
            message: message.into(),
            www_authenticate,
            forbidden: false,
        }
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            www_authenticate: None,
            forbidden: true,
        }
    }
}

pub type ResolveFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolContext, McpAuthError>> + Send + 'a>>;

/// How a request becomes a [`ToolContext`]; the auth crate implements it.
pub trait ContextResolver: Send + Sync + 'static {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap) -> ResolveFuture<'a>;
}
