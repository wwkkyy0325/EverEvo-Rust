//! TodoWrite tool — matches Claude Code's TodoWrite behavior.
//!
//! Lets the LLM maintain a structured task list. Todos are stored in
//! per-session AppState, rendered by the frontend, and auto-persisted to
//! disk so task state survives server restarts and context compaction.
//!
//! ## Persistence
//!
//! Each session's todo list is auto-saved to `data/tasks/<session_id>.json`
//! on every update. On startup, `load_persisted_tasks()` restores all
//! previously saved task lists into the store.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ── Types (match Claude Code's TodoItem exactly) ──────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String, // imperative: "Run the tests"
    pub status: String,  // "pending" | "in_progress" | "completed"
    #[serde(rename = "activeForm")]
    pub active_form: String, // present continuous: "Running the tests"
}

/// Type alias for the shared todo store.
pub type TodoStore = Arc<RwLock<HashMap<Uuid, Vec<TodoItem>>>>;

pub fn new_todo_store() -> TodoStore {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Load persisted task lists from `data/tasks/*.json` into the store.
/// Called once on server startup.
pub async fn load_persisted_tasks(store: &TodoStore, data_dir: &std::path::Path) {
    let tasks_dir = data_dir.join("tasks");
    if !tasks_dir.exists() {
        return;
    }
    let mut loaded = 0u64;
    if let Ok(entries) = std::fs::read_dir(&tasks_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                // Extract session ID from filename (e.g., "abc-123.json" → "abc-123")
                let session_id_str = path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if let Ok(id) = Uuid::parse_str(session_id_str) {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(todos) = serde_json::from_str::<Vec<TodoItem>>(&content) {
                            store.write().await.insert(id, todos);
                            loaded += 1;
                        }
                    }
                }
            }
        }
    }
    if loaded > 0 {
        tracing::info!(loaded, "Restored persisted task lists from disk");
    }
}

// ── Tool ──────────────────────────────────────────────────────────────

pub struct TodoWriteTool {
    store: TodoStore,
    /// Directory for persisting task lists to disk. When set, every update
    /// auto-saves the session's todo list to `persist_dir/<session_id>.json`.
    persist_dir: Option<PathBuf>,
}

impl TodoWriteTool {
    pub fn new(store: TodoStore) -> Self {
        Self { store, persist_dir: None }
    }

    /// Enable disk persistence. Task lists are auto-saved to
    /// `<persist_dir>/<session_id>.json` on every update.
    pub fn with_persistence(mut self, dir: PathBuf) -> Self {
        self.persist_dir = Some(dir);
        self
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn description(&self) -> &str {
        "Use this tool to create and manage a structured task list for your current \
         session. Track progress, organize complex tasks, and demonstrate thoroughness. \
         Use proactively for multi-step tasks. Each todo needs: content (imperative form), \
         status (pending/in_progress/completed), activeForm (present continuous form). \
         Only ONE task in_progress at a time. Mark complete IMMEDIATELY after finishing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The updated todo list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "Imperative form describing what needs to be done"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current task status"
                            },
                            "activeForm": {
                                "type": "string",
                                "description": "Present continuous form shown during execution"
                            }
                        },
                        "required": ["content", "status", "activeForm"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        // Parse todos from input
        let todos: Vec<TodoItem> = serde_json::from_value(params["todos"].clone())
            .map_err(|e| EverEvoError::InvalidInput(format!("Invalid todos: {e}")))?;

        // Use session_id from params if provided, else default to global key
        let session_id = params["session_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::nil);

        let mut store = self.store.write().await;

        // If all todos are completed, clear the list (Claude Code behavior)
        let all_done = todos.iter().all(|t| t.status == "completed");
        if all_done {
            store.remove(&session_id);
            // Remove persisted file if it exists
            if let Some(ref dir) = self.persist_dir {
                let path = dir.join(format!("{session_id}.json"));
                let _ = std::fs::remove_file(&path);
            }
        } else {
            // Auto-persist to disk before releasing the lock
            if let Some(ref dir) = self.persist_dir {
                let _ = std::fs::create_dir_all(dir);
                if let Ok(json) = serde_json::to_string_pretty(&todos) {
                    let path = dir.join(format!("{session_id}.json"));
                    let _ = std::fs::write(&path, &json);
                }
            }
            store.insert(session_id, todos);
        }

        let new_todos = store.get(&session_id).cloned().unwrap_or_default();

        Ok(ToolOutput {
            content: format!(
                "Todo list updated. {} items ({} completed, {} in progress, {} pending).",
                new_todos.len(),
                new_todos.iter().filter(|t| t.status == "completed").count(),
                new_todos
                    .iter()
                    .filter(|t| t.status == "in_progress")
                    .count(),
                new_todos.iter().filter(|t| t.status == "pending").count(),
            ),
            is_error: false,
        })
    }
}
