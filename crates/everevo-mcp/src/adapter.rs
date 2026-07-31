//! MCP tool adapter — wraps MCP `ToolDef` as an everevo `Tool` trait implementation.

use super::client::McpClient;
use super::protocol::ToolDef;
use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Adapter that exposes an MCP tool as an everevo `Tool`.
///
/// Shares the underlying MCP client via `Arc<Mutex<>>` so multiple tools
/// from the same server share one connection.
pub struct McpTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
    client: Arc<Mutex<McpClient>>,
}

impl McpTool {
    /// Create an adapter for each tool discovered on an MCP server.
    pub fn from_defs(client: Arc<Mutex<McpClient>>, tools: &[ToolDef]) -> Vec<Arc<dyn Tool>> {
        tools
            .iter()
            .map(|td| {
                Arc::new(Self {
                    name: td.name.clone(),
                    description: td.description.clone(),
                    parameters: td.input_schema.clone(),
                    client: Arc::clone(&client),
                }) as Arc<dyn Tool>
            })
            .collect()
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let mut client = self.client.lock().await;
        match client.call_tool(&self.name, params, cancel).await {
            Ok((text, images)) => {
                // When images are present (e.g. browser_screenshot), annotate
                // the text so the LLM knows a screenshot was attached; the
                // actual image bytes travel via the `images` field to a
                // vision-capable model.
                let content = if images.is_empty() {
                    text
                } else {
                    format!(
                        "{text}\n[{n} image(s) attached — visible to vision models]",
                        n = images.len()
                    )
                };
                Ok(ToolOutput {
                    content,
                    is_error: false,
                    images,
                })
            }
            Err(e) => {
                // Return error as content so the LLM can see the MCP error
                Ok(ToolOutput {
                    content: format!("MCP error: {e}"),
                    is_error: true,
                    ..Default::default()
                })
            }
        }
    }
}

/// Discover tools from an MCP server (stdio transport) and return them as everevo-compatible tools.
pub async fn discover_mcp_tools(
    command: &str,
    args: &[&str],
    env: &std::collections::HashMap<String, String>,
) -> Result<(Arc<Mutex<McpClient>>, Vec<Arc<dyn Tool>>), String> {
    let client = McpClient::connect_stdio(command, args, env).await?;
    let _tool_count = client.tools.len();
    let result = finalize_discovery(client).await?;
    tracing::info!(
        tool_count = _tool_count,
        command,
        "MCP tools discovered (stdio)"
    );
    Ok(result)
}

/// Discover tools from an MCP server (HTTP transport) — no local process needed.
pub async fn discover_mcp_tools_http(
    url: &str,
    headers: &std::collections::HashMap<String, String>,
) -> Result<(Arc<Mutex<McpClient>>, Vec<Arc<dyn Tool>>), String> {
    let client = McpClient::connect_http(url, headers).await?;
    let _tool_count = client.tools.len();
    let result = finalize_discovery(client).await?;
    tracing::info!(tool_count = _tool_count, %url, "MCP tools discovered (HTTP)");
    Ok(result)
}

/// Shared discovery finalization: wrap client in Arc<Mutex<>> and create tool adapters.
async fn finalize_discovery(
    client: McpClient,
) -> Result<(Arc<Mutex<McpClient>>, Vec<Arc<dyn Tool>>), String> {
    let client = Arc::new(Mutex::new(client));

    let tools = {
        let guard = client.lock().await;
        McpTool::from_defs(Arc::clone(&client), &guard.tools)
    };

    Ok((client, tools))
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolDef;

    fn make_tool_def(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: format!("Tool {name}"),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } }
            }),
        }
    }

    fn make_empty_client() -> Arc<Mutex<McpClient>> {
        // Create a dummy HTTP transport client for tests.
        // We use a child process solely to keep the struct valid;
        // tests never actually call the MCP server.
        let mut child = tokio::process::Command::new("cmd")
            .args(["/c", "exit 0"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let transport = crate::client::Transport::Stdio {
            child: Box::new(child),
            writer: tokio::io::BufWriter::new(stdin),
            reader: tokio::io::BufReader::new(stdout),
        };

        Arc::new(Mutex::new(McpClient {
            transport,
            next_id: 0,
            server_info: crate::protocol::ServerInfo {
                name: "test".into(),
                version: "1.0".into(),
            },
            tools: vec![],
        }))
    }

    #[test]
    fn test_mcp_tool_name_and_description() {
        let client = make_empty_client();
        let tool_def = make_tool_def("test_tool");
        let tools = McpTool::from_defs(client, &[tool_def]);

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "test_tool");
        assert!(tools[0].description().contains("Tool test_tool"));
    }

    #[test]
    fn test_mcp_tool_parameters_schema() {
        let client = make_empty_client();
        let tool_def = make_tool_def("search");
        let tools = McpTool::from_defs(client, &[tool_def]);

        let schema = tools[0].parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"]["type"] == "string");
    }

    #[test]
    fn test_mcp_tool_risk_level() {
        let client = make_empty_client();
        let tool_def = make_tool_def("any_tool");
        let tools = McpTool::from_defs(client, &[tool_def]);

        // MCP tools default to Medium risk
        assert_eq!(tools[0].risk_level(), RiskLevel::Medium);
    }

    #[test]
    fn test_from_defs_multiple_tools() {
        let client = make_empty_client();
        let defs = vec![
            make_tool_def("search"),
            make_tool_def("fetch"),
            make_tool_def("summarize"),
        ];
        let tools = McpTool::from_defs(client, &defs);

        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].name(), "search");
        assert_eq!(tools[1].name(), "fetch");
        assert_eq!(tools[2].name(), "summarize");
    }

    #[test]
    fn test_from_defs_empty_list() {
        let client = make_empty_client();
        let tools = McpTool::from_defs(client, &[]);
        assert!(tools.is_empty());
    }
}
