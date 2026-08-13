//! In-process todo_write tool with persistent storage and session state.
//!
//! Complemented by MCP plugin `plugin-todo-write`. This in-process version manages
//! persistent task storage (JSON files per session), global task tracking, and
//! workspace-aware persistence — features the MCP plugin cannot provide.
//! This in-process implementation is kept for backward compatibility.
//! New development should use the MCP plugin version.

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
    // "pending" | "in_progress" | "completed" | "failed" | "skipped" | "deferred"
    pub status: String,
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
                let session_id_str = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
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

/// Sentinel key for the cross-conversation ("global") task list. Todos written
/// with `scope="global"` land here and are surfaced in every new session's
/// context — use for long-running project tasks that span conversations.
pub const GLOBAL_TASK_KEY: Uuid = Uuid::nil();

// ── Tool ──────────────────────────────────────────────────────────────

pub struct TodoWriteTool {
    store: TodoStore,
    /// Directory for persisting task lists to disk. When set, every update
    /// auto-saves the session's todo list to `persist_dir/<session_id>.json`.
    persist_dir: Option<PathBuf>,
    /// The session this tool instance is scoped to. Injected at registry-build
    /// time so the LLM doesn't have to pass `session_id` (which it can't know).
    /// Defaults to nil only in tests / the shared CLI path.
    session_id: Uuid,
}

impl TodoWriteTool {
    pub fn new(store: TodoStore) -> Self {
        Self {
            store,
            persist_dir: None,
            session_id: Uuid::nil(),
        }
    }

    /// Enable disk persistence. Task lists are auto-saved to
    /// `<persist_dir>/<session_id>.json` on every update.
    pub fn with_persistence(mut self, dir: PathBuf) -> Self {
        self.persist_dir = Some(dir);
        self
    }

    /// Bind this tool instance to a specific session — its todos are then
    /// keyed correctly even though the LLM never supplies `session_id`.
    pub fn with_session_id(mut self, session_id: Uuid) -> Self {
        self.session_id = session_id;
        self
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn description(&self) -> &str {
        "Use this tool to create and manage a structured task list. Track progress, \
         organize complex tasks, and demonstrate thoroughness. Use proactively for \
         multi-step tasks. Each todo needs: content (imperative form), status, \
         activeForm (present continuous form). Status is one of: pending (待执行), \
         in_progress (执行中), completed (已完成), failed (失败), skipped (跳过), \
         deferred (暂缓). Only ONE task in_progress at a time. Mark complete \
         IMMEDIATELY after finishing. On failure, mark the item 'failed' with the \
         reason folded into its content or activeForm. Items can be appended to or \
         edited by resending the full list. Use scope='global' for long-running \
         project tasks that should persist across conversations (new sessions will \
         see them); scope='session' (default) for the current conversation only."
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
                                "enum": ["pending", "in_progress", "completed", "failed", "skipped", "deferred"],
                                "description": "Current task status"
                            },
                            "activeForm": {
                                "type": "string",
                                "description": "Present continuous form shown during execution"
                            }
                        },
                        "required": ["content", "status", "activeForm"]
                    }
                },
                "scope": {
                    "type": "string",
                    "enum": ["session", "global"],
                    "description": "Where this list applies: 'session' (default, this conversation) or 'global' (persists across conversations)"
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
        // Parse todos from input. Audit LOW (2026-08-13): a missing/wrong-typed
        // `todos` surfaced a raw serde error ("invalid type: null, expected a
        // sequence") with no remediation hint — give the agent the shape it needs.
        let todos: Vec<TodoItem> = match params.get("todos").and_then(|t| t.as_array()) {
            Some(arr) => serde_json::from_value(serde_json::Value::Array(arr.clone()))
                .map_err(|e| EverEvoError::InvalidInput(format!("Invalid todos: {e}")))?,
            None => {
                return Err(EverEvoError::InvalidInput(
                    "todos is required and must be an array of items like \
                     {\"description\": \"...\", \"status\": \"pending\"} — provide the full \
                     list each call (write the whole list, not a delta)"
                        .into(),
                ))
            }
        };

        // scope: "session" (default) writes to this session's list;
        //        "global" writes to the shared cross-conversation list.
        let scope = params["scope"].as_str().unwrap_or("session");
        let session_id = if scope == "global" {
            GLOBAL_TASK_KEY
        } else {
            params["session_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .unwrap_or(self.session_id)
        };

        let mut store = self.store.write().await;

        // If all session-scoped todos are completed, clear the list
        // (Claude Code behavior). Global todos always persist — never auto-delete.
        let all_done = todos.iter().all(|t| t.status == "completed");
        let is_global = session_id == GLOBAL_TASK_KEY;
        if all_done && !is_global {
            store.remove(&session_id);
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

        let count = |status: &str| new_todos.iter().filter(|t| t.status == status).count();
        let total = new_todos.len();
        let completed = count("completed");
        let in_progress = count("in_progress");
        let pending = count("pending");
        let failed = count("failed");
        let skipped = count("skipped");
        let deferred = count("deferred");
        // Keep the summary compact — list non-zero status buckets only.
        let mut buckets = vec![];
        if pending > 0 {
            buckets.push(format!("{pending} pending"));
        }
        if in_progress > 0 {
            buckets.push(format!("{in_progress} in_progress"));
        }
        if completed > 0 {
            buckets.push(format!("{completed} completed"));
        }
        if failed > 0 {
            buckets.push(format!("{failed} failed"));
        }
        if skipped > 0 {
            buckets.push(format!("{skipped} skipped"));
        }
        if deferred > 0 {
            buckets.push(format!("{deferred} deferred"));
        }

        Ok(ToolOutput {
            content: format!(
                "Todo list updated. {} items ({}).",
                total,
                if buckets.is_empty() {
                    "empty".to_string()
                } else {
                    buckets.join(", ")
                },
            ),
            is_error: false,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATUSES: &[&str] = &[
        "pending",
        "in_progress",
        "completed",
        "failed",
        "skipped",
        "deferred",
    ];

    #[test]
    fn schema_enum_covers_all_six_statuses() {
        let tool = TodoWriteTool::new(new_todo_store());
        let schema = tool.parameters_schema();
        let status_enum = schema["properties"]["todos"]["items"]["properties"]["status"]["enum"]
            .as_array()
            .expect("status must have an enum");
        let enum_vals: Vec<&str> = status_enum.iter().map(|v| v.as_str().unwrap()).collect();
        for s in ALL_STATUSES {
            assert!(enum_vals.contains(s), "schema enum missing {s}");
        }
        assert_eq!(enum_vals.len(), ALL_STATUSES.len());
    }

    #[tokio::test]
    async fn execute_counts_all_status_buckets() {
        let store = new_todo_store();
        let tool = TodoWriteTool::new(store.clone()).with_session_id(Uuid::nil());
        let list: Vec<serde_json::Value> = ALL_STATUSES
            .iter()
            .enumerate()
            .map(|(i, s)| {
                serde_json::json!({
                    "content": format!("task {i}"),
                    "status": s,
                    "activeForm": format!("working {i}"),
                })
            })
            .collect();
        let params = serde_json::json!({ "todos": list, "scope": "session" });

        let out = tool.execute(params, None).await.expect("execute ok");
        assert!(!out.is_error);
        for s in ALL_STATUSES {
            assert!(
                out.content.contains(&format!("1 {s}")),
                "summary should mention '1 {s}', got: {}",
                out.content
            );
        }
        // Store should hold the full list (session-scoped, keyed by nil = default).
        let stored = store.read().await;
        let items = stored.get(&Uuid::nil()).expect("todos stored");
        assert_eq!(items.len(), ALL_STATUSES.len());
    }

    #[tokio::test]
    async fn dynamic_append_replaces_previous_list() {
        let store = new_todo_store();
        let tool = TodoWriteTool::new(store.clone()).with_session_id(Uuid::nil());

        // First write: 2 items
        let first = serde_json::json!({
            "todos": [
                {"content": "a", "status": "in_progress", "activeForm": "doing a"},
                {"content": "b", "status": "pending", "activeForm": "doing b"}
            ],
            "scope": "session"
        });
        tool.execute(first, None).await.expect("first write ok");

        // Append a third item + flip b → deferred (dynamic modify).
        let second = serde_json::json!({
            "todos": [
                {"content": "a", "status": "completed", "activeForm": "doing a"},
                {"content": "b", "status": "deferred", "activeForm": "doing b"},
                {"content": "c", "status": "pending", "activeForm": "doing c"}
            ],
            "scope": "session"
        });
        let out = tool.execute(second, None).await.expect("second write ok");
        assert!(out.content.contains("1 deferred"), "got: {}", out.content);

        let stored = store.read().await;
        let items = stored.get(&Uuid::nil()).expect("todos stored");
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].status, "deferred");
        assert_eq!(items[0].status, "completed");
    }
}
