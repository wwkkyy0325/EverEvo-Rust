//! Shared domain types used across all crates.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Session ────────────────────────────────────────────────────────────

/// A conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

// ── Message ────────────────────────────────────────────────────────────

/// Role of a message in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: Role,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Wire-format tool result for SSE streaming (distinct from `tool::ToolOutput`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPayload {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

// ── Chat ───────────────────────────────────────────────────────────────

/// Incoming chat request from the frontend.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatRequest {
    #[serde(default)]
    pub session_id: Option<Uuid>,
    pub message: String,
}

/// SSE event types sent to the frontend during agent processing.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
/// Previously named StreamEvent — renamed to avoid collision with everevo_core::llm::StreamEvent.
pub enum SseEvent {
    /// LLM is thinking (text token).
    Thinking {
        token: String,
    },
    /// A tool is about to be called.
    ToolCallStart {
        tool_call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// A tool execution result.
    ToolCallEnd {
        tool_call_id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    /// The agent's final response is complete.
    Done {
        session_id: Uuid,
        message_id: Uuid,
    },
    /// An error occurred.
    Error {
        message: String,
    },
}

// ── LLM Provider ───────────────────────────────────────────────────────

/// Supported LLM providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProviderKind {
    Anthropic,
    OpenAI,
    Ollama,
}

/// Configuration for a single LLM provider.
#[derive(Clone, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    pub kind: LlmProviderKind,
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    pub model: String,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
}

impl std::fmt::Debug for LlmProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmProviderConfig")
            .field("kind", &self.kind)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

// ── Tool ───────────────────────────────────────────────────────────────

/// Risk level determines which sandbox tier to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    /// Safe to run in WASM or directly.
    Low,
    /// Needs Docker/filesystem isolation.
    Medium,
    /// Requires explicit user confirmation before execution.
    High,
}

// ── Knowledge Graph ────────────────────────────────────────────────────

/// An entity extracted from conversation or ingested from documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: Uuid,
    pub name: String,
    pub entity_type: String,
    pub properties: serde_json::Value,
    pub session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// A relation between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub id: Uuid,
    pub from_entity: String,
    pub relation_type: String,
    pub to_entity: String,
    pub properties: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

// ── RAG ────────────────────────────────────────────────────────────────

/// A chunk of a document stored in the vector database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    pub id: String,
    pub document_id: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub chunk_index: usize,
}

/// A search result from the RAG pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk: DocumentChunk,
    pub score: f32,
}
