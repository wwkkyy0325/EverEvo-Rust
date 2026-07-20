//! LLM Provider implementations — Mock (testing) + HttpClient (real).
//! Uses reqwest directly — Anthropic + OpenAI-compatible, no model name restrictions.

use async_trait::async_trait;

use everevo_core::llm::{FinishReason, LlmMessage, LlmProvider, LlmResponse, StreamEvent, ToolSchema};
use everevo_core::EverEvoError;

// ── HTTP Client ─────────────────────────────────────────────────────────

pub struct HttpClient {
    api_format: String,    // "anthropic" | "openai"
    api_key: String,
    base_url: String,
    model: String,
}

impl HttpClient {
    pub fn new(api_format: &str, api_key: &str, base_url: &str, model: &str) -> Self {
        Self { api_format: api_format.into(), api_key: api_key.into(), base_url: base_url.into(), model: model.into() }
    }

    fn endpoint(&self) -> String {
        match self.api_format.as_str() {
            "anthropic" => format!("{}/v1/messages", self.base_url.trim_end_matches('/')),
            _ => format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
        }
    }

    fn build_body(&self, messages: &[LlmMessage], tools: &[ToolSchema], stream: bool) -> serde_json::Value {
        if self.api_format == "anthropic" {
            self.build_anthropic_body(messages, tools, stream)
        } else {
            self.build_openai_body(messages, tools, stream)
        }
    }

    fn build_anthropic_body(&self, messages: &[LlmMessage], tools: &[ToolSchema], stream: bool) -> serde_json::Value {
        let msgs: Vec<serde_json::Value> = messages.iter().map(|m| {
            // Anthropic protocol: tool results MUST be user messages with tool_result blocks.
            // The `tool` role is an OpenAI invention; Anthropic rejects it.
            let is_tool_result = m.tool_call_id.is_some();
            let role_str = if is_tool_result { "user".to_string() } else { m.role.to_string() };

            // Build content blocks array
            let mut content_blocks: Vec<serde_json::Value> = Vec::new();

            // Add thinking block if present (required for DeepSeek V4 round-trips)
            if let Some(ref thinking) = m.thinking {
                if !thinking.is_empty() {
                    content_blocks.push(serde_json::json!({
                        "type": "thinking",
                        "thinking": thinking,
                    }));
                }
            }

            // Add text content (skip for tool results — they must only have tool_result blocks)
            if !is_tool_result {
                let has_thinking_or_tools = m.thinking.as_ref().map_or(false, |t| !t.is_empty()) || m.tool_calls.is_some();
                if !m.content.is_empty() || !has_thinking_or_tools {
                    content_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": m.content,
                    }));
                }
            }
            // Add tool_use blocks
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
            // Tool results — may be a single or combined (pipe-separated IDs)
            if is_tool_result {
                let Some(raw_id) = m.tool_call_id.as_ref() else {
                    tracing::warn!("Tool result message without tool_call_id, skipping");
                    return serde_json::json!({"role": "user", "content": ""});
                };
                if raw_id.contains('|') {
                    // Multiple tool results merged: parse the JSON array
                    if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&m.content) {
                        for item in &items {
                            content_blocks.push(serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": item["i"].as_str().unwrap_or(""),
                                "content": item["c"].as_str().unwrap_or(""),
                            }));
                        }
                    }
                } else {
                    content_blocks.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": raw_id,
                        "content": m.content,
                    }));
                }
            }

            serde_json::json!({ "role": role_str, "content": content_blocks })
        }).collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": 4096,
            "messages": msgs,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.parameters,
            })).collect::<Vec<_>>());
        }
        if stream { body["stream"] = serde_json::json!(true); }
        body
    }

    fn build_openai_body(&self, messages: &[LlmMessage], tools: &[ToolSchema], stream: bool) -> serde_json::Value {
        let mut msgs: Vec<serde_json::Value> = Vec::new();
        for m in messages {
            let role = m.role.to_string();

            // Handle merged multi-tool results: split into separate messages
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

            let mut msg = serde_json::json!({ "role": role, "content": m.content });
            if let Some(ref tcs) = m.tool_calls {
                msg["tool_calls"] = serde_json::json!(tcs.iter().map(|tc| serde_json::json!({
                    "id": tc.id, "type": "function", "function": { "name": tc.name, "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default() },
                })).collect::<Vec<_>>());
                msg["content"] = serde_json::Value::Null;
            }
            if m.tool_call_id.is_some() {
                msg["role"] = serde_json::json!("tool");
                msg["tool_call_id"] = serde_json::json!(m.tool_call_id);
            }
            msgs.push(msg);
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

    fn parse_response(&self, json: &serde_json::Value) -> LlmResponse {
        if self.api_format == "anthropic" {
            let content = json["content"].as_array().and_then(|blocks| {
                blocks.iter().find(|b| b["type"] == "text").and_then(|b| b["text"].as_str().map(|s| s.to_string()))
            });
            let tool_calls: Vec<_> = json["content"].as_array().map_or(vec![], |blocks| {
                blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| everevo_core::types::ToolCall {
                    id: b["id"].as_str().unwrap_or("").into(),
                    name: b["name"].as_str().unwrap_or("").into(),
                    arguments: b["input"].clone(),
                }).collect()
            });
            LlmResponse { content, tool_calls: tool_calls.clone(), finish_reason: if !tool_calls.is_empty() { FinishReason::ToolCalls } else { FinishReason::Stop } }
        } else {
            let choice = &json["choices"][0];
            let content = choice["message"]["content"].as_str().map(|s| s.to_string());
            let tool_calls: Vec<_> = choice["message"]["tool_calls"].as_array().map_or(vec![], |tcs| {
                tcs.iter().map(|tc| everevo_core::types::ToolCall {
                    id: tc["id"].as_str().unwrap_or("").into(),
                    name: tc["function"]["name"].as_str().unwrap_or("").into(),
                    arguments: serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(serde_json::json!({})),
                }).collect()
            });
            let finish = choice["finish_reason"].as_str().unwrap_or("stop");
            LlmResponse { content, tool_calls: tool_calls.clone(), finish_reason: if finish == "tool_calls" || !tool_calls.is_empty() { FinishReason::ToolCalls } else { FinishReason::Stop } }
        }
    }
}

#[async_trait]
impl LlmProvider for HttpClient {
    async fn chat(&self, messages: &[LlmMessage], tools: &[ToolSchema]) -> Result<LlmResponse, EverEvoError> {
        let body = self.build_body(messages, tools, false);
        let client = reqwest::Client::new();
        let mut req = client.post(&self.endpoint()).json(&body);
        if self.api_format == "anthropic" {
            req = req.header("x-api-key", &self.api_key).header("anthropic-version", "2023-06-01");
        } else {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let resp = req.send().await.map_err(|e| EverEvoError::LlmProvider(e.to_string()))?;
        let status = resp.status();
        let json: serde_json::Value = resp.json().await.map_err(|e| EverEvoError::LlmProvider(e.to_string()))?;
        if !status.is_success() {
            return Err(EverEvoError::LlmProvider(format!("HTTP {status}: {json}")));
        }
        Ok(self.parse_response(&json))
    }
}

impl HttpClient {
    /// True streaming — tokens arrive via channel as they come from the HTTP response.
    pub async fn stream_chat(&self, messages: &[LlmMessage], tools: &[ToolSchema]) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, EverEvoError> {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(256);
        let api_format = self.api_format.clone();
        let endpoint = self.endpoint();
        let api_key = self.api_key.clone();
        let body = self.build_body(messages, tools, true);

        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .unwrap_or_default();
            let mut req = client.post(&endpoint).json(&body);
            if api_format == "anthropic" { req = req.header("x-api-key", &api_key).header("anthropic-version", "2023-06-01"); }
            else { req = req.header("Authorization", format!("Bearer {}", api_key)); }

            tracing::debug!(%endpoint, msg_count = body["messages"].as_array().map(|a| a.len()).unwrap_or(0), tool_count = body.get("tools").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0), "LLM request");

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(%e, "LLM HTTP request failed");
                    let _ = tx.send(StreamEvent::Text(format!("[HTTP error: {e}]"))).await;
                    let _ = tx.send(StreamEvent::Done).await;
                    return;
                }
            };
            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                tracing::error!(%status, %body_text, "LLM API error");
                let _ = tx.send(StreamEvent::Text(format!("[HTTP {status}: {body_text}]"))).await;
                let _ = tx.send(StreamEvent::Done).await;
                return;
            }

            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            let mut detected_format: Option<&str> = None;
            let mut active_tool_id: Option<String> = None; // track current tool call block ID
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk { Ok(c) => c, Err(_) => continue };
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string(); buf = buf[pos+1..].to_string();
                    if line.is_empty() || line.starts_with(':') { continue; }
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" { let _ = tx.send(StreamEvent::Done).await; return; }
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                            // Auto-detect format from first data event
                            if detected_format.is_none() {
                                if json.get("type").is_some() {
                                    detected_format = Some("anthropic");
                                } else if json.get("choices").is_some() {
                                    detected_format = Some("openai");
                                }
                                tracing::debug!(format = detected_format, "SSE format auto-detected");
                            }

                            match detected_format {
                                Some("anthropic") => {
                                    match json["type"].as_str() {
                                        Some("content_block_start") => {
                                            let cb = &json["content_block"];
                                            match cb["type"].as_str() {
                                                Some("tool_use") => {
                                                    let id = cb["id"].as_str().unwrap_or("");
                                                    let name = cb["name"].as_str().unwrap_or("");
                                                    active_tool_id = Some(id.to_string());
                                                    let _ = tx.send(StreamEvent::ToolCallStart { id: id.into(), name: name.into() }).await;
                                                }
                                                Some("text") => {
                                                    active_tool_id = None;
                                                }
                                                _ => {}
                                            }
                                        }
                                        Some("content_block_delta") => {
                                            let delta = &json["delta"];
                                            match delta["type"].as_str() {
                                                Some("thinking_delta") => {
                                                    if let Some(t) = delta["thinking"].as_str() {
                                                        let _ = tx.send(StreamEvent::Thinking(t.into())).await;
                                                    }
                                                }
                                                Some("text_delta") => {
                                                    if let Some(t) = delta["text"].as_str() {
                                                        let _ = tx.send(StreamEvent::Text(t.into())).await;
                                                    }
                                                }
                                                Some("input_json_delta") => {
                                                    if let Some(args) = delta["partial_json"].as_str() {
                                                        let id = active_tool_id.clone().unwrap_or_default();
                                                        let _ = tx.send(StreamEvent::ToolCallArg { id, arg_delta: args.into() }).await;
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                        Some("content_block_stop") => {
                                            active_tool_id = None;
                                        }
                                        Some("message_delta") => {
                                            if json["delta"]["stop_reason"].as_str() == Some("end_turn") {
                                                // Final turn — will receive message_stop next
                                            }
                                        }
                                        Some("message_stop") => {
                                            let _ = tx.send(StreamEvent::Done).await;
                                            return;
                                        }
                                        _ => {}
                                    }
                                }
                                Some("openai") => {
                                    let choice = &json["choices"][0];
                                    if let Some(r) = choice["delta"]["reasoning_content"].as_str() {
                                        let _ = tx.send(StreamEvent::Thinking(r.into())).await;
                                    }
                                    if let Some(delta) = choice["delta"]["content"].as_str() {
                                        let _ = tx.send(StreamEvent::Text(delta.into())).await;
                                    }
                                    // Tool calls in OpenAI format
                                    if let Some(tcs) = choice["delta"]["tool_calls"].as_array() {
                                        for tc in tcs {
                                            if let Some(id) = tc["id"].as_str() {
                                                let name = tc["function"]["name"].as_str().unwrap_or("");
                                                let _ = tx.send(StreamEvent::ToolCallStart { id: id.into(), name: name.into() }).await;
                                            }
                                            if let Some(args) = tc["function"]["arguments"].as_str() {
                                                let id = tc["id"].as_str().unwrap_or("");
                                                let _ = tx.send(StreamEvent::ToolCallArg { id: id.into(), arg_delta: args.into() }).await;
                                            }
                                        }
                                    }
                                    if choice["finish_reason"].as_str() == Some("stop") {
                                        let _ = tx.send(StreamEvent::Done).await;
                                        return;
                                    }
                                    if choice["finish_reason"].as_str() == Some("tool_calls") {
                                        let _ = tx.send(StreamEvent::Done).await;
                                        return;
                                    }
                                }
                                _ => {
                                    // Format not yet detected — skip and wait for next event
                                    tracing::trace!(?json, "SSE event before format detection");
                                }
                            }
                        }
                    }
                }
            }
            let _ = tx.send(StreamEvent::Done).await;
        });

        Ok(rx)
    }

    #[allow(dead_code)]
    async fn chat_stream(&self, messages: &[LlmMessage], tools: &[ToolSchema]) -> Result<Vec<StreamEvent>, EverEvoError> {
        let body = self.build_body(messages, tools, true);
        let client = reqwest::Client::new();
        let mut req = client.post(&self.endpoint()).json(&body);
        if self.api_format == "anthropic" {
            req = req.header("x-api-key", &self.api_key).header("anthropic-version", "2023-06-01");
        } else {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let resp = req.send().await.map_err(|e| EverEvoError::LlmProvider(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(EverEvoError::LlmProvider(format!("HTTP {status}: {text}")));
        }

        let mut events = Vec::new();
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| EverEvoError::LlmProvider(e.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer = buffer[pos+1..].to_string();
                if line.is_empty() || line.starts_with(':') { continue; }
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" { events.push(StreamEvent::Done); continue; }
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        if self.api_format == "anthropic" {
                            match json["type"].as_str() {
                                Some("content_block_delta") => {
                                    if let Some(t) = json["delta"]["text"].as_str() { events.push(StreamEvent::Text(t.into())); }
                                }
                                Some("content_block_start") => {
                                    if json["content_block"]["type"] == "tool_use" {
                                        let id = json["content_block"]["id"].as_str().unwrap_or("");
                                        let name = json["content_block"]["name"].as_str().unwrap_or("");
                                        events.push(StreamEvent::ToolCallStart { id: id.into(), name: name.into() });
                                    }
                                }
                                Some("message_delta") => events.push(StreamEvent::Done),
                                _ => {}
                            }
                        } else {
                            let choice = &json["choices"][0];
                            if let Some(delta) = choice["delta"]["content"].as_str() { events.push(StreamEvent::Text(delta.into())); }
                            if let Some(tcs) = choice["delta"]["tool_calls"].as_array() {
                                for tc in tcs {
                                    let idx = tc["index"].as_u64().unwrap_or(0);
                                    if let Some(id) = tc["id"].as_str() { events.push(StreamEvent::ToolCallStart { id: id.into(), name: format!("tool_{idx}") }); }
                                    if let Some(args) = tc["function"]["arguments"].as_str() { events.push(StreamEvent::ToolCallArg { id: tc["id"].as_str().unwrap_or("").into(), arg_delta: args.into() }); }
                                }
                            }
                            if choice["finish_reason"].as_str().is_some() { events.push(StreamEvent::Done); }
                        }
                    }
                }
            }
        }
        if !events.iter().any(|e| matches!(e, StreamEvent::Done)) { events.push(StreamEvent::Done); }
        Ok(events)
    }
}

// ── Mock ────────────────────────────────────────────────────────────────

pub struct MockLlmProvider {
    responses: tokio::sync::Mutex<Vec<LlmResponse>>,
    stream_events: tokio::sync::Mutex<Vec<Vec<StreamEvent>>>,
    call_log: tokio::sync::Mutex<Vec<Vec<LlmMessage>>>,
}

impl MockLlmProvider {
    pub fn new() -> Self {
        Self {
            responses: tokio::sync::Mutex::new(Vec::new()),
            stream_events: tokio::sync::Mutex::new(Vec::new()),
            call_log: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn with_text(self, text: impl Into<String>) -> Self {
        self.responses.blocking_lock().push(LlmResponse {
            content: Some(text.into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
        });
        self
    }

    pub fn with_tool_call(self, name: impl Into<String>, arguments: serde_json::Value) -> Self {
        self.responses.blocking_lock().push(LlmResponse {
            content: None,
            tool_calls: vec![everevo_core::types::ToolCall {
                id: format!("call_{}", uuid::Uuid::new_v4()),
                name: name.into(),
                arguments,
            }],
            finish_reason: FinishReason::ToolCalls,
        });
        self
    }

    pub fn with_response(self, resp: LlmResponse) -> Self {
        self.responses.blocking_lock().push(resp);
        self
    }

    pub fn call_log(&self) -> Vec<Vec<LlmMessage>> {
        self.call_log.blocking_lock().clone()
    }

    pub fn call_count(&self) -> usize {
        self.call_log.blocking_lock().len()
    }
}

impl Default for MockLlmProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn chat(
        &self,
        messages: &[LlmMessage],
        _tools: &[ToolSchema],
    ) -> Result<LlmResponse, EverEvoError> {
        self.call_log.lock().await.push(messages.to_vec());
        let resp = {
            let mut r = self.responses.lock().await;
            if r.is_empty() { None } else { Some(r.remove(0)) }
        };
        resp.ok_or_else(|| EverEvoError::LlmProvider("Mock: no more responses".into()))
    }

    async fn chat_stream(
        &self,
        messages: &[LlmMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<StreamEvent>, EverEvoError> {
        self.call_log.lock().await.push(messages.to_vec());
        let e = {
            let mut s = self.stream_events.lock().await;
            if s.is_empty() { None } else { Some(s.remove(0)) }
        };
        match e {
            Some(ev) => Ok(ev),
            None => {
                let r = self.chat(messages, &[]).await?;
                Ok(vec![StreamEvent::Text(r.content.unwrap_or_default())])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn test_mock_basic() { let m = MockLlmProvider::new().with_text("hello"); assert_eq!(m.chat(&[LlmMessage::user("hi")], &[]).await.unwrap().content.unwrap(), "hello"); }
}
