//! MCP protocol types — re-exported from `everevo-mcp-protocol`.
//!
//! Backward-compatible type aliases keep existing code working with
//! the short names (`Request`, `Response`, etc.) while the canonical
//! names live in `everevo_mcp_protocol` (`JsonRpcRequest`, etc.).

pub use everevo_mcp_protocol::*;

// ── Backward-compatible type aliases ────────────────────────────────────

pub type Request = JsonRpcRequest;
pub type Response = JsonRpcResponse;
pub type RpcError = JsonRpcError;
pub type Notification = JsonRpcNotification;
