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
    Done,
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
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::User,
            content: content.into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Assistant,
            content: content.into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn tool(content: impl Into<String>, call_id: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Tool,
            content: content.into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
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
