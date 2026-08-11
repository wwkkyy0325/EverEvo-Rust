//! Workflow tool — structured multi-agent orchestration.
//!
//! Matches Claude Code's Workflow pattern: accepts tasks, spawns sub-agents,
//! aggregates results. Each task runs as an independent agent with shared
//! tool access.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::delegate::{SubAgentHandle, SubAgentStatus};

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTask {
    pub description: String,
    pub prompt: String,
}

pub type WorkflowResults = Arc<std::sync::Mutex<Vec<String>>>;

pub fn new_workflow_results() -> WorkflowResults {
    Arc::new(std::sync::Mutex::new(Vec::new()))
}

// ── Tool ──────────────────────────────────────────────────────────────

pub struct WorkflowTool {
    /// LLM client for sub-agents.
    llm: Option<Arc<crate::llm::HttpClient>>,
    /// Base tools available to sub-agents.
    base_tools: Option<Arc<everevo_core::tool::ToolRegistry>>,
    /// Sandbox root directory.
    sandbox_root: Option<Arc<std::path::PathBuf>>,
    /// Shared results backlog.
    results: WorkflowResults,
    /// Handles for monitoring.
    handles: Arc<std::sync::Mutex<Vec<SubAgentHandle>>>,
    /// Statuses for monitoring.
    statuses: Arc<std::sync::Mutex<Vec<SubAgentStatus>>>,
    /// Pending counter.
    pending: Arc<std::sync::atomic::AtomicUsize>,
    max_concurrent: usize,
    /// Shared pending counter (from TaskTool) — auto-continue loop watches this.
    shared_pending: Option<super::delegate::SharedPending>,
    /// Shared results backlog (from TaskTool) — auto-continue loop drains this.
    shared_backlog: Option<super::delegate::SharedBacklog>,
    /// Result sender — feeds subagent_rx so the main loop injects [SubAgent Result].
    /// Mirrors TaskTool.result_tx; shared across tools so Workflow/Team results
    /// flow through the same channel as TaskTool results.
    result_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl WorkflowTool {
    pub fn new(results: WorkflowResults) -> Self {
        Self {
            llm: None,
            base_tools: None,
            sandbox_root: None,
            results,
            handles: Arc::new(std::sync::Mutex::new(Vec::new())),
            statuses: Arc::new(std::sync::Mutex::new(Vec::new())),
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max_concurrent: 4,
            shared_pending: None,
            shared_backlog: None,
            result_tx: None,
        }
    }

    /// Wire into the shared auto-continue system.
    /// Results from parallel sub-agents will be pushed to the shared backlog,
    /// and the shared pending counter is used so the main agent loop knows
    /// when to wait for completions.
    pub fn with_shared_counters(
        mut self,
        pending: super::delegate::SharedPending,
        backlog: super::delegate::SharedBacklog,
    ) -> Self {
        self.shared_pending = Some(pending);
        self.shared_backlog = Some(backlog);
        self
    }

    /// Wire the result sender so sub-agent results flow back to the main loop
    /// via subagent_rx (same channel TaskTool uses).
    pub fn with_result_tx(mut self, tx: tokio::sync::mpsc::UnboundedSender<String>) -> Self {
        self.result_tx = Some(tx);
        self
    }

    /// Wire up sub-agent execution capability.
    pub fn with_subagent_engine(
        mut self,
        llm: Arc<crate::llm::HttpClient>,
        base_tools: Arc<everevo_core::tool::ToolRegistry>,
        sandbox_root: Arc<std::path::PathBuf>,
    ) -> Self {
        self.llm = Some(llm);
        self.base_tools = Some(base_tools);
        self.sandbox_root = Some(sandbox_root);
        self
    }

    pub fn with_max_concurrent(mut self, n: usize) -> Self {
        self.max_concurrent = n;
        self
    }
}

#[async_trait]
impl Tool for WorkflowTool {
    fn name(&self) -> &str {
        "parallel_agents"
    }

    fn description(&self) -> &str {
        "Run multiple independent tasks in PARALLEL via sub-agents (or SEQUENTIAL \
         mode to chain context). Each task gets full tool access and an isolated \
         context. Distinct from `workflow_run` (which runs a JSON step pipeline) — \
         this tool is for fan-out of whole sub-agent tasks. Use when 2+ tasks are \
         independent and benefit from isolated reasoning."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": { "type": "string" },
                            "prompt": { "type": "string" }
                        },
                        "required": ["description", "prompt"]
                    }
                },
                "mode": {
                    "type": "string",
                    "enum": ["parallel", "sequential"]
                }
            },
            "required": ["tasks"]
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
        let tasks: Vec<WorkflowTask> = serde_json::from_value(params["tasks"].clone())
            .map_err(|e| EverEvoError::InvalidInput(format!("Invalid tasks: {e}")))?;

        if tasks.is_empty() {
            return Ok(ToolOutput {
                content: "No tasks provided.".into(),
                is_error: false,
                ..Default::default()
            });
        }

        let mode = params["mode"].as_str().unwrap_or("parallel");

        // Check if sub-agent engine is wired up
        let (Some(ref llm), Some(ref tools), Some(ref sandbox)) =
            (&self.llm, &self.base_tools, &self.sandbox_root)
        else {
            let plan = tasks
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{}. {} — {}", i + 1, t.description, t.prompt))
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(ToolOutput {
                content: format!("Workflow plan ({} tasks):\n\n{plan}\n\n*Sub-agent engine not wired — tasks logged only.*", tasks.len()),
                is_error: false,
             ..Default::default() });
        };

        // ── Real sub-agent execution ──────────────────────────────
        let task_count = tasks.len();

        if mode == "sequential" {
            // Run tasks in sequence, each receiving context from the previous result
            let mut context = String::new();
            let mut results = Vec::new();

            for task in tasks {
                let base_prompt = &task.prompt;
                let prompt = if !context.is_empty() {
                    format!(
                        "{base_prompt}\n\n## Context from previous task\n{context}\n\n\
                         Use the above context to inform your work."
                    )
                } else {
                    base_prompt.clone()
                };

                let cancel = tokio_util::sync::CancellationToken::new();
                let llm_c = Arc::clone(llm);
                let tools_c = Arc::clone(tools);
                let sandbox_c = Arc::clone(sandbox);

                let content = run_workflow_agent(&prompt, llm_c, tools_c, &sandbox_c, cancel).await;
                let summary = format!("## {}\n\n{content}", task.description);
                results.push(summary.clone());
                self.results
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(summary);
                context = content;
            }

            return Ok(ToolOutput {
                content: results.join("\n\n---\n\n"),
                is_error: false,
                ..Default::default()
            });
        }

        // ── Parallel mode ──
        let total = tasks.len();
        let max_concurrent = self.max_concurrent.max(1);
        if total > max_concurrent {
            tracing::info!(
                total,
                max_concurrent,
                "WorkflowTool: queuing excess tasks (will run all, max_concurrent at a time)"
            );
        }
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        for task in &tasks {
            let permit = semaphore.clone().acquire_owned().await;

            let task_id = Uuid::new_v4();
            let desc = task.description.clone();
            let prompt = task.prompt.clone();
            let started = chrono::Utc::now();
            let cancel = tokio_util::sync::CancellationToken::new();

            self.statuses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(SubAgentStatus {
                    id: task_id,
                    description: desc.clone(),
                    started_at: started.to_rfc3339(),
                    status: "running".into(),
                    elapsed_ms: 0,
                });
            self.handles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(SubAgentHandle {
                    id: task_id,
                    description: desc.clone(),
                    started_at: started,
                    cancel: cancel.clone(),
                });
            self.pending
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Also bump the shared pending counter so the main loop's
            // auto-continue mechanism knows sub-agents are running.
            if let Some(ref sp) = self.shared_pending {
                sp.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }

            let llm_c = Arc::clone(llm);
            let tools_c = Arc::clone(tools);
            let sandbox_c = Arc::clone(sandbox);
            let results_c = Arc::clone(&self.results);
            let pending_c = Arc::clone(&self.pending);
            let statuses_c = Arc::clone(&self.statuses);
            let shared_pending = self.shared_pending.clone();
            let shared_backlog = self.shared_backlog.clone();
            let result_tx = self.result_tx.clone();
            let task_id_str = task_id.to_string();

            tokio::spawn(async move {
                let _permit = permit; // hold semaphore permit until task completes
                let content = run_workflow_agent(&prompt, llm_c, tools_c, &sandbox_c, cancel).await;
                let summary = format!("## {desc}\n\n{content}");
                // Feed into subagent_rx so the main loop injects [SubAgent Result]
                if let Some(ref tx) = result_tx {
                    let _ = tx.send(summary.clone());
                }
                results_c
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(summary.clone());
                // Push into shared backlog so auto-continue can see it
                if let Some(ref bl) = shared_backlog {
                    bl.lock().unwrap_or_else(|e| e.into_inner()).push((
                        task_id_str.clone(),
                        desc.clone(),
                        summary,
                    ));
                }
                pending_c.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                if let Some(ref sp) = shared_pending {
                    sp.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                }
                if let Ok(mut s) = statuses_c.lock() {
                    if let Some(e) = s.iter_mut().find(|e| e.id == task_id) {
                        e.status = if content.starts_with("Error:") {
                            "failed".into()
                        } else {
                            "completed".into()
                        };
                    }
                }
            });
        }

        Ok(ToolOutput {
            content: format!(
                "Workflow started ({mode}, {total}/{task_count} sub-agents spawned, \
                 max {max_concurrent} concurrent). Results will appear as they complete.",
            ),
            is_error: false,
            ..Default::default()
        })
    }
}

// ── Sub-agent runner ──────────────────────────────────────────────────

async fn run_workflow_agent(
    prompt: &str,
    llm: Arc<crate::llm::HttpClient>,
    tools: Arc<everevo_core::tool::ToolRegistry>,
    sandbox_root: &std::path::Path,
    cancel: tokio_util::sync::CancellationToken,
) -> String {
    use everevo_core::llm::LlmMessage;

    if cancel.is_cancelled() {
        return "Cancelled.".into();
    }

    let messages = vec![
        LlmMessage::system(format!(
            "You are a sub-agent executing a workflow task. Sandbox root: {}. \
             Complete the task and return results concisely.",
            sandbox_root.display()
        )),
        LlmMessage::user(prompt),
    ];

    let agent = crate::AgentLoop::sub_agent(3);
    agent.run_subagent(llm, tools, messages, cancel).await
}
