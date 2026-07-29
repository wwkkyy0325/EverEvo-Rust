//! EverEvo MCP (Model Context Protocol) client.
//!
//! Connects to MCP-compatible tool servers via:
//! - stdio (JSON-RPC 2.0 over stdin/stdout — spawns child process)
//! - HTTP (Streamable HTTP 2025-03-26 — connects to remote servers)
//!
//! Claude Code alignment: supports all three transport types that
//! Claude Code's `.mcp.json` config format supports.
//!
//! ## Quick start (stdio)
//!
//! ```rust,ignore
//! use everevo_mcp::{discover_mcp_tools, McpTool};
//! let (_client, tools) = discover_mcp_tools("npx", &["-y", "mcp-server"], &HashMap::new()).await?;
//! ```
//!
//! ## Quick start (HTTP)
//!
//! ```rust,ignore
//! use everevo_mcp::discover_mcp_tools_http;
//! let (_client, tools) = discover_mcp_tools_http("https://mcp.example.com/mcp", &HashMap::new()).await?;
//! ```

#![allow(clippy::disallowed_methods)]

pub mod adapter;
pub mod client;
pub mod protocol;

pub use adapter::{discover_mcp_tools, discover_mcp_tools_http, McpTool};
pub use client::McpClient;
pub use protocol::*;
