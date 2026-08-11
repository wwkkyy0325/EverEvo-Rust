//! A2A wire types — follows the A2A v0.3.0 specification.
//!
//! ## Type hierarchy
//!
//! ```text
//! AgentCard          — discovery document at /.well-known/agent.json
//!   AgentSkill       — declared capability
//!
//! Message            — a communication turn
//!   Part             — TextPart | FilePart | DataPart
//!
//! Task               — unit of work with lifecycle
//!   TaskStatus       — state + optional message
//!   Artifact         — agent-generated output
//!
//! TaskSendParams     — request body for message/send
//! TaskQueryParams    — request body for tasks/get
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Agent Card (Discovery) ─────────────────────────────────────────────────

/// The Agent Card — served at `GET /.well-known/agent.json`.
/// External agents discover EverEvo's capabilities via this document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    #[serde(default)]
    pub version: String,
    #[serde(default = "default_protocol_version")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: AgentCapabilities,
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
    #[serde(default)]
    pub default_input_modes: Vec<String>,
    #[serde(default)]
    pub default_output_modes: Vec<String>,
}

fn default_protocol_version() -> String {
    "0.3.0".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCapabilities {
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub push_notifications: bool,
    #[serde(default)]
    pub state_transition_history: bool,
    #[serde(default)]
    pub extensions: Vec<AgentExtension>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExtension {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub params: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub examples: Vec<String>,
    #[serde(default)]
    pub input_modes: Vec<String>,
    #[serde(default)]
    pub output_modes: Vec<String>,
}

impl AgentSkill {
    pub fn new(id: &str, name: &str, description: &str, tags: Vec<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            tags,
            examples: vec![],
            input_modes: vec!["text".into()],
            output_modes: vec!["text".into()],
        }
    }
}

// ── Message & Parts ───────────────────────────────────────────────────────

/// A communication turn — sent by user or agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aMessage {
    pub role: String, // "user" | "agent"
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl A2aMessage {
    pub fn user(parts: Vec<Part>) -> Self {
        Self {
            role: "user".into(),
            parts,
            metadata: None,
        }
    }

    pub fn agent(parts: Vec<Part>) -> Self {
        Self {
            role: "agent".into(),
            parts,
            metadata: None,
        }
    }

    /// Extract all text content from text parts, joined.
    pub fn text_content(&self) -> Option<String> {
        let texts: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n"))
        }
    }
}

/// A single content part in a message or artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Part {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "file")]
    File {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes: Option<String>, // base64
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    #[serde(rename = "data")]
    Data {
        #[serde(default)]
        data: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        schema: Option<String>,
    },
}

impl Part {
    pub fn text(content: &str) -> Self {
        Part::Text {
            text: content.into(),
        }
    }

    pub fn file_uri(name: &str, mime: &str, uri: &str) -> Self {
        Part::File {
            name: Some(name.into()),
            mime_type: Some(mime.into()),
            bytes: None,
            uri: Some(uri.into()),
        }
    }

    pub fn data(value: serde_json::Value) -> Self {
        Part::Data {
            data: value,
            schema: None,
        }
    }
}

// ── Task ──────────────────────────────────────────────────────────────────

/// A unit of work with full lifecycle tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aTask {
    pub id: String,
    #[serde(default)]
    pub context_id: String,
    pub status: TaskStatus,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<TaskStateRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl A2aTask {
    pub fn new(task_id: &str, context_id: &str) -> Self {
        Self {
            id: task_id.into(),
            context_id: context_id.into(),
            status: TaskStatus::new(TaskState::Submitted),
            artifacts: vec![],
            history: None,
            metadata: None,
        }
    }
}

/// Task state — follows the A2A 9-state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Submitted,
    Working,
    #[serde(rename = "input-required")]
    InputRequired,
    #[serde(rename = "auth-required")]
    AuthRequired,
    Completed,
    Failed,
    Canceled,
    Rejected,
    Unknown,
}

/// The current status of a task: state + optional message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<A2aMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

impl TaskStatus {
    pub fn new(state: TaskState) -> Self {
        Self {
            state,
            message: None,
            timestamp: Some(Utc::now()),
        }
    }

    pub fn with_message(state: TaskState, message: A2aMessage) -> Self {
        Self {
            state,
            message: Some(message),
            timestamp: Some(Utc::now()),
        }
    }
}

/// Historical record of a task state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateRecord {
    pub state: TaskState,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Agent-generated output artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl Artifact {
    pub fn new(parts: Vec<Part>, name: &str) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            parts,
            metadata: None,
        }
    }
}

// ── JSON-RPC 2.0 Envelope ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String, // "2.0"
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    pub id: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            result: Some(result),
            error: None,
            id,
        }
    }

    pub fn error(id: serde_json::Value, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
            id,
        }
    }
}

// ── Task RPC params ───────────────────────────────────────────────────────

/// Request body for `message/send` and `message/stream`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSendParams {
    pub message: A2aMessage,
    #[serde(default)]
    pub context_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub blocking: Option<bool>,
    #[serde(default)]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Request body for `tasks/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListTasksParams {
    #[serde(default)]
    pub state: Option<TaskState>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub page_token: Option<String>,
}

/// A streaming event emitted during `tasks/sendSubscribe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StreamEvent {
    #[serde(rename = "task")]
    Task { task: A2aTask },
    #[serde(rename = "status-update")]
    StatusUpdate {
        task_id: String,
        state: TaskState,
        message: Option<String>,
    },
    #[serde(rename = "artifact-update")]
    ArtifactUpdate { task_id: String, artifact: Artifact },
}

/// Request body for `tasks/get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskQueryParams {
    pub id: String,
    #[serde(default)]
    pub history_length: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_state_serde() {
        let json = serde_json::to_string(&TaskState::Submitted).unwrap();
        assert_eq!(json, r#""submitted""#);

        let json = serde_json::to_string(&TaskState::InputRequired).unwrap();
        assert_eq!(json, r#""input-required""#);

        let state: TaskState = serde_json::from_str(r#""completed""#).unwrap();
        assert_eq!(state, TaskState::Completed);
    }

    #[test]
    fn test_part_serde() {
        let text = Part::text("hello");
        let json = serde_json::to_string(&text).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains("hello"));

        let file = Part::file_uri("img.png", "image/png", "https://example.com/1.png");
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains(r#""type":"file""#));

        let roundtrip: Part = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Part::File { name, uri, .. } => {
                assert_eq!(name.unwrap(), "img.png");
                assert_eq!(uri.unwrap(), "https://example.com/1.png");
            }
            _ => panic!("expected file part"),
        }
    }

    #[test]
    fn test_agent_card_serde() {
        let card = AgentCard {
            name: "Test".into(),
            description: "Desc".into(),
            url: "http://localhost".into(),
            version: "1.0".into(),
            protocol_version: "0.3.0".into(),
            capabilities: AgentCapabilities::default(),
            skills: vec![],
            default_input_modes: vec!["text".into()],
            default_output_modes: vec!["text".into()],
        };
        let json = serde_json::to_string_pretty(&card).unwrap();
        assert!(json.contains("Test"));
        let _: AgentCard = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_jsonrpc_envelope() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "message/send".into(),
            params: serde_json::json!({"message": {"role": "user", "parts": [{"type": "text", "text": "hi"}]}}),
            id: serde_json::Value::Number(1.into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("message/send"));
    }

    #[test]
    fn test_message_text_content() {
        let msg = A2aMessage::user(vec![Part::text("hello"), Part::text("world")]);
        assert_eq!(msg.text_content(), Some("hello\nworld".into()));

        let msg = A2aMessage::agent(vec![Part::file_uri("x", "text/plain", "/x")]);
        assert_eq!(msg.text_content(), None);
    }
}
