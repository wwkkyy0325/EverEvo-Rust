//! HTTP LLM client — Anthropic + OpenAI-compatible streaming.
//!
//! ## Proxy detection
//!
//! The client reads `HTTPS_PROXY` / `HTTP_PROXY` env vars (and their
//! lowercase variants) to route through a local proxy. This is essential in
//! mainland China where direct API access to `api.deepseek.com` or
//! `api.anthropic.com` may be blocked. Common proxy setups:
//! - Clash / V2Ray: `http://127.0.0.1:7890`
//! - System proxy (IE settings): auto-detected via `native-tls`
//! - Env var: `HTTPS_PROXY=http://your-proxy:port`

use async_trait::async_trait;

use everevo_core::llm::{LlmMessage, LlmProvider, LlmResponse, StreamEvent, ToolSchema};
use everevo_core::EverEvoError;

// ── Proxy detection ───────────────────────────────────────────────────────

/// Detect proxy URL from environment variables.
///
/// Checks in order: `EVEREVO_HTTP_PROXY`, `HTTPS_PROXY`, `https_proxy`,
/// `HTTP_PROXY`, `http_proxy`, `ALL_PROXY`, `all_proxy`.
///
/// Falls back to auto-detecting common local proxy ports (Clash: 7890,
/// V2Ray: 10808) by attempting a TCP connect. Returns `None` only if
/// no proxy is configured and no known proxy port responds.
pub async fn detect_proxy() -> Option<String> {
    // 1. Explicit override (EverEvo-specific)
    if let Ok(val) = std::env::var("EVEREVO_HTTP_PROXY") {
        let val = val.trim().to_string();
        if !val.is_empty() {
            tracing::info!(proxy = %val, "Using EVEREVO_HTTP_PROXY");
            return Some(val);
        }
    }
    // 2. Standard env vars
    for var in &[
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(val) = std::env::var(var) {
            let val = val.trim().to_string();
            if !val.is_empty() {
                tracing::info!(proxy_var = var, proxy = %val, "Proxy detected from env");
                return Some(val);
            }
        }
    }
    // 3. Auto-detect common local proxy ports (Clash / V2Ray / Shadowsocks)
    // These are the most common in mainland China; a TCP connect to the
    // proxy port confirms the proxy is running.
    const CANDIDATE_PORTS: &[u16] = &[7890, 7891, 10808, 10809, 8118, 1080];
    for &port in CANDIDATE_PORTS {
        let addr = format!("127.0.0.1:{port}");
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            // Assume HTTP proxy on this port. SOCKS5 ports (7891, 10809, 1080)
            // get a socks5h:// prefix; HTTP ports get http://.
            let scheme = match port {
                7891 | 10809 | 1080 => "socks5h",
                _ => "http",
            };
            let proxy_url = format!("{scheme}://{addr}");
            tracing::info!(%proxy_url, "Auto-detected local proxy");
            return Some(proxy_url);
        }
    }
    None
}

/// Sync proxy detection from env vars only (no network I/O).
pub fn detect_proxy_sync() -> Option<String> {
    if let Ok(val) = std::env::var("EVEREVO_HTTP_PROXY") {
        let val = val.trim().to_string();
        if !val.is_empty() {
            return Some(val);
        }
    }
    for var in &[
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(val) = std::env::var(var) {
            let val = val.trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Build a reqwest client with optional proxy.
pub fn build_llm_http_client(proxy_url: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent(format!("EverEvo/{}", env!("CARGO_PKG_VERSION")));

    // Apply proxy from env vars (sync)
    let env_proxy = detect_proxy_sync();
    let proxy_src = proxy_url.or(env_proxy.as_deref());
    if let Some(url) = proxy_src {
        match reqwest::Proxy::all(url) {
            Ok(proxy) => {
                builder = builder.proxy(proxy);
                tracing::info!(%url, "LLM client using proxy");
            }
            Err(e) => {
                tracing::warn!(%url, error = %e, "Invalid proxy URL — proceeding without proxy");
            }
        }
    }

    builder.build().unwrap_or_default()
}

// ── Client ────────────────────────────────────────────────────────────────

/// LLM provider via HTTP (Anthropic Messages API or OpenAI Chat Completions).
pub struct HttpClient {
    api_format: String, // "anthropic" | "openai"
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client, // shared connection pool — created once, reused
}

impl HttpClient {
    pub fn new(api_format: &str, api_key: &str, base_url: &str, model: &str) -> Self {
        Self::with_proxy(api_format, api_key, base_url, model, None)
    }

    /// Create client with an explicit proxy URL (bypasses env-var detection).
    pub fn with_proxy(
        api_format: &str,
        api_key: &str,
        base_url: &str,
        model: &str,
        proxy_url: Option<&str>,
    ) -> Self {
        let client = build_llm_http_client(proxy_url);
        Self {
            api_format: api_format.into(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            model: model.into(),
            client,
        }
    }

    /// Classify HTTP error for better diagnostics.
    fn classify_http_error(status: reqwest::StatusCode, body: &str) -> String {
        let code = status.as_u16();
        let detail = if body.len() > 300 { &body[..300] } else { body };
        match code {
            401 | 403 => format!(
                "Authentication failed (HTTP {code}). Check your API key in data/config.toml."
            ),
            429 => "Rate limited (HTTP 429). The API is throttling requests — wait a moment and retry."
                .to_string(),
            500..=599 => format!(
                "Server error (HTTP {code}). The LLM provider is having issues — retry in a few seconds. Detail: {detail}"
            ),
            400 => format!(
                "Bad request (HTTP 400). The prompt or parameters may be invalid. Detail: {detail}"
            ),
            404 => format!(
                "Endpoint not found (HTTP 404). Check base_url in data/config.toml: {}",
                body
            ),
            _ => format!("HTTP {code}: {detail}"),
        }
    }

    fn endpoint(&self) -> String {
        match self.api_format.as_str() {
            "anthropic" => format!("{}/v1/messages", self.base_url.trim_end_matches('/')),
            _ => format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
        }
    }

    fn build_body(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        stream: bool,
    ) -> serde_json::Value {
        if self.api_format == "anthropic" {
            self.build_anthropic_body(messages, tools, stream)
        } else {
            self.build_openai_body(messages, tools, stream)
        }
    }

    fn build_anthropic_body(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        stream: bool,
    ) -> serde_json::Value {
        let msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let is_tool_result = m.tool_call_id.is_some();
                let role_str = if is_tool_result {
                    "user".to_string()
                } else {
                    m.role.to_string()
                };

                let mut content_blocks: Vec<serde_json::Value> = Vec::new();

                if let Some(ref thinking) = m.thinking {
                    if !thinking.is_empty() {
                        content_blocks.push(serde_json::json!({
                            "type": "thinking",
                            "thinking": thinking,
                        }));
                    }
                }

                if !is_tool_result {
                    let has_thinking_or_tools = m.thinking.as_ref().is_some_and(|t| !t.is_empty())
                        || m.tool_calls.is_some();
                    if !m.content.is_empty() || !has_thinking_or_tools {
                        content_blocks.push(serde_json::json!({
                            "type": "text",
                            "text": m.content,
                        }));
                    }
                    // Multimodal: append image blocks (e.g. browser screenshots)
                    // for vision-capable models.
                    for img in &m.images {
                        content_blocks.push(serde_json::json!({
                            "type": "image",
                            "source": { "type": "base64", "media_type": img.mime_type, "data": img.data }
                        }));
                    }
                }
                if let Some(ref tcs) = m.tool_calls {
                    for tc in tcs {
                        content_blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.arguments,
                        }));
                    }
                }
                if is_tool_result {
                    let Some(raw_id) = m.tool_call_id.as_ref() else {
                        tracing::warn!("Tool result message without tool_call_id, skipping");
                        return serde_json::json!({"role": "user", "content": ""});
                    };
                    if raw_id.contains('|') {
                        if let Ok(items) =
                            serde_json::from_str::<Vec<serde_json::Value>>(&m.content)
                        {
                            for item in &items {
                                content_blocks.push(serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": item["i"].as_str().unwrap_or(""),
                                    "content": item["c"].as_str().unwrap_or(""),
                                }));
                            }
                        }
                    } else {
                        // Anthropic tool_result.content accepts an array of
                        // text/image blocks — used to carry screenshots back.
                        let content = if m.images.is_empty() {
                            serde_json::Value::String(m.content.clone())
                        } else {
                            let mut parts =
                                vec![serde_json::json!({ "type": "text", "text": m.content })];
                            for img in &m.images {
                                parts.push(serde_json::json!({
                                    "type": "image",
                                    "source": { "type": "base64", "media_type": img.mime_type, "data": img.data }
                                }));
                            }
                            serde_json::Value::Array(parts)
                        };
                        content_blocks.push(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": raw_id,
                            "content": content,
                        }));
                    }
                }

                serde_json::json!({ "role": role_str, "content": content_blocks })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": 4096,
            "messages": msgs,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools
                .iter()
                .map(|t| serde_json::json!({
                    "name": t.name, "description": t.description, "input_schema": t.parameters,
                }))
                .collect::<Vec<_>>());
        }
        if stream {
            body["stream"] = serde_json::json!(true);
        }
        body
    }

    fn build_openai_body(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        stream: bool,
    ) -> serde_json::Value {
        let mut msgs: Vec<serde_json::Value> = Vec::new();
        for m in messages {
            let role = m.role.to_string();

            if let Some(raw_id) = m.tool_call_id.as_ref() {
                if raw_id.contains('|') {
                    if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&m.content) {
                        for item in &items {
                            msgs.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": item["i"].as_str().unwrap_or(""),
                                "content": item["c"].as_str().unwrap_or(""),
                            }));
                        }
                    }
                }
                continue;
            }

            // OpenAI: tool-role messages can't carry images inline. Non-tool
            // messages with images use a content array (text + image_url).
            let is_tool_msg = m.tool_call_id.is_some();
            let content = if !is_tool_msg && !m.images.is_empty() {
                let mut parts = vec![serde_json::json!({ "type": "text", "text": m.content })];
                for img in &m.images {
                    parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", img.mime_type, img.data) }
                    }));
                }
                serde_json::Value::Array(parts)
            } else {
                serde_json::Value::String(m.content.clone())
            };
            let mut msg = serde_json::json!({ "role": role, "content": content });
            if let Some(ref thinking) = m.thinking {
                if !thinking.is_empty() {
                    msg["reasoning_content"] = serde_json::json!(thinking);
                }
            }
            if let Some(ref tcs) = m.tool_calls {
                msg["tool_calls"] = serde_json::json!(tcs.iter().map(|tc| serde_json::json!({
                    "id": tc.id, "type": "function", "function": { "name": tc.name, "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default() },
                })).collect::<Vec<_>>());
                msg["content"] = serde_json::Value::Null;
            }
            if is_tool_msg {
                msg["role"] = serde_json::json!("tool");
                msg["tool_call_id"] = serde_json::json!(m.tool_call_id);
            }
            msgs.push(msg);

            // Tool-result images can't live in a tool-role message; emit a
            // follow-up user message so a vision model can see the screenshot.
            if is_tool_msg && !m.images.is_empty() {
                let mut img_parts = vec![serde_json::json!({
                    "type": "text", "text": "[screenshot from tool result]"
                })];
                for img in &m.images {
                    img_parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", img.mime_type, img.data) }
                    }));
                }
                msgs.push(serde_json::json!({ "role": "user", "content": img_parts }));
            }
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": msgs,
            "stream": stream,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools.iter().map(|t| serde_json::json!({
                "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
            })).collect::<Vec<_>>());
        }
        body
    }
}

#[async_trait]
impl LlmProvider for HttpClient {
    async fn chat(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
    ) -> Result<LlmResponse, EverEvoError> {
        let body = self.build_body(messages, tools, false);
        let mut last_error: Option<EverEvoError> = None;

        for attempt in 0..=HttpClient::MAX_RETRIES {
            if attempt > 0 {
                let delay_ms = HttpClient::BASE_BACKOFF_MS * (1u64 << (attempt - 1)); // 1s, 2s, 4s
                tracing::info!(
                    attempt,
                    max_retries = HttpClient::MAX_RETRIES,
                    delay_ms,
                    "LLM retry — transient failure, backing off"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }

            let mut req = self.client.post(self.endpoint()).json(&body);
            if self.api_format == "anthropic" {
                req = req
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01");
            } else {
                req = req.header("Authorization", format!("Bearer {}", self.api_key));
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    let msg = format!("{e}");
                    let is_timeout = e.is_timeout() || msg.contains("timeout");
                    let is_connect = e.is_connect() || msg.contains("connect");
                    if is_timeout || is_connect {
                        let detail = if is_connect {
                            format!(
                                "Connection to {} failed. Check network, VPN/proxy, or base_url in config. \
                                 (Tip: set HTTPS_PROXY=http://127.0.0.1:PORT if using a local proxy.)",
                                self.endpoint()
                            )
                        } else {
                            format!("Request to {} timed out after 600s.", self.endpoint())
                        };
                        last_error = Some(EverEvoError::LlmProvider(format!(
                            "{detail} (attempt {}/{}, error: {e})",
                            attempt + 1,
                            HttpClient::MAX_RETRIES + 1
                        )));
                        continue;
                    }
                    return Err(EverEvoError::LlmProvider(e.to_string()));
                }
            };

            let status = resp.status();

            // Read body once (reqwest responses are not cloneable for streaming).
            let body_text = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    last_error = Some(EverEvoError::LlmProvider(format!(
                        "Failed to read response body: {e}"
                    )));
                    continue;
                }
            };

            let json: serde_json::Value = match serde_json::from_str(&body_text) {
                Ok(v) => v,
                Err(e) => {
                    // Non-JSON response — might be a reverse proxy error page
                    if HttpClient::is_retryable(status) {
                        last_error = Some(EverEvoError::LlmProvider(format!(
                            "Non-JSON response (HTTP {}), attempt {}/{}",
                            status.as_u16(),
                            attempt + 1,
                            HttpClient::MAX_RETRIES + 1
                        )));
                        continue;
                    }
                    return Err(EverEvoError::LlmProvider(format!(
                        "Invalid JSON response: {e}"
                    )));
                }
            };

            if !status.is_success() {
                let classified = Self::classify_http_error(status, &json.to_string());
                if HttpClient::is_retryable(status) {
                    last_error = Some(EverEvoError::LlmProvider(format!(
                        "{classified} (attempt {}/{})",
                        attempt + 1,
                        HttpClient::MAX_RETRIES + 1
                    )));
                    continue;
                }
                // Client errors (400, 401, 403, 404) — don't retry
                return Err(EverEvoError::LlmProvider(classified));
            }

            return Ok(Self::parse_response(&json));
        }

        // All retries exhausted
        Err(last_error.unwrap_or_else(|| {
            EverEvoError::LlmProvider(format!(
                "LLM request failed after {} attempts",
                HttpClient::MAX_RETRIES + 1
            ))
        }))
    }
}

impl HttpClient {
    /// Maximum retries for transient LLM API failures (429, 5xx, timeout).
    const MAX_RETRIES: u32 = 3;
    /// Base backoff in milliseconds.
    const BASE_BACKOFF_MS: u64 = 1000;

    /// Whether an HTTP status is retryable (transient server/rate-limit errors).
    fn is_retryable(status: reqwest::StatusCode) -> bool {
        status.as_u16() == 429 || status.is_server_error()
    }

    fn parse_response(json: &serde_json::Value) -> LlmResponse {
        if Self::guess_format(json) == "anthropic" {
            let content = json["content"].as_array().and_then(|blocks| {
                blocks
                    .iter()
                    .find(|b| b["type"] == "text")
                    .and_then(|b| b["text"].as_str().map(|s| s.to_string()))
            });
            let tool_calls: Vec<_> = json["content"].as_array().map_or(vec![], |blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "tool_use")
                    .map(|b| everevo_core::types::ToolCall {
                        id: b["id"].as_str().unwrap_or("").into(),
                        name: b["name"].as_str().unwrap_or("").into(),
                        arguments: b["input"].clone(),
                    })
                    .collect()
            });
            LlmResponse {
                content,
                tool_calls: tool_calls.clone(),
                finish_reason: if !tool_calls.is_empty() {
                    everevo_core::llm::FinishReason::ToolCalls
                } else {
                    everevo_core::llm::FinishReason::Stop
                },
            }
        } else {
            let choice = &json["choices"][0];
            let content = choice["message"]["content"].as_str().map(|s| s.to_string());
            let tool_calls: Vec<_> =
                choice["message"]["tool_calls"]
                    .as_array()
                    .map_or(vec![], |tcs| {
                        tcs.iter()
                            .map(|tc| everevo_core::types::ToolCall {
                                id: tc["id"].as_str().unwrap_or("").into(),
                                name: tc["function"]["name"].as_str().unwrap_or("").into(),
                                arguments: serde_json::from_str(
                                    tc["function"]["arguments"].as_str().unwrap_or("{}"),
                                )
                                .unwrap_or(serde_json::json!({})),
                            })
                            .collect()
                    });
            let finish = choice["finish_reason"].as_str().unwrap_or("stop");
            LlmResponse {
                content,
                tool_calls: tool_calls.clone(),
                finish_reason: if finish == "tool_calls" || !tool_calls.is_empty() {
                    everevo_core::llm::FinishReason::ToolCalls
                } else {
                    everevo_core::llm::FinishReason::Stop
                },
            }
        }
    }

    fn guess_format(json: &serde_json::Value) -> &str {
        if json.get("type").is_some() {
            "anthropic"
        } else {
            "openai"
        }
    }

    /// True streaming — tokens arrive via channel as they come from the HTTP response.
    pub async fn stream_chat(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, EverEvoError> {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(256);
        let api_format = self.api_format.clone();
        let endpoint = self.endpoint();
        let api_key = self.api_key.clone();
        let body = self.build_body(messages, tools, true);

        let client = self.client.clone();
        tokio::spawn(async move {
            let mut req = client.post(&endpoint).json(&body);
            if api_format == "anthropic" {
                req = req
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", "2023-06-01");
            } else {
                req = req.header("Authorization", format!("Bearer {}", api_key));
            }

            tracing::debug!(%endpoint, msg_count = body["messages"].as_array().map(|a| a.len()).unwrap_or(0), tool_count = body.get("tools").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0), "LLM request");

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    let msg = if e.is_connect() {
                        format!("Connection failed — cannot reach {endpoint}. Check network, VPN, or base_url in config.")
                    } else if e.is_timeout() {
                        "Request timed out after 10 minutes — the model may be overloaded. Try a shorter prompt or different model.".to_string()
                    } else {
                        format!("HTTP error: {e}")
                    };
                    tracing::error!(%e, "LLM HTTP request failed");
                    let _ = tx.send(StreamEvent::Text(msg)).await;
                    let _ = tx.send(StreamEvent::Done).await;
                    return;
                }
            };
            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                let classified = Self::classify_http_error(status, &body_text);
                tracing::error!(%status, %body_text, "LLM API error");
                let _ = tx.send(StreamEvent::Text(classified)).await;
                let _ = tx.send(StreamEvent::Done).await;
                return;
            }

            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            let mut detected_format: Option<&str> = None;
            let mut active_tool_id: Option<String> = None;
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                // Check cancellation periodically (every chunk)
                if cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                    tracing::info!("LLM stream cancelled by user");
                    let _ = tx.send(StreamEvent::Done).await;
                    return;
                }
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf = buf[pos + 1..].to_string();
                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            let _ = tx.send(StreamEvent::Done).await;
                            return;
                        }
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                            if detected_format.is_none() {
                                if json.get("type").is_some() {
                                    detected_format = Some("anthropic");
                                } else if json.get("choices").is_some() {
                                    detected_format = Some("openai");
                                }
                                tracing::debug!(
                                    format = detected_format,
                                    "SSE format auto-detected"
                                );
                                // First event received — the model is responding
                                tracing::info!(
                                    format = detected_format,
                                    "LLM streaming started (first SSE event)"
                                );
                            }

                            match detected_format {
                                Some("anthropic") => match json["type"].as_str() {
                                    Some("content_block_start") => {
                                        let cb = &json["content_block"];
                                        match cb["type"].as_str() {
                                            Some("tool_use") => {
                                                let id = cb["id"].as_str().unwrap_or("");
                                                let name = cb["name"].as_str().unwrap_or("");
                                                active_tool_id = Some(id.to_string());
                                                tracing::info!(
                                                    tool_name = name,
                                                    tool_id = id,
                                                    "LLM tool call start"
                                                );
                                                let _ = tx
                                                    .send(StreamEvent::ToolCallStart {
                                                        id: id.into(),
                                                        name: name.into(),
                                                    })
                                                    .await;
                                            }
                                            _ => {
                                                active_tool_id = None;
                                            }
                                        }
                                    }
                                    Some("content_block_delta") => {
                                        let delta = &json["delta"];
                                        match delta["type"].as_str() {
                                            Some("thinking_delta") => {
                                                if let Some(t) = delta["thinking"].as_str() {
                                                    let _ = tx
                                                        .send(StreamEvent::Thinking(t.into()))
                                                        .await;
                                                }
                                            }
                                            Some("text_delta") => {
                                                if let Some(t) = delta["text"].as_str() {
                                                    let _ =
                                                        tx.send(StreamEvent::Text(t.into())).await;
                                                }
                                            }
                                            Some("input_json_delta") => {
                                                if let Some(args) = delta["partial_json"].as_str() {
                                                    let id =
                                                        active_tool_id.clone().unwrap_or_default();
                                                    let _ = tx
                                                        .send(StreamEvent::ToolCallArg {
                                                            id,
                                                            arg_delta: args.into(),
                                                        })
                                                        .await;
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                    Some("content_block_stop") => {
                                        active_tool_id = None;
                                    }
                                    Some("message_stop") => {
                                        tracing::info!("LLM stream ended (message_stop)");
                                        let _ = tx.send(StreamEvent::Done).await;
                                        return;
                                    }
                                    _ => {}
                                },
                                Some("openai") => {
                                    let choice = &json["choices"][0];
                                    if let Some(r) = choice["delta"]["reasoning_content"].as_str() {
                                        let _ = tx.send(StreamEvent::Thinking(r.into())).await;
                                    }
                                    if let Some(delta) = choice["delta"]["content"].as_str() {
                                        let _ = tx.send(StreamEvent::Text(delta.into())).await;
                                    }
                                    if let Some(tcs) = choice["delta"]["tool_calls"].as_array() {
                                        for tc in tcs {
                                            if let Some(id) = tc["id"].as_str() {
                                                let name =
                                                    tc["function"]["name"].as_str().unwrap_or("");
                                                let _ = tx
                                                    .send(StreamEvent::ToolCallStart {
                                                        id: id.into(),
                                                        name: name.into(),
                                                    })
                                                    .await;
                                            }
                                            if let Some(args) = tc["function"]["arguments"].as_str()
                                            {
                                                let id = tc["id"].as_str().unwrap_or("");
                                                let _ = tx
                                                    .send(StreamEvent::ToolCallArg {
                                                        id: id.into(),
                                                        arg_delta: args.into(),
                                                    })
                                                    .await;
                                            }
                                        }
                                    }
                                    if choice["finish_reason"].as_str() == Some("stop") {
                                        tracing::info!("LLM stream ended (finish_reason=stop)");
                                        let _ = tx.send(StreamEvent::Done).await;
                                        return;
                                    }
                                    if choice["finish_reason"].as_str() == Some("tool_calls") {
                                        tracing::info!(
                                            "LLM stream ended (finish_reason=tool_calls)"
                                        );
                                        let _ = tx.send(StreamEvent::Done).await;
                                        return;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            tracing::info!("LLM stream ended (connection closed)");
            let _ = tx.send(StreamEvent::Done).await;
        });

        Ok(rx)
    }
}
