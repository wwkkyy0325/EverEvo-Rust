//! MCP protocol types — JSON-RPC 2.0 per modelcontextprotocol.io spec.
//!
//! This crate provides ONLY the wire-format types shared between MCP clients
//! and servers. It depends on `serde` + `serde_json` only — no async runtime,
//! no HTTP client, no subprocess management.
//!
//! ## Why a separate crate
//!
//! Plugin binaries (compiled as standalone `.exe`) need to speak MCP but
//! should NOT depend on `everevo-mcp` (which pulls tokio + reqwest).
//! This crate gives plugins the types they need with zero heavy deps.
//!
//! Used by:
//! - `everevo-mcp` (client implementation, re-exports these types)
//! - `everevo-kernel` (PluginRegistry internal types)
//! - Every plugin binary (types for stdin/stdout JSON-RPC)

use serde::{Deserialize, Serialize};

// ── JSON-RPC 2.0 base types ──────────────────────────────────────────────

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    pub id: u64,
}

/// A JSON-RPC 2.0 response (success or error).
///
/// Per JSON-RPC 2.0 §5: id MUST be the same as the request id, or null
/// if the request id could not be determined (parse error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    /// Request identifier. `null` for parse errors where id could not be read.
    pub id: serde_json::Value,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

/// A JSON-RPC 2.0 notification (no id, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

// ── MCP Initialize ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub tools: Option<ToolsCapability>,
    #[serde(default)]
    pub resources: Option<ResourcesCapability>,
    #[serde(default)]
    pub prompts: Option<PromptsCapability>,
    #[serde(default)]
    pub logging: Option<serde_json::Value>,
    #[serde(default)]
    pub roots: Option<RootsCapability>,
    #[serde(default)]
    pub sampling: Option<serde_json::Value>,
    #[serde(default)]
    pub experimental: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesCapability {
    #[serde(default)]
    pub subscribe: bool,
    #[serde(default, rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptsCapability {
    #[serde(default, rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootsCapability {
    #[serde(default, rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(default)]
    pub tools: Option<ToolsCapability>,
    #[serde(default)]
    pub resources: Option<ResourcesCapability>,
    #[serde(default)]
    pub prompts: Option<PromptsCapability>,
    #[serde(default)]
    pub logging: Option<serde_json::Value>,
    #[serde(default)]
    pub experimental: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

// ── MCP Tools ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<ToolDef>,
}

/// A tool definition as returned by `tools/list`.
///
/// This is the core type that plugins expose and the kernel consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub is_error: bool,
}

/// Content blocks returned by a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    Resource { resource: ResourceContent },
}

/// Embedded resource content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub text: String,
}

// ── MCP Resources ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResourcesResult {
    pub resources: Vec<ResourceDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "mimeType", default)]
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResourceParams {
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResourceResult {
    pub contents: Vec<ResourceContent>,
}

// ── MCP Prompts ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPromptsResult {
    pub prompts: Vec<PromptDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetPromptParams {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetPromptResult {
    #[serde(default)]
    pub description: String,
    pub messages: Vec<PromptMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptMessage {
    pub role: String,
    pub content: PromptContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PromptContent {
    Text { r#type: String, text: String },
    Simple(String),
}

// ── Builder helpers ─────────────────────────────────────────────────────

impl JsonRpcRequest {
    pub fn new(method: &str, id: u64) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params: None,
            id,
        }
    }

    pub fn with_params(mut self, params: serde_json::Value) -> Self {
        self.params = Some(params);
        self
    }
}

impl JsonRpcNotification {
    pub fn new(method: &str) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params: None,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest::new("tools/list", 1);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"method\":\"tools/list\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn test_response_deserialization() {
        let json = r#"{"jsonrpc":"2.0","result":{"tools":[]},"id":1}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_error_response() {
        let json =
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_parse_error_response_with_null_id() {
        // JSON-RPC 2.0 §5: "If there was an error in detecting the id
        // in the Request object (e.g. Parse error/Invalid Request),
        // it MUST be Null."
        let json = r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"},"id":null}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.error.is_some());
        assert!(resp.id.is_null());
    }

    #[test]
    fn test_tool_def_deserialization() {
        let json =
            r#"{"name":"read","description":"Read a file","inputSchema":{"type":"object"}}"#;
        let td: ToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(td.name, "read");
        assert_eq!(td.description, "Read a file");
    }

    #[test]
    fn test_call_tool_result_text() {
        let json = r#"{"content":[{"type":"text","text":"hello"}],"isError":false}"#;
        let result: CallToolResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.content.len(), 1);
        assert!(!result.is_error);
    }

    #[test]
    fn test_initialize_params_serde() {
        let params = InitializeParams {
            protocol_version: "2025-03-26".into(),
            capabilities: ClientCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: false,
                }),
                ..Default::default()
            },
            client_info: ClientInfo {
                name: "test".into(),
                version: "0.1.0".into(),
            },
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("protocolVersion"));
        let back: InitializeParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.protocol_version, "2025-03-26");
    }

    #[test]
    fn test_tool_def_roundtrip() {
        let td = ToolDef {
            name: "shell".into(),
            description: "Execute a command".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        };
        let json = serde_json::to_string(&td).unwrap();
        let back: ToolDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "shell");
        assert_eq!(back.input_schema["type"], "object");
    }
}
