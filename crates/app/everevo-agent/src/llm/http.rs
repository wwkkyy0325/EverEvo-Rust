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

mod body;
mod proxy;
mod response;

pub use proxy::{build_llm_http_client, detect_proxy, detect_proxy_sync};

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::llm::{LlmMessage, LlmProvider, LlmResponse, StreamEvent, ToolSchema};
use everevo_core::EverEvoError;

// ── Circuit breaker ───────────────────────────────────────────────────────

/// Consecutive transient failures (429/5xx) before the circuit opens.
const CIRCUIT_OPEN_THRESHOLD: u32 = 5;
/// Cooldown while the circuit is open (fast-fail), before a half-open probe.
const CIRCUIT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// Lightweight provider circuit breaker (`Closed → Open → HalfOpen → Closed`).
/// Shared across sessions because `HttpClient` is `Arc`-shared; protects the
/// whole app from hammering a down / rate-limited provider (research: rate
/// limiting is the single largest reliability issue in agent systems).
#[derive(Debug, Default)]
pub(crate) struct CircuitBreaker {
    consecutive_failures: u32,
    open_until: Option<std::time::Instant>,
}

impl CircuitBreaker {
    /// Returns true if a request may proceed. While open (cooldown not
    /// elapsed), calls fast-fail without hitting the API; after the cooldown a
    /// half-open probe is allowed.
    fn allows(&mut self) -> bool {
        if let Some(t) = self.open_until {
            if std::time::Instant::now() < t {
                return false;
            }
            self.open_until = None; // cooldown elapsed → half-open probe
        }
        true
    }

    /// Record a transient failure; opens the circuit after the threshold.
    fn failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= CIRCUIT_OPEN_THRESHOLD {
            self.open_until = Some(std::time::Instant::now() + CIRCUIT_COOLDOWN);
            tracing::warn!(
                failures = self.consecutive_failures,
                cooldown_s = CIRCUIT_COOLDOWN.as_secs(),
                "LLM provider circuit opened — fast-failing until cooldown"
            );
        }
    }

    /// Record success; closes the circuit and resets the failure counter.
    fn success(&mut self) {
        if self.consecutive_failures > 0 || self.open_until.is_some() {
            tracing::info!(
                failures = self.consecutive_failures,
                "LLM provider circuit closed"
            );
        }
        self.consecutive_failures = 0;
        self.open_until = None;
    }
}

// ── Client ────────────────────────────────────────────────────────────────

/// LLM provider via HTTP (Anthropic Messages API or OpenAI Chat Completions).
pub struct HttpClient {
    api_format: String, // "anthropic" | "openai"
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client, // shared connection pool — created once, reused
    /// Accumulated token usage across all API calls made by this client.
    total_input_tokens: Arc<std::sync::atomic::AtomicU64>,
    total_output_tokens: Arc<std::sync::atomic::AtomicU64>,
    /// Provider circuit breaker (fast-fail while down).
    circuit: Arc<std::sync::Mutex<CircuitBreaker>>,
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
            total_input_tokens: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            total_output_tokens: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            circuit: Arc::new(std::sync::Mutex::new(CircuitBreaker::default())),
        }
    }

    /// Returns true if a request may proceed (delegates to [`CircuitBreaker`]).
    fn circuit_allows(&self) -> bool {
        self.circuit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allows()
    }

    /// Record a transient failure (delegates to [`CircuitBreaker`]).
    fn circuit_failure(&self) {
        self.circuit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failure();
    }

    /// Record success (delegates to [`CircuitBreaker`]).
    fn circuit_success(&self) {
        self.circuit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .success();
    }

    /// Return accumulated token usage across all API calls.
    pub fn token_usage(&self) -> (u64, u64) {
        (
            self.total_input_tokens
                .load(std::sync::atomic::Ordering::Relaxed),
            self.total_output_tokens
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    fn endpoint(&self) -> String {
        match self.api_format.as_str() {
            "anthropic" => format!("{}/v1/messages", self.base_url.trim_end_matches('/')),
            _ => format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
        }
    }
}

#[async_trait]
impl LlmProvider for HttpClient {
    /// DeepSeek's Anthropic-compatible endpoint natively executes web search
    /// server-side (`server_tool_use` → `web_search_tool_result`) within the
    /// turn — avoiding the plugin engine cascade (Sogou rate-limits, GFW,
    /// cn.bing garbage for rare entities). Disable with `EVEREVO_NATIVE_WEB_SEARCH=0`.
    fn native_web_search_tool(&self) -> Option<ToolSchema> {
        if self.api_format == "anthropic"
            && std::env::var("EVEREVO_NATIVE_WEB_SEARCH").as_deref() != Ok("0")
        {
            Some(ToolSchema {
                name: "web_search".into(),
                description: "Server-side web search executed by the API. Use FIRST for general factual questions, current events, Wikipedia facts, or anything needing up-to-date web knowledge; supports multi-step research within a single turn.".into(),
                parameters: serde_json::json!({}),
                native_type: Some("web_search_20250305".into()),
            })
        } else {
            None
        }
    }

    async fn chat(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
    ) -> Result<LlmResponse, EverEvoError> {
        let body = self.build_body(messages, tools, false);
        let mut last_error: Option<EverEvoError> = None;

        // Circuit breaker: if the provider is down, fail fast rather than
        // spending MAX_RETRIES×backoff on every session that hits it.
        if !self.circuit_allows() {
            return Err(EverEvoError::LlmProvider(
                "LLM provider circuit is open (repeated transient failures). \
                 Try again in a moment."
                    .into(),
            ));
        }

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
                        self.circuit_failure();
                        continue;
                    }
                    self.circuit_failure();
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
                        self.circuit_failure();
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
                    self.circuit_failure();
                    continue;
                }
                // Client errors (400, 401, 403, 404) — don't retry
                return Err(EverEvoError::LlmProvider(classified));
            }

            self.circuit_success();
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

    /// Streaming transport override: HttpClient streams over SSE directly.
    /// Delegates to the inherent `stream_chat` (kept so `run_subagent`, which
    /// holds a concrete `Arc<HttpClient>`, resolves the same implementation).
    async fn stream_chat(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, EverEvoError> {
        HttpClient::stream_chat(self, messages, tools, cancel).await
    }
}

impl HttpClient {
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
        let input_counter = Arc::clone(&self.total_input_tokens);
        let output_counter = Arc::clone(&self.total_output_tokens);
        let circuit = Arc::clone(&self.circuit);
        tokio::spawn(async move {
            // Circuit breaker fast-fail: provider down → don't burn backoff.
            if !circuit.lock().unwrap_or_else(|e| e.into_inner()).allows() {
                let _ = tx
                    .send(StreamEvent::Error(
                        "LLM provider circuit is open (repeated transient failures). \
                         Try again in a moment."
                            .into(),
                    ))
                    .await;
                let _ = tx
                    .send(StreamEvent::Done {
                        input_tokens: 0,
                        output_tokens: 0,
                        stop_reason: None,
                    })
                    .await;
                return;
            }
            // Send with retries for transient failures (mirrors chat()). A
            // transient 429/5xx/connect/timeout must not silently become the
            // model's "answer" — retry before streaming. Non-retryable client
            // errors (400/401/403) surface as StreamEvent::Error so the agent
            // loop reports them as a real error (SSE `error` event) instead of
            // scoring error text as the answer.
            let mut resp = None;
            for attempt in 0..=HttpClient::MAX_RETRIES {
                if attempt > 0 {
                    let delay_ms = HttpClient::BASE_BACKOFF_MS * (1u64 << (attempt - 1)); // 1s, 2s, 4s
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                let mut req = client.post(&endpoint).json(&body);
                if api_format == "anthropic" {
                    req = req
                        .header("x-api-key", &api_key)
                        .header("anthropic-version", "2023-06-01");
                } else {
                    req = req.header("Authorization", format!("Bearer {}", api_key));
                }

                tracing::debug!(%endpoint, msg_count = body["messages"].as_array().map(|a| a.len()).unwrap_or(0), tool_count = body.get("tools").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0), "LLM request");

                let r = match req.send().await {
                    Ok(r) => r,
                    Err(e) => {
                        let retryable = e.is_connect() || e.is_timeout();
                        let msg = if e.is_connect() {
                            format!("Connection failed — cannot reach {endpoint}. Check network, VPN, or base_url in config.")
                        } else if e.is_timeout() {
                            "Request timed out after 10 minutes — the model may be overloaded. Try a shorter prompt or different model.".to_string()
                        } else {
                            format!("HTTP error: {e}")
                        };
                        if retryable && attempt < HttpClient::MAX_RETRIES {
                            tracing::warn!(attempt, %e, "LLM HTTP request failed — retrying");
                            circuit.lock().unwrap_or_else(|e| e.into_inner()).failure();
                            continue;
                        }
                        tracing::error!(%e, "LLM HTTP request failed");
                        circuit.lock().unwrap_or_else(|e| e.into_inner()).failure();
                        let _ = tx.send(StreamEvent::Error(msg)).await;
                        let _ = tx
                            .send(StreamEvent::Done {
                                input_tokens: 0,
                                output_tokens: 0,
                                stop_reason: None,
                            })
                            .await;
                        return;
                    }
                };
                if !r.status().is_success() {
                    let status = r.status();
                    let body_text = r.text().await.unwrap_or_default();
                    let classified = Self::classify_http_error(status, &body_text);
                    if HttpClient::is_retryable(status) && attempt < HttpClient::MAX_RETRIES {
                        tracing::warn!(attempt, %status, "LLM API transient error — retrying");
                        circuit.lock().unwrap_or_else(|e| e.into_inner()).failure();
                        continue;
                    }
                    tracing::error!(%status, %body_text, "LLM API error");
                    circuit.lock().unwrap_or_else(|e| e.into_inner()).failure();
                    let _ = tx.send(StreamEvent::Error(classified)).await;
                    let _ = tx
                        .send(StreamEvent::Done {
                            input_tokens: 0,
                            output_tokens: 0,
                            stop_reason: None,
                        })
                        .await;
                    return;
                }
                resp = Some(r);
                circuit.lock().unwrap_or_else(|e| e.into_inner()).success();
                break;
            }

            let mut stream = match resp {
                Some(r) => r.bytes_stream(),
                None => {
                    // Unreachable: the loop always returns or breaks with a response.
                    let _ = tx
                        .send(StreamEvent::Error(
                            "LLM request failed after retries".into(),
                        ))
                        .await;
                    let _ = tx
                        .send(StreamEvent::Done {
                            input_tokens: 0,
                            output_tokens: 0,
                            stop_reason: None,
                        })
                        .await;
                    return;
                }
            };
            let mut buf = String::new();
            let mut detected_format: Option<&str> = None;
            let mut active_tool_id: Option<String> = None;
            let mut last_stop_reason: Option<String> = None;
            let mut total_input_tokens: u32 = 0;
            let mut total_output_tokens: u32 = 0;
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                // Check cancellation periodically (every chunk)
                if cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                    tracing::info!("LLM stream cancelled by user");
                    let _ = tx
                        .send(StreamEvent::Done {
                            input_tokens: total_input_tokens,
                            output_tokens: total_output_tokens,
                            stop_reason: last_stop_reason.clone(),
                        })
                        .await;
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
                            let _ = tx
                                .send(StreamEvent::Done {
                                    input_tokens: total_input_tokens,
                                    output_tokens: total_output_tokens,
                                    stop_reason: last_stop_reason.clone(),
                                })
                                .await;
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
                                    Some("message_start") => {
                                        if let Some(u) =
                                            json["message"]["usage"]["input_tokens"].as_u64()
                                        {
                                            total_input_tokens = u as u32;
                                        }
                                    }
                                    Some("message_delta") => {
                                        if let Some(u) = json["usage"]["output_tokens"].as_u64() {
                                            total_output_tokens = u as u32;
                                        }
                                        if let Some(sr) = json["delta"]["stop_reason"].as_str() {
                                            last_stop_reason = Some(sr.to_string());
                                        }
                                    }
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
                                            Some("server_tool_use") => {
                                                // Server-side tool (e.g. native web search):
                                                // the provider executes it within this turn —
                                                // do NOT dispatch it as a client tool.
                                                let name = cb["name"].as_str().unwrap_or("");
                                                active_tool_id = None;
                                                tracing::info!(
                                                    server_tool = name,
                                                    "LLM server-side tool call (provider-executed, not dispatched)"
                                                );
                                                let _ = tx
                                                    .send(StreamEvent::ServerToolUse {
                                                        name: name.into(),
                                                    })
                                                    .await;
                                            }
                                            Some("web_search_tool_result") => {
                                                active_tool_id = None;
                                                tracing::info!(
                                                    "LLM server-side web search result received"
                                                );
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
                                        tracing::info!(
                                            input_tokens = total_input_tokens,
                                            output_tokens = total_output_tokens,
                                            "LLM stream ended (message_stop)"
                                        );
                                        input_counter.fetch_add(
                                            total_input_tokens as u64,
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                        output_counter.fetch_add(
                                            total_output_tokens as u64,
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                        let _ = tx
                                            .send(StreamEvent::Done {
                                                input_tokens: total_input_tokens,
                                                output_tokens: total_output_tokens,
                                                stop_reason: last_stop_reason.clone(),
                                            })
                                            .await;
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
                                        let _ = tx
                                            .send(StreamEvent::Done {
                                                input_tokens: total_input_tokens,
                                                output_tokens: total_output_tokens,
                                                stop_reason: last_stop_reason.clone(),
                                            })
                                            .await;
                                        return;
                                    }
                                    if choice["finish_reason"].as_str() == Some("tool_calls") {
                                        tracing::info!(
                                            "LLM stream ended (finish_reason=tool_calls)"
                                        );
                                        let _ = tx
                                            .send(StreamEvent::Done {
                                                input_tokens: total_input_tokens,
                                                output_tokens: total_output_tokens,
                                                stop_reason: last_stop_reason.clone(),
                                            })
                                            .await;
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
            let _ = tx
                .send(StreamEvent::Done {
                    input_tokens: total_input_tokens,
                    output_tokens: total_output_tokens,
                    stop_reason: last_stop_reason.clone(),
                })
                .await;
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod circuit_tests {
    use super::*;

    #[test]
    fn test_circuit_starts_closed() {
        let cb = CircuitBreaker::default();
        assert_eq!(cb.consecutive_failures, 0);
        assert!(cb.open_until.is_none());
    }

    #[test]
    fn test_circuit_opens_after_threshold() {
        let mut cb = CircuitBreaker::default();
        // Below threshold the circuit stays closed (allows() true).
        for _ in 0..CIRCUIT_OPEN_THRESHOLD - 1 {
            assert!(cb.allows());
            cb.failure();
        }
        assert!(cb.open_until.is_none());
        // The threshold failure opens it → fast-fail.
        cb.failure();
        assert!(cb.open_until.is_some());
        assert!(!cb.allows());
    }

    #[test]
    fn test_circuit_success_resets() {
        let mut cb = CircuitBreaker::default();
        cb.failure();
        cb.failure();
        cb.failure();
        assert!(cb.allows()); // still under threshold
        cb.success();
        assert_eq!(cb.consecutive_failures, 0);
        assert!(cb.open_until.is_none());
    }

    #[test]
    fn test_circuit_fast_fails_when_open() {
        let mut cb = CircuitBreaker::default();
        for _ in 0..CIRCUIT_OPEN_THRESHOLD {
            cb.failure();
        }
        assert!(!cb.allows());
        // A success (half-open probe) closes it again.
        cb.success();
        assert!(cb.allows());
        assert_eq!(cb.consecutive_failures, 0);
    }
}
