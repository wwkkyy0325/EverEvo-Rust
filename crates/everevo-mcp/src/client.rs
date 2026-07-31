//! MCP client — supports stdio and HTTP transports.
//!
//! Claude Code alignment:
//! - stdio: spawns a child process, JSON-RPC over stdin/stdout
//! - HTTP: Streamable HTTP (2025-03-26), POST to /mcp endpoint
//! - SSE transport shares the HTTP path (POST + SSE response parsing)

use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::protocol::*;

// ── Transport ────────────────────────────────────────────────────────────

/// Internal transport abstraction — same JSON-RPC protocol, different I/O.
pub enum Transport {
    /// stdio: spawn a local process, communicate via stdin/stdout.
    Stdio {
        child: Box<Child>,
        writer: BufWriter<ChildStdin>,
        reader: BufReader<ChildStdout>,
    },
    /// HTTP: Streamable HTTP transport (2025-03-26 spec).
    /// POST JSON-RPC to `{base_url}`, receive JSON-RPC response.
    Http {
        base_url: String,
        headers: HashMap<String, String>,
        http_client: reqwest::Client,
    },
}

// ── Client ───────────────────────────────────────────────────────────────

/// Connected MCP client with discovered tools.
pub struct McpClient {
    pub transport: Transport,
    pub(crate) next_id: u64,
    pub server_info: ServerInfo,
    pub tools: Vec<ToolDef>,
}

impl McpClient {
    /// Connect to an MCP server via stdio (spawns the command).
    pub async fn connect_stdio(
        command: &str,
        args: &[&str],
        env: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn MCP server '{command}': {e}"))?;

        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;

        let transport = Transport::Stdio {
            child: Box::new(child),
            writer: BufWriter::new(stdin),
            reader: BufReader::new(stdout),
        };

        Self::handshake(transport).await
    }

    /// Connect to an MCP server via HTTP (Streamable HTTP transport).
    pub async fn connect_http(
        base_url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(120))
            .user_agent(format!("EverEvo-MCP/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("create HTTP client: {e}"))?;

        let transport = Transport::Http {
            base_url: base_url.trim_end_matches('/').to_string(),
            headers: headers.clone(),
            http_client,
        };

        Self::handshake(transport).await
    }

    /// Common handshake logic for all transports.
    async fn handshake(transport: Transport) -> Result<Self, String> {
        let mut client = Self {
            transport,
            next_id: 1,
            server_info: ServerInfo {
                name: String::new(),
                version: String::new(),
            },
            tools: Vec::new(),
        };

        // ── Initialize handshake ──
        let init_params = serde_json::to_value(InitializeParams {
            protocol_version: "2025-03-26".into(),
            capabilities: ClientCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: false,
                }),
            },
            client_info: ClientInfo {
                name: "EverEvo".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        })
        .map_err(|e| format!("serialize init: {e}"))?;

        let init_result = client.send_recv("initialize", Some(init_params)).await?;
        let init: InitializeResult = serde_json::from_value(init_result)
            .map_err(|e| format!("parse initialize result: {e}"))?;

        // Send initialized notification
        client
            .send_notification("notifications/initialized", None)
            .await?;

        // ── Discover tools ──
        let tools_result = client.send_recv("tools/list", None).await?;
        let tools: ListToolsResult = serde_json::from_value(tools_result)
            .map_err(|e| format!("parse tools/list result: {e}"))?;

        client.server_info = init.server_info;
        client.tools = tools.tools;

        Ok(client)
    }

    /// Call an MCP tool and return the text content.
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(String, Vec<everevo_core::ImageData>), String> {
        // Check cancellation before sending
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err("cancelled".into());
        }

        let params = serde_json::to_value(CallToolParams {
            name: name.into(),
            arguments,
        })
        .map_err(|e| format!("serialize: {e}"))?;

        let req_id = self.next_id;

        let result = if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let cancel_params = serde_json::json!({
                        "requestId": req_id,
                        "reason": "User cancelled"
                    });
                    let _ = self.send_notification("notifications/cancelled", Some(cancel_params)).await;
                    return Err("cancelled".into());
                }
                r = self.send_recv("tools/call", Some(params)) => r,
            }
        } else {
            self.send_recv("tools/call", Some(params)).await
        };

        let result = result?;
        let call: CallToolResult =
            serde_json::from_value(result).map_err(|e| format!("parse tools/call result: {e}"))?;

        let mut text_parts = Vec::new();
        let mut images = Vec::new();
        for block in &call.content {
            match block {
                ContentBlock::Text { text } => text_parts.push(text.clone()),
                ContentBlock::Image { data, mime_type } => {
                    images.push(everevo_core::ImageData {
                        data: data.clone(),
                        mime_type: mime_type.clone(),
                    });
                }
                ContentBlock::Resource { .. } => {}
            }
        }
        let text = text_parts.join("\n");

        Ok((text, images))
    }

    /// List available tool names.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name.as_str()).collect()
    }

    /// Ping the MCP server to verify the connection is still alive.
    pub async fn ping(&mut self) -> bool {
        match self.send_recv("ping", None).await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(error = %e, "MCP ping failed — server may be dead");
                false
            }
        }
    }

    /// Check if the server process is still running (stdio only).
    /// HTTP transports always return true (connection is stateless).
    pub fn is_alive(&mut self) -> bool {
        match &mut self.transport {
            Transport::Stdio { child, .. } => match child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    tracing::warn!(exit = %status, "MCP server process exited");
                    false
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to check MCP server process");
                    false
                }
            },
            Transport::Http { .. } => {
                // HTTP is stateless — can't check process liveness.
                // Use ping() or is_alive_async() to verify connectivity instead.
                true
            }
        }
    }

    /// Check HTTP transport liveness by sending a ping RPC.
    /// For stdio, equivalent to `is_alive()` (checks process exit).
    pub async fn is_alive_async(&mut self) -> bool {
        match &mut self.transport {
            Transport::Stdio { child, .. } => match child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    tracing::warn!(exit = %status, "MCP server process exited");
                    false
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to check MCP server process");
                    false
                }
            },
            Transport::Http { .. } => {
                self.ping().await
            }
        }
    }

    /// Discover available resources from the MCP server.
    pub async fn list_resources(&mut self) -> Result<Vec<ResourceDef>, String> {
        let result = self.send_recv("resources/list", None).await?;
        let list: ListResourcesResult =
            serde_json::from_value(result).map_err(|e| format!("parse resources/list: {e}"))?;
        Ok(list.resources)
    }

    /// Read a resource by URI.
    pub async fn read_resource(&mut self, uri: &str) -> Result<String, String> {
        let params = serde_json::to_value(ReadResourceParams { uri: uri.into() })
            .map_err(|e| format!("serialize: {e}"))?;
        let result = self.send_recv("resources/read", Some(params)).await?;
        let read: ReadResourceResult =
            serde_json::from_value(result).map_err(|e| format!("parse resources/read: {e}"))?;
        let text = read
            .contents
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(text)
    }

    /// Discover available prompt templates from the MCP server.
    pub async fn list_prompts(&mut self) -> Result<Vec<PromptDef>, String> {
        let result = self.send_recv("prompts/list", None).await?;
        let list: ListPromptsResult =
            serde_json::from_value(result).map_err(|e| format!("parse prompts/list: {e}"))?;
        Ok(list.prompts)
    }

    /// Get a rendered prompt by name with arguments.
    pub async fn get_prompt(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<GetPromptResult, String> {
        let params = serde_json::to_value(GetPromptParams {
            name: name.into(),
            arguments,
        })
        .map_err(|e| format!("serialize: {e}"))?;
        let result = self.send_recv("prompts/get", Some(params)).await?;
        let prompt: GetPromptResult =
            serde_json::from_value(result).map_err(|e| format!("parse prompts/get: {e}"))?;
        Ok(prompt)
    }

    // ── Internal: JSON-RPC send/recv ──────────────────────────────────

    async fn send_recv(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        let req = Request {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id,
        };
        let req_json = serde_json::to_string(&req).map_err(|e| format!("serialize: {e}"))?;

        match &mut self.transport {
            Transport::Stdio { writer, reader, .. } => {
                writer
                    .write_all(req_json.as_bytes())
                    .await
                    .map_err(|e| format!("write: {e}"))?;
                writer
                    .write_all(b"\n")
                    .await
                    .map_err(|e| format!("write newline: {e}"))?;
                writer.flush().await.map_err(|e| format!("flush: {e}"))?;

                let mut line = String::new();
                reader
                    .read_line(&mut line)
                    .await
                    .map_err(|e| format!("read: {e}"))?;
                if line.is_empty() {
                    return Err("no response from MCP server".into());
                }

                let resp: Response =
                    serde_json::from_str(&line).map_err(|e| format!("parse: {e}"))?;
                if let Some(err) = resp.error {
                    return Err(format!("MCP error {}: {}", err.code, err.message));
                }
                Ok(resp.result.unwrap_or(serde_json::Value::Null))
            }

            Transport::Http {
                base_url,
                headers,
                http_client,
            } => {
                let url: &String = base_url; // coerce &mut String → &String for IntoUrl
                let mut http_req = http_client
                    .post(url)
                    .header("Content-Type", "application/json")
                    .body(req_json);

                for (k, v) in headers {
                    http_req = http_req.header(k.as_str(), v.as_str());
                }

                let resp = http_req
                    .send()
                    .await
                    .map_err(|e| format!("HTTP request failed: {e}"))?;

                let status = resp.status();
                let body = resp
                    .text()
                    .await
                    .map_err(|e| format!("read HTTP body: {e}"))?;

                if !status.is_success() {
                    return Err(format!("HTTP {status}: {body}"));
                }

                // Streamable HTTP: response may be JSON or SSE
                let json_val: serde_json::Value = if body.trim_start().starts_with('{') {
                    serde_json::from_str(&body).map_err(|e| format!("parse JSON response: {e}"))?
                } else {
                    // Try parsing as SSE — find the first data: line with JSON
                    body.lines()
                        .find_map(|line| {
                            line.strip_prefix("data: ")
                                .and_then(|data| serde_json::from_str(data).ok())
                        })
                        .ok_or_else(|| {
                            format!("Invalid response (not JSON nor SSE data): {body}")
                        })?
                };

                if let Some(err) = json_val.get("error") {
                    return Err(format!(
                        "MCP error {}: {}",
                        err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1),
                        err.get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown"),
                    ));
                }
                Ok(json_val
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            }
        }
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let notif = Notification {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        };
        let json = serde_json::to_string(&notif).map_err(|e| format!("serialize: {e}"))?;

        match &mut self.transport {
            Transport::Stdio { writer, .. } => {
                writer
                    .write_all(json.as_bytes())
                    .await
                    .map_err(|e| format!("write: {e}"))?;
                writer
                    .write_all(b"\n")
                    .await
                    .map_err(|e| format!("write newline: {e}"))?;
                writer.flush().await.map_err(|e| format!("flush: {e}"))?;
                Ok(())
            }
            Transport::Http {
                base_url,
                headers,
                http_client,
            } => {
                let url: &String = base_url; // coerce &mut String → &String
                let mut http_req = http_client
                    .post(url)
                    .header("Content-Type", "application/json")
                    .body(json);

                for (k, v) in headers {
                    http_req = http_req.header(k.as_str(), v.as_str());
                }

                // Notifications are fire-and-forget — just log errors
                match http_req.send().await {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        tracing::warn!(error = %e, "MCP notification send failed (non-fatal)");
                        Ok(())
                    }
                }
            }
        }
    }
}
