//! LLM Provider abstraction — trait + shared types.
//!
//! Lives in `everevo-core` so ANY crate can implement an LLM backend
//! without depending on `everevo-agent`. Same pattern as `Tool` trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::ToolCall;
use crate::EverEvoError;

// ── Trait ───────────────────────────────────────────────────────────────

/// Abstract LLM provider — implement for each backend (Anthropic, OpenAI, Ollama…).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a conversation and get a response. May contain text, tool calls, or both.
    async fn chat(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
    ) -> Result<LlmResponse, EverEvoError>;

    /// Stream a response token-by-token. Default impl falls back to `chat()`.
    async fn chat_stream(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
    ) -> Result<Vec<StreamEvent>, EverEvoError> {
        let resp = self.chat(messages, tools).await?;
        Ok(vec![StreamEvent::Text(resp.content.unwrap_or_default())])
    }

    /// Stream a response as a channel of events. Default impl wraps
    /// [`LlmProvider::chat_stream`] into a channel; providers with a native
    /// streaming transport override this (e.g. `HttpClient`). The `cancel`
    /// token lets the caller abort a long-running stream.
    async fn stream_chat(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, EverEvoError> {
        let events = self.chat_stream(messages, tools).await?;
        if let Some(token) = cancel {
            token.cancel();
        }
        let (tx, rx) = tokio::sync::mpsc::channel(events.len().max(1));
        for ev in events {
            let _ = tx.send(ev).await;
        }
        Ok(rx)
    }

    /// Server-side (provider-executed) tool the API supports natively, e.g.
    /// `web_search_20250305`. Returns `Some(schema)` to declare the tool to
    /// the model; the provider executes it within the turn (the loop must NOT
    /// dispatch it as a client tool). Default: no native tool.
    fn native_web_search_tool(&self) -> Option<ToolSchema> {
        None
    }
}

// ── Message Types ───────────────────────────────────────────────────────

/// A message in the LLM conversation format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
    /// Chain-of-thought / reasoning content (DeepSeek V4, Claude extended thinking).
    /// Must be round-tripped back to the API in multi-turn conversations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Image attachments (base64). Empty = text-only. Carried in-memory only
    /// (not persisted to DB) — feeds screenshots to vision-capable LLMs.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub images: Vec<ImageData>,
}

/// A base64-encoded image attachment for multimodal (vision) messages.
/// Carried alongside text `content`; serialized as image content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    /// Base64-encoded image bytes (no `data:` prefix).
    pub data: String,
    /// MIME type, e.g. `"image/png"`.
    pub mime_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmRole {
    System,
    User,
    Assistant,
    Tool,
}

/// JSON Schema for a tool (LLM function calling format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// Server-side (provider-executed) tool type, e.g. `web_search_20250305`.
    /// When set, the schema is emitted WITHOUT an `input_schema` so the API
    /// executes the tool server-side (model issues `server_tool_use`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_type: Option<String>,
}

// ── Response Types ──────────────────────────────────────────────────────

/// Response from the LLM — may contain text, tool calls, or both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Error,
}

/// Events emitted during streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamEvent {
    /// A token of the model's chain-of-thought / reasoning (thinking phase).
    Thinking(String),
    /// A token of the final response text.
    Text(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArg {
        id: String,
        arg_delta: String,
    },
    /// A server-side (provider-executed) tool call, e.g. native web search.
    /// The provider runs the tool and injects the result within the same turn;
    /// the loop must NOT dispatch it as a client tool.
    ServerToolUse {
        name: String,
    },
    /// Stream completed. Carries real token counts from the LLM API.
    Done {
        input_tokens: u32,
        output_tokens: u32,
        /// Provider `stop_reason` (`end_turn` | `max_tokens` | `tool_use` | ...).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
    },
    /// Terminal error from the LLM provider (auth, bad request, quota).
    /// The agent loop surfaces this as a real error (SSE `error` event), so
    /// it is never scored as the model's answer by the GAIA harness.
    Error(String),
}

// ── Constructors ────────────────────────────────────────────────────────

impl LlmMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::System,
            content: content.into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            images: Vec::new(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::User,
            content: content.into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            images: Vec::new(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Assistant,
            content: content.into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            images: Vec::new(),
        }
    }
    pub fn tool(content: impl Into<String>, call_id: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Tool,
            content: content.into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            images: Vec::new(),
        }
    }
}

impl std::fmt::Display for LlmRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System => write!(f, "system"),
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
            Self::Tool => write!(f, "tool"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_message_constructors() {
        assert!(matches!(LlmMessage::system("sys").role, LlmRole::System));
        assert!(matches!(LlmMessage::user("hi").role, LlmRole::User));
        assert_eq!(
            LlmMessage::tool("out", "call_1").tool_call_id.unwrap(),
            "call_1"
        );
    }

    #[test]
    fn test_llm_role_serde() {
        let json = serde_json::to_string(&LlmRole::User).unwrap();
        assert_eq!(json, r#""user""#);
        let role: LlmRole = serde_json::from_str(r#""assistant""#).unwrap();
        assert_eq!(role, LlmRole::Assistant);
    }
}
