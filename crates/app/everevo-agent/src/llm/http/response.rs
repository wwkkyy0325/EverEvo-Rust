//! Response-side handling for the HTTP LLM client.
//!
//! Error classification, the retry policy (`MAX_RETRIES`, `BASE_BACKOFF_MS`,
//! `is_retryable`), and response parsing. The retry constants and helpers are
//! used by the coordinator module (`http`) in both `chat` and `stream_chat`,
//! so they are `pub(crate)`.

use super::HttpClient;
use everevo_core::llm::{FinishReason, LlmResponse};
use everevo_core::types::ToolCall;
use serde_json;

impl HttpClient {
    /// Classify HTTP error for better diagnostics.
    pub(crate) fn classify_http_error(status: reqwest::StatusCode, body: &str) -> String {
        let code = status.as_u16();
        let detail = if body.chars().count() > 300 {
            let s: String = body.chars().take(300).collect();
            s
        } else {
            body.to_string()
        };
        let detail = detail.as_str();
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

    /// Maximum retries for transient LLM API failures (429, 5xx, timeout).
    pub(crate) const MAX_RETRIES: u32 = 3;
    /// Base backoff in milliseconds.
    pub(crate) const BASE_BACKOFF_MS: u64 = 1000;

    /// Whether an HTTP status is retryable (transient server/rate-limit errors).
    pub(crate) fn is_retryable(status: reqwest::StatusCode) -> bool {
        status.as_u16() == 429 || status.is_server_error()
    }

    pub(crate) fn parse_response(json: &serde_json::Value) -> LlmResponse {
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
                    .map(|b| ToolCall {
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
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
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
                            .map(|tc| ToolCall {
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
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
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
}
