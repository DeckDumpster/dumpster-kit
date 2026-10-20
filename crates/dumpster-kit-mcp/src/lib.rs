//! Game-agnostic MCP server for the dumpster sites.
//!
//! A site implements [`ToolProvider`] for its game and a [`ContextResolver`] that turns a request
//! into a [`ToolContext`] (the auth crate's job); [`router`] serves them over Streamable HTTP.
//! [`Keywords`] generates the `search_cards` description and `query_help` output from the site's
//! `search_keywords.json`, and [`conformance`] checks a running server against the v1 contract.

pub mod conformance;
mod keywords;
mod limits;
mod provider;
mod server;

pub use keywords::{Keyword, Keywords};
pub use limits::{Limits, RateLimiter};
pub use provider::{
    ContextResolver, McpAuthError, ResolveFuture, ToolContext, ToolDef, ToolError, ToolProvider,
};
pub use server::{PROTOCOL_VERSION, router};

/// The v1 tool contract: the names every site exposes.
pub const V1_TOOLS: &[&str] = &[
    "search_cards",
    "get_card",
    "query_help",
    "collection_summary",
    "list_decks",
    "get_deck",
    "create_deck",
    "set_deck_lines",
    "release_deck",
    "delete_deck",
    "validate_deck",
];
