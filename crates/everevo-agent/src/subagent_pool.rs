//! SubAgentPool — common abstraction for dispatching parallel sub-agents.
//!
//! Replaces the duplicated dispatch logic in TeamTool, TaskTool, and WorkflowTool.
//! Features: semaphore-based concurrency control, deterministic result ordering,
//! timeout + cancellation, optional mpsc notification.
//!
//! Claude Code alignment: mirrors the `parallel()` and `pipeline()` primitives
//! with configurable concurrency caps.

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use everevo_core::llm::LlmMessage;
use everevo_core::tool::ToolRegistry;

use crate::loop_::AgentLoop;
use crate::subagent_context::SubAgentContext;

// ── Types ──────────────────────────────────────────────────────────────

/// Configuration for the sub-agent pool.
#[derive(Clone)]
pub struct SubAgentPoolConfig {
    /// Maximum concurrent sub-agents (default 8, range 1-20).
    pub max_concurrent: usize,
    /// Per-agent timeout in seconds (default 300).
    pub timeout_secs: u64,
}

impl Default for SubAgentPoolConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 8,
            timeout_secs: 300,
        }
    }
}

/// A unit of work dispatched to a sub-agent.
#[derive(Clone)]
pub struct SubAgentTask {
    pub description: String,
    pub prompt: String,
    pub max_turns: usize,
    /// Optional system prompt override (e.g., TeamRole guidance).
    pub system_prompt_override: Option<String>,
    /// Cancellation token for this specific task.
    pub cancel_token: Option<CancellationToken>,
}

/// Deterministic result from a sub-agent run.
#[derive(Clone)]
pub struct SubAgentResult {
    pub id: Uuid,
    pub description: String,
    pub status: String,
    pub content: String,
    pub duration_ms: u64,
}

// ── Pool ───────────────────────────────────────────────────────────────

/// A bounded-concurrency pool for dispatching sub-agents.
pub struct SubAgentPool {
    config: SubAgentPoolConfig,
    llm: Arc<crate::llm::HttpClient>,
    tools: Arc<ToolRegistry>,
    sub_ctx: SubAgentContext,
    #[allow(dead_code)]
    sandbox_root: Arc<std::path::PathBuf>,
    semaphore: Arc<Semaphore>,
}

impl SubAgentPool {
    pub fn new(
        config: SubAgentPoolConfig,
        llm: Arc<crate::llm::HttpClient>,
        tools: Arc<ToolRegistry>,
        sub_ctx: SubAgentContext,
        sandbox_root: Arc<std::path::PathBuf>,
    ) -> Self {
        let max = config.max_concurrent.max(1);
        Self {
            config,
            llm,
            tools,
            sub_ctx,
            sandbox_root,
            semaphore: Arc::new(Semaphore::new(max)),
        }
    }

    /// Block until all tasks complete. Returns results in **submission order**
    /// (deterministic — fixes the TeamTool `values().nth(i)` bug).
    pub async fn execute_all(&self, tasks: Vec<SubAgentTask>) -> Vec<SubAgentResult> {
        let results: Arc<std::sync::Mutex<Vec<Option<SubAgentResult>>>> =
            Arc::new(std::sync::Mutex::new(vec![None; tasks.len()]));
        let mut handles = Vec::with_capacity(tasks.len());

        for (idx, task) in tasks.into_iter().enumerate() {
            let permit = self
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore closed");
            let llm = Arc::clone(&self.llm);
            let tools = Arc::clone(&self.tools);
            let ctx = self.sub_ctx.clone();
            let timeout = self.config.timeout_secs;
            let results_c = Arc::clone(&results);

            let handle = tokio::spawn(async move {
                let _permit = permit;
                let start = Instant::now();
                let id = Uuid::new_v4();

                // Build messages
                let system_prompt = task
                    .system_prompt_override
                    .as_deref()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| ctx.build_system_prompt(&task.prompt));
                let messages = vec![
                    LlmMessage::system(&system_prompt),
                    LlmMessage::user(&task.prompt),
                ];
                let cancel = task.cancel_token.unwrap_or_else(CancellationToken::new);

                let content = tokio::time::timeout(
                    std::time::Duration::from_secs(timeout),
                    AgentLoop::new()
                        .with_max_turns(task.max_turns.max(1))
                        .run_subagent(llm, tools, messages, cancel),
                )
                .await
                .unwrap_or_else(|_| format!("Timeout after {timeout}s"));

                let duration_ms = start.elapsed().as_millis() as u64;
                results_c.lock().unwrap()[idx] = Some(SubAgentResult {
                    id,
                    description: task.description,
                    status: if content.starts_with("Timeout") {
                        "timeout".into()
                    } else if content.starts_with("Error:") {
                        "failed".into()
                    } else {
                        "completed".into()
                    },
                    content,
                    duration_ms,
                });
            });
            handles.push(handle);
        }

        // Wait for all tasks
        for h in handles {
            let _ = h.await;
        }

        // Collect in submission order (deterministic)
        let collected: Vec<SubAgentResult> = {
            let guard = results.lock().unwrap();
            guard.iter().filter_map(|r| r.clone()).collect()
        };
        collected
    }

    /// Fire-and-forget dispatch. Results are sent to the provided channel.
    /// Each task holds a semaphore permit until completion.
    pub fn spawn_all(
        &self,
        tasks: Vec<SubAgentTask>,
        result_tx: tokio::sync::mpsc::UnboundedSender<SubAgentResult>,
        pending: Arc<std::sync::atomic::AtomicUsize>,
    ) {
        for task in tasks {
            let permit = self.semaphore.clone().acquire_owned();
            let llm = Arc::clone(&self.llm);
            let tools = Arc::clone(&self.tools);
            let ctx = self.sub_ctx.clone();
            let timeout = self.config.timeout_secs;
            let tx = result_tx.clone();
            let pending_c = Arc::clone(&pending);

            pending_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move {
                let _permit = permit.await.expect("semaphore closed");
                let start = Instant::now();
                let id = Uuid::new_v4();

                let system_prompt = task
                    .system_prompt_override
                    .as_deref()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| ctx.build_system_prompt(&task.prompt));
                let messages = vec![
                    LlmMessage::system(&system_prompt),
                    LlmMessage::user(&task.prompt),
                ];
                let cancel = task.cancel_token.unwrap_or_else(CancellationToken::new);

                let content = tokio::time::timeout(
                    std::time::Duration::from_secs(timeout),
                    AgentLoop::new()
                        .with_max_turns(task.max_turns.max(1))
                        .run_subagent(llm, tools, messages, cancel),
                )
                .await
                .unwrap_or_else(|_| format!("Timeout after {timeout}s"));

                let duration_ms = start.elapsed().as_millis() as u64;
                let _ = tx.send(SubAgentResult {
                    id,
                    description: task.description,
                    status: if content.starts_with("Timeout") {
                        "timeout".into()
                    } else {
                        "completed".into()
                    },
                    content,
                    duration_ms,
                });
                pending_c.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
    }

    // ── Accessors ──────────────────────────────────────────────────

    pub fn llm(&self) -> &Arc<crate::llm::HttpClient> {
        &self.llm
    }
    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = SubAgentPoolConfig::default();
        assert_eq!(cfg.max_concurrent, 8);
        assert_eq!(cfg.timeout_secs, 300);
    }

    #[test]
    fn test_task_construction() {
        let task = SubAgentTask {
            description: "test".into(),
            prompt: "do something".into(),
            max_turns: 3,
            system_prompt_override: None,
            cancel_token: None,
        };
        assert_eq!(task.max_turns, 3);
    }
}
