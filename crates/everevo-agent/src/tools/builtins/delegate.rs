//! Task Tool — Claude Code pattern. Non-blocking: spawn, return immediately.
//!
//! ## Lifecycle Safety (Phase 1 fix)
//!
//! Every sub-agent now has:
//! - **max_turns** (default 50, from config `agent.subagent_max_turns`)
//! - **timeout** (default 300s, from config `agent.subagent_timeout_secs`)
//! - **CancellationToken** (stored in SubAgentHandle, cancellable via API)
//! - **Status tracking** (start record written BEFORE execution, not just after)
//!
//! The `handles` and `statuses` fields enable monitoring and cancellation
//! via `GET /api/agent/tasks` and `POST /api/agent/tasks/{id}/cancel`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::subagent_context::SubAgentContext;

// ── Defaults (safety floor if config is not provided) ─────────────────────

const FALLBACK_SUBAGENT_MAX_TURNS: usize = 100;
const FALLBACK_SUBAGENT_TIMEOUT_SECS: u64 = 600;

// ── Shared types ─────────────────────────────────────────────────────────

/// Shared sub-agent results backlog — (task_id, description, result_text).
/// All sub-agent dispatch paths (TaskTool, WorkflowTool, TeamTool) push here
/// so the auto-continue loop in chat.rs can drain results uniformly.
pub type SharedBacklog = Arc<std::sync::Mutex<Vec<(String, String, String)>>>;
/// Shared pending sub-agent counter — auto-continue loop watches this.
pub type SharedPending = Arc<std::sync::atomic::AtomicUsize>;

// ── Sub-agent Handle & Status ───────────────────────────────────────────

/// Handle to a running sub-agent — enables monitoring and cancellation.
#[derive(Clone)]
pub struct SubAgentHandle {
    pub id: Uuid,
    pub description: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub cancel: CancellationToken,
}

/// Snapshot of sub-agent status for API reporting.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubAgentStatus {
    pub id: Uuid,
    pub description: String,
    pub started_at: String,
    pub status: String, // "running" | "completed" | "failed" | "timeout" | "cancelled"
    pub elapsed_ms: u64,
}

// ── TaskTool ──────────────────────────────────────────────────────────────

pub struct TaskTool {
    sandbox_root: Arc<PathBuf>,
    base_tools: Arc<everevo_core::tool::ToolRegistry>,
    llm: Option<Arc<crate::llm::HttpClient>>,
    persona: Arc<std::sync::RwLock<Option<String>>>,
    result_tx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>,
    /// Pending sub-agent count — AgentLoop uses this to block Done
    /// while sub-agents are still running.
    pub pending: Arc<AtomicUsize>,
    /// Parent agent's work directory for path inheritance.
    parent_work_dir: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
    /// Pre-built sub-agent context (populated by chat route from all pipelines).
    pub subagent_ctx: Arc<std::sync::RwLock<SubAgentContext>>,
    /// Running sub-agent JoinHandles for lifecycle management.
    pub task_handles: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    /// Running sub-agent handles for cancellation/monitoring.
    pub handles: Arc<std::sync::Mutex<Vec<SubAgentHandle>>>,
    /// Completed sub-agent statuses (pruned periodically).
    pub statuses: Arc<std::sync::Mutex<Vec<SubAgentStatus>>>,
    /// Accumulated sub-agent results: (id, description, result_text).
    /// Appended on completion, drained by the auto-continue loop in chat.rs.
    pub results_backlog: Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
    /// Max turns per sub-agent (safety ceiling, configurable).
    subagent_max_turns: usize,
    /// Timeout per sub-agent in seconds (safety ceiling, configurable).
    subagent_timeout_secs: u64,
}

impl TaskTool {
    pub fn new(
        sandbox_root: Arc<PathBuf>,
        base_tools: Arc<everevo_core::tool::ToolRegistry>,
        llm: Option<Arc<crate::llm::HttpClient>>,
    ) -> Self {
        Self {
            sandbox_root,
            base_tools,
            llm,
            persona: Arc::new(std::sync::RwLock::new(None)),
            result_tx: Arc::new(std::sync::Mutex::new(None)),
            pending: Arc::new(AtomicUsize::new(0)),
            parent_work_dir: Arc::new(std::sync::RwLock::new(None)),
            subagent_ctx: Arc::new(std::sync::RwLock::new(SubAgentContext::default())),
            task_handles: Arc::new(std::sync::Mutex::new(Vec::new())),
            handles: Arc::new(std::sync::Mutex::new(Vec::new())),
            statuses: Arc::new(std::sync::Mutex::new(Vec::new())),
            results_backlog: Arc::new(std::sync::Mutex::new(Vec::new())),
            subagent_max_turns: FALLBACK_SUBAGENT_MAX_TURNS,
            subagent_timeout_secs: FALLBACK_SUBAGENT_TIMEOUT_SECS,
        }
    }

    /// Configure sub-agent safety limits.
    pub fn with_subagent_limits(mut self, max_turns: usize, timeout_secs: u64) -> Self {
        self.subagent_max_turns = max_turns;
        self.subagent_timeout_secs = timeout_secs;
        self
    }

    /// Set the parent agent's work directory so sub-agents can access its files.
    pub fn set_parent_work_dir(&self, dir: std::path::PathBuf) {
        *self.parent_work_dir.write().unwrap() = Some(dir);
    }

    /// Get a receiver for the AgentLoop and store the sender.
    pub fn take_receiver(&self) -> tokio::sync::mpsc::UnboundedReceiver<String> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self.result_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        rx
    }

    pub fn set_persona(&self, persona: String) {
        *self.persona.write().unwrap() = Some(persona);
    }

    /// Get status of all sub-agents (running + recently completed).
    pub fn get_statuses(&self) -> Vec<SubAgentStatus> {
        let now = Utc::now();
        let mut statuses = self.statuses.lock().unwrap_or_else(|e| e.into_inner());

        // Update elapsed_ms for running agents
        for s in statuses.iter_mut() {
            if s.status == "running" {
                if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&s.started_at) {
                    let started_utc = started.with_timezone(&chrono::Utc);
                    s.elapsed_ms = now
                        .signed_duration_since(started_utc)
                        .num_milliseconds()
                        .max(0) as u64;
                }
            }
        }
        statuses.clone()
    }

    /// Cancel a running sub-agent by ID. Returns true if found and cancelled.
    pub fn cancel(&self, id: Uuid) -> bool {
        let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = handles.iter().position(|h| h.id == id) {
            let handle = handles.remove(pos);
            handle.cancel.cancel();
            tracing::info!(%id, desc = %handle.description, "Sub-agent cancelled by user");
            // Status updated in the spawned task's cancel path
            return true;
        }
        false
    }

    /// Prune old completed statuses (keep last 100, remove older than 1 hour).
    pub fn prune_statuses(&self) {
        let mut statuses = self.statuses.lock().unwrap_or_else(|e| e.into_inner());
        let now = Utc::now();
        statuses.retain(|s| {
            if s.status == "running" {
                return true;
            }
            if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&s.started_at) {
                let started_utc = started.with_timezone(&chrono::Utc);
                if now.signed_duration_since(started_utc).num_hours() < 1 {
                    return true;
                }
            }
            false
        });
        // Keep at most 100 recent
        if statuses.len() > 100 {
            // Sort: running first, then recent completions
            statuses.sort_by_key(|s| s.status != "running");
            statuses.truncate(100);
        }
    }

    fn dispatch_one(
        &self,
        desc: &str,
        stype: &str,
        max_turns: usize,
        isolation: Option<String>,
    ) -> Uuid {
        self.pending.fetch_add(1, Ordering::SeqCst);

        let cancel = CancellationToken::new();
        let subagent_id = Uuid::new_v4();
        let started_at = Utc::now();
        let use_worktree = isolation.as_deref() == Some("worktree");

        // Register handle for cancellation
        let handle = SubAgentHandle {
            id: subagent_id,
            description: desc.to_string(),
            started_at,
            cancel: cancel.clone(),
        };
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);

        // Register running status (visible immediately via API)
        self.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SubAgentStatus {
                id: subagent_id,
                description: desc.to_string(),
                started_at: started_at.to_rfc3339(),
                status: "running".into(),
                elapsed_ms: 0,
            });

        // Compute effective limits (LLM-specified or config default)
        let effective_max_turns = if max_turns == 0 {
            self.subagent_max_turns
        } else {
            max_turns
        };
        let effective_timeout_secs = self.subagent_timeout_secs;

        // Write telemetry START record (before execution, not just after)
        let sandbox_root = Arc::clone(&self.sandbox_root);
        let persist_dir = sandbox_root
            .parent()
            .unwrap_or(&sandbox_root)
            .join("telemetry")
            .join("subagent_tasks");
        std::fs::create_dir_all(&persist_dir).ok();
        let _ = std::fs::write(
            persist_dir.join(format!("{}.started.json", subagent_id)),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": subagent_id.to_string(),
                "task": desc,
                "status": "started",
                "started_at": started_at.to_rfc3339(),
                "max_turns": effective_max_turns,
                "timeout_secs": effective_timeout_secs,
            }))
            .unwrap_or_default(),
        );

        // Clone all needed state for the spawned task
        let tools = Arc::clone(&self.base_tools);
        let llm = self.llm.clone();
        let ctx = self.subagent_ctx.read().unwrap().clone();
        let tx = self
            .result_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let pending = Arc::clone(&self.pending);
        let statuses = Arc::clone(&self.statuses);
        let backlog = Arc::clone(&self.results_backlog);
        let desc = desc.to_string();
        let stype = stype.to_string();
        let task_handles = Arc::clone(&self.task_handles);

        let handle = tokio::spawn(async move {
            let Some(llm_client) = llm else {
                let err_msg = format!(
                    "[SubAgent FAILED] {desc}\nNo LLM configured for sub-agent."
                );
                backlog
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((subagent_id.to_string(), desc.to_string(), err_msg.clone()));
                if let Some(ref t) = tx {
                    let _ = t.send(err_msg);
                }
                pending.fetch_sub(1, Ordering::SeqCst);
                return;
            };

            // ── Git worktree isolation ─────────────────────────────
            let _worktree_guard = if use_worktree {
                let wt_name = format!("subagent-{}", subagent_id);
                let wt_path = sandbox_root.join(&wt_name);
                // Create worktree from HEAD
                #[allow(clippy::disallowed_methods)]
                let output = tokio::process::Command::new("git")
                    .args([
                        "worktree",
                        "add",
                        "--detach",
                        wt_path.to_string_lossy().as_ref(),
                        "HEAD",
                    ])
                    .output()
                    .await;
                match output {
                    Ok(o) if o.status.success() => {
                        tracing::info!(%subagent_id, path = %wt_path.display(), "Git worktree created");
                        Some(wt_path)
                    }
                    _ => {
                        tracing::warn!(%subagent_id, "Git worktree creation failed — using default sandbox");
                        None
                    }
                }
            } else {
                None
            };

            let effective_root = _worktree_guard.as_ref().unwrap_or(&sandbox_root);

            // ── Execution with timeout + cancellation ──────────────
            let outcome: (&str, Option<String>) = tokio::select! {
                _ = cancel.cancelled() => {
                    ("cancelled", None)
                }
                r = tokio::time::timeout(
                    Duration::from_secs(effective_timeout_secs),
                    spawn_single(
                        effective_root, &tools, llm_client,
                        &desc, &stype, &ctx,
                        effective_max_turns, cancel.clone(),
                    ),
                ) => {
                    match r {
                        Ok(result) => ("completed", Some(result)),
                        Err(_elapsed) => {
                            tracing::warn!(
                                subagent_id = %subagent_id,
                                desc = %desc,
                                timeout_secs = effective_timeout_secs,
                                "Sub-agent timed out"
                            );
                            ("timeout", Some(format!(
                                "[SubAgent ⏱️ TIMEOUT] {desc}\n\nTimeout after {}s",
                                effective_timeout_secs
                            )))
                        }
                    }
                }
            };

            // Update status
            {
                let mut statuses = statuses.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(s) = statuses.iter_mut().find(|s| s.id == subagent_id) {
                    s.status = outcome.0.to_string();
                    if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&s.started_at) {
                        let started_utc = started.with_timezone(&chrono::Utc);
                        s.elapsed_ms = Utc::now()
                            .signed_duration_since(started_utc)
                            .num_milliseconds()
                            .max(0) as u64;
                    }
                }
            }

            // ── Clean up git worktree ───────────────────────────────
            if let Some(ref wt_path) = _worktree_guard {
                #[allow(clippy::disallowed_methods)]
                let output = tokio::process::Command::new("git")
                    .args([
                        "worktree",
                        "remove",
                        "--force",
                        wt_path.to_string_lossy().as_ref(),
                    ])
                    .output()
                    .await;
                match output {
                    Ok(o) if o.status.success() => {
                        tracing::info!(%subagent_id, path = %wt_path.display(), "Git worktree removed");
                    }
                    _ => {
                        tracing::warn!(%subagent_id, path = %wt_path.display(), "Git worktree cleanup failed");
                    }
                }
            }

            // Remove handle
            // (handles lock is separate — clean up in the handles list)
            // Note: handles are cleaned up via cancel() or pruned elsewhere

            pending.fetch_sub(1, Ordering::SeqCst);

            if let Some(result_text) = outcome.1.clone() {
                let _ = tx.map(|t| t.send(result_text.clone()));
                backlog.lock().unwrap_or_else(|e| e.into_inner()).push((
                    subagent_id.to_string(),
                    desc.to_string(),
                    result_text,
                ));
            } else if outcome.0 == "cancelled" {
                let _ = tx.map(|t| {
                    t.send(format!(
                        "[SubAgent 🛑 CANCELLED] {desc}\n\nThe sub-agent was cancelled by the user."
                    ))
                });
            }
        });
        task_handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);
        subagent_id
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }
    fn description(&self) -> &str {
        "Launch sub-agents. Use 'description' for single task, 'subtasks' array for PARALLEL. Sub-agents run in background — results appear when ready. The main agent continues while sub-agents work."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object", "properties": {
                "description": {"type": "string"},
                "subtasks": {"type": "array", "items": {"type": "object", "properties": {"description": {"type": "string"}, "subagent_type": {"type": "string"}}, "required": ["description"]}},
                "subagent_type": {"type": "string"},
                "max_turns": {"type": "integer", "description": "Max turns (0=unlimited)"},
                "isolation": {"type": "string", "enum": ["worktree", "none"], "description": "Isolation mode: worktree (git worktree) or none (default)"}
            }
        })
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        if let Some(subtasks) = params["subtasks"].as_array() {
            let mut ids = Vec::new();
            for st in subtasks {
                let d = st["description"].as_str().unwrap_or("unnamed");
                let t = st["subagent_type"].as_str().unwrap_or("code-explorer");
                let mt = st["max_turns"].as_u64().map(|v| v as usize).unwrap_or(0);
                let iso = st["isolation"].as_str().map(|s| s.to_string());
                ids.push(self.dispatch_one(d, t, mt, iso).to_string());
            }
            return Ok(ToolOutput {
                content: format!(
                    "{} subagents dispatched (task_ids: {}). Use cancel_task with any id to stop it.",
                    ids.len(),
                    ids.join(", ")
                ),
                is_error: false,
                ..Default::default()
            });
        }
        let desc = params["description"]
            .as_str()
            .or_else(|| params["task"].as_str())
            .unwrap_or("unnamed");
        let max_turns = params["max_turns"]
            .as_u64()
            .map(|v| v as usize)
            .unwrap_or(0);
        let stype = params["subagent_type"].as_str().unwrap_or("code-explorer");
        let isolation = params["isolation"].as_str().map(|s| s.to_string());
        let id = self.dispatch_one(desc, stype, max_turns, isolation);
        Ok(ToolOutput {
            content: format!(
                "SubAgent dispatched: {desc} (task_id: {id}). Use cancel_task with this task_id to stop it if needed."
            ),
            is_error: false,
            ..Default::default()
        })
    }
}

// ── CancelTaskTool — lets the LLM proactively stop a sub-agent ───────────

/// Tool for the LLM to cancel a running sub-agent by its `task_id`.
/// Shares the TaskTool's `handles`/`pending`/`statuses` so the cancellation
/// propagates to the spawned sub-agent's `CancellationToken` and the pending
/// count stays consistent (so auto-continue doesn't wait on a dead task).
pub struct CancelTaskTool {
    handles: Arc<std::sync::Mutex<Vec<SubAgentHandle>>>,
    statuses: Arc<std::sync::Mutex<Vec<SubAgentStatus>>>,
    pending: Arc<AtomicUsize>,
}

impl CancelTaskTool {
    pub fn new(
        handles: Arc<std::sync::Mutex<Vec<SubAgentHandle>>>,
        statuses: Arc<std::sync::Mutex<Vec<SubAgentStatus>>>,
        pending: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            handles,
            statuses,
            pending,
        }
    }
}

#[async_trait]
impl Tool for CancelTaskTool {
    fn name(&self) -> &str {
        "cancel_task"
    }
    fn description(&self) -> &str {
        "Cancel a running sub-agent by its task_id (the UUID returned by the `task` tool). \
         Use when a spawned task is no longer needed, is taking too long, or duplicated work — \
         the cancelled sub-agent stops at its next tool call and its result is discarded. \
         Parameters: task_id (required)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The sub-agent UUID returned by the `task` tool"
                }
            },
            "required": ["task_id"]
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
        let id_str = params["task_id"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("task_id is required".into()))?;
        let task_id: Uuid = id_str
            .parse()
            .map_err(|_| EverEvoError::InvalidInput(format!("invalid task_id: {id_str}")))?;

        let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = handles.iter().position(|h| h.id == task_id) {
            let handle = handles.remove(pos);
            drop(handles);
            handle.cancel.cancel();
            // Mark status cancelled + decrement pending count.
            let mut statuses = self.statuses.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = statuses.iter_mut().find(|s| s.id == task_id) {
                s.status = "cancelled".into();
            }
            drop(statuses);
            let prev = self.pending.fetch_sub(1, Ordering::SeqCst);
            tracing::info!(
                %task_id,
                prev_pending = prev,
                desc = %handle.description,
                "Sub-agent cancelled by LLM via cancel_task tool"
            );
            return Ok(ToolOutput {
                content: format!(
                    "Cancelled sub-agent {task_id} ({}). It will stop at its next tool call; its result is discarded.",
                    handle.description
                ),
                is_error: false,
                ..Default::default()
            });
        }
        Ok(ToolOutput {
            content: format!(
                "No running sub-agent with task_id {task_id} — it may have already finished or been cancelled."
            ),
            is_error: false,
            ..Default::default()
        })
    }
}

/// Type-specific guidance injected into the sub-agent system prompt.
fn stype_guidance(stype: &str) -> String {
    match stype {
        "reviewer" => "\n\n## Role: Code Reviewer\n\
            You are a critical code reviewer. Focus on:\n\
            - Correctness bugs and edge cases\n\
            - Security vulnerabilities\n\
            - Performance issues\n\
            - Adherence to project conventions\n\
            - Test coverage gaps\n\
            Be thorough and adversarial — find every issue.\n"
            .into(),
        "research" | "code-explorer" => "\n\n## Role: Researcher\n\
            You are a thorough researcher. Focus on:\n\
            - Exploring all relevant files and patterns\n\
            - Finding connections across modules\n\
            - Documenting your findings with file paths and line numbers\n\
            - Providing a structured, comprehensive report\n\
            Leave no stone unturned.\n"
            .into(),
        "file" => "\n\n## Role: File Operations\n\
            You are a precise file operator. Focus on:\n\
            - Making the requested file changes exactly as specified\n\
            - Verifying each change with tests or checks\n\
            - Leaving no unintended side effects\n\
            - Reporting what was changed and why.\n"
            .into(),
        _ => "\n\n## Role: General Assistant\n\
            Complete the assigned task thoroughly and return a structured result.\n"
            .into(),
    }
}

// ── spawn_single ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn spawn_single(
    sandbox_root: &Path,
    base_tools: &everevo_core::tool::ToolRegistry,
    llm: Arc<crate::llm::HttpClient>,
    desc: &str,
    stype: &str,
    sub_ctx: &SubAgentContext,
    max_turns: usize,
    cancel: CancellationToken,
) -> String {
    // Increment recursion depth for this sub-agent's children
    let mut child_ctx = sub_ctx.clone();
    child_ctx.depth = sub_ctx.depth.saturating_add(1);

    // Pass ALL tools from base_tools — not just shell+memory.
    // Each delegation level would lose tools otherwise (cascading tool loss).
    let mut sub_tools = everevo_core::tool::ToolRegistry::new();
    for name in base_tools.names() {
        if let Some(tool) = base_tools.get(name) {
            sub_tools.register(Arc::clone(tool));
        }
    }

    // Build the full system prompt with type-specific guidance.
    let mut system_prompt = child_ctx.build_system_prompt(desc);
    system_prompt.push_str(&stype_guidance(stype));

    let messages = vec![
        everevo_core::llm::LlmMessage::system(&system_prompt),
        everevo_core::llm::LlmMessage::user(format!(
            "Execute this task and return the result:\n\n{desc}\n\n\
             If you need to run shell commands, use the shell tool.\n\
             Report ALL findings including empty results.",
        )),
    ];

    // Run with max_turns limit — uses shared AgentLoop::run_subagent().
    let start = std::time::Instant::now();
    let sa_id = Uuid::new_v4();
    let agent_loop = crate::AgentLoop::new().with_max_turns(max_turns);
    let final_text = agent_loop
        .run_subagent(llm, Arc::new(sub_tools), messages, cancel)
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    // Persist telemetry (completion record)
    let persist_dir = sandbox_root
        .parent()
        .unwrap_or(sandbox_root)
        .join("telemetry")
        .join("subagent_tasks");
    std::fs::create_dir_all(&persist_dir).ok();
    let content_len = final_text.len();
    let meta_note = if content_len == 0 {
        "empty response — likely channel drop or LLM connection failure"
    } else if duration_ms < 3000 && final_text.starts_with("Error:") {
        "fast failure — likely LLM API error"
    } else {
        "sub-agent completed"
    };
    let _ = std::fs::write(
        persist_dir.join(format!("{}.json", sa_id)),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": sa_id.to_string(),
            "task": desc,
            "success": !final_text.is_empty() && !final_text.starts_with("Error:"),
            "duration_ms": duration_ms,
            "content_len": content_len,
            "note": meta_note,
            "content": &final_text[..500.min(final_text.len())],
        }))
        .unwrap_or_default(),
    );

    // Detect errors that run_subagent returns as normal text (see mod.rs + http.rs).
    // HTTP errors come as StreamEvent::Text with patterns like:
    // "Authentication failed (HTTP 401)...", "Server error (HTTP 500)...", etc.
    let is_error = final_text.is_empty()
        || final_text.starts_with("Error:")
        || final_text.contains("[Cancelled]")
        || final_text.starts_with("Timeout")
        || final_text.starts_with("Authentication failed")
        || final_text.starts_with("Rate limited")
        || final_text.starts_with("Server error")
        || final_text.starts_with("Model overloaded")
        || final_text.starts_with("Bad request")
        || final_text.starts_with("Connection failed")
        || final_text.starts_with("Network error")
        || final_text.starts_with("API error")
        || final_text.starts_with("Invalid request")
        || final_text.starts_with("Failed to read response");
    let meta = serde_json::json!({
        "agent_id": sa_id.to_string(),
        "task": desc,
        "status": if is_error { "FAILED" } else { "SUCCESS" },
        "duration_ms": duration_ms,
        "content_len": final_text.len(),
        "timestamp": Utc::now().to_rfc3339(),
        "schema_version": "1.0",
    });
    format!(
        "---SUBAGENT_RESULT---\n{}\n---END_RESULT---\n\n{}",
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
        final_text
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cancel_state() -> (
        Arc<std::sync::Mutex<Vec<SubAgentHandle>>>,
        Arc<std::sync::Mutex<Vec<SubAgentStatus>>>,
        Arc<AtomicUsize>,
    ) {
        (
            Arc::new(std::sync::Mutex::new(vec![])),
            Arc::new(std::sync::Mutex::new(vec![])),
            Arc::new(AtomicUsize::new(0)),
        )
    }

    #[tokio::test]
    async fn test_cancel_task_cancels_and_decrements() {
        let (handles, statuses, pending) = make_cancel_state();
        let id = Uuid::new_v4();
        let token = CancellationToken::new();
        handles.lock().unwrap().push(SubAgentHandle {
            id,
            description: "research X".into(),
            started_at: Utc::now(),
            cancel: token.clone(),
        });
        statuses.lock().unwrap().push(SubAgentStatus {
            id,
            description: "research X".into(),
            started_at: Utc::now().to_rfc3339(),
            status: "running".into(),
            elapsed_ms: 0,
        });
        pending.store(1, Ordering::SeqCst);

        let tool = CancelTaskTool::new(handles.clone(), statuses.clone(), pending.clone());
        let out = tool
            .execute(serde_json::json!({"task_id": id.to_string()}), None)
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(handles.lock().unwrap().is_empty(), "handle removed");
        assert_eq!(pending.load(Ordering::SeqCst), 0, "pending decremented");
        assert_eq!(statuses.lock().unwrap()[0].status, "cancelled");
        assert!(token.is_cancelled(), "cancellation token fired");
    }

    #[tokio::test]
    async fn test_cancel_task_unknown_id_is_not_error() {
        let (handles, statuses, pending) = make_cancel_state();
        let tool = CancelTaskTool::new(handles.clone(), statuses, pending);
        let out = tool
            .execute(
                serde_json::json!({"task_id": Uuid::new_v4().to_string()}),
                None,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("No running sub-agent"));
    }

    #[test]
    fn test_name() {
        assert_eq!(
            TaskTool::new(
                Arc::new(PathBuf::from("/tmp")),
                Arc::new(everevo_core::tool::ToolRegistry::new()),
                None
            )
            .name(),
            "task"
        );
    }

    #[test]
    fn test_schema() {
        let t = TaskTool::new(
            Arc::new(PathBuf::from("/tmp")),
            Arc::new(everevo_core::tool::ToolRegistry::new()),
            None,
        );
        assert!(t.parameters_schema()["properties"]["subtasks"].is_object());
    }

    #[test]
    fn test_dispatch_returns_immediately() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let t = TaskTool::new(
                Arc::new(PathBuf::from("/tmp")),
                Arc::new(everevo_core::tool::ToolRegistry::new()),
                None,
            );
            let r = t
                .execute(serde_json::json!({"description": "test"}), None)
                .await
                .unwrap();
            assert!(!r.is_error);
            assert!(r.content.contains("dispatched"));
        });
    }

    #[test]
    fn test_handle_lifecycle() {
        let t = TaskTool::new(
            Arc::new(PathBuf::from("/tmp")),
            Arc::new(everevo_core::tool::ToolRegistry::new()),
            None,
        );
        // Start with no handles
        assert!(t
            .handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
        assert!(t
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
    }

    #[test]
    fn test_cancel_nonexistent() {
        let t = TaskTool::new(
            Arc::new(PathBuf::from("/tmp")),
            Arc::new(everevo_core::tool::ToolRegistry::new()),
            None,
        );
        assert!(!t.cancel(Uuid::new_v4()));
    }

    #[test]
    fn test_get_statuses_returns_clone() {
        let t = TaskTool::new(
            Arc::new(PathBuf::from("/tmp")),
            Arc::new(everevo_core::tool::ToolRegistry::new()),
            None,
        );
        let statuses = t.get_statuses();
        assert!(statuses.is_empty());
    }

    #[test]
    fn test_prune_statuses_removes_old() {
        let t = TaskTool::new(
            Arc::new(PathBuf::from("/tmp")),
            Arc::new(everevo_core::tool::ToolRegistry::new()),
            None,
        );
        // Add an old completed status
        let old = SubAgentStatus {
            id: Uuid::new_v4(),
            description: "old".into(),
            started_at: "2020-01-01T00:00:00+00:00".into(),
            status: "completed".into(),
            elapsed_ms: 1000,
        };
        t.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(old);
        t.prune_statuses();
        // Old completed should be pruned
        assert!(t
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
    }

    #[test]
    fn test_prune_keeps_running() {
        let t = TaskTool::new(
            Arc::new(PathBuf::from("/tmp")),
            Arc::new(everevo_core::tool::ToolRegistry::new()),
            None,
        );
        let running = SubAgentStatus {
            id: Uuid::new_v4(),
            description: "running-task".into(),
            started_at: Utc::now().to_rfc3339(),
            status: "running".into(),
            elapsed_ms: 5000,
        };
        t.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(running);
        t.prune_statuses();
        // Running should NOT be pruned
        assert_eq!(
            t.statuses.lock().unwrap_or_else(|e| e.into_inner()).len(),
            1
        );
    }
}
