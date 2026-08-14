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

use crate::loop_::AgentRun;
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
    /// Per-task LLM override (dual-model: asymmetric verify reviewers run on
    /// the stronger verifier provider). None → the pool's default provider.
    pub model_override: Option<Arc<crate::llm::HttpClient>>,
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
            let llm = task
                .model_override
                .clone()
                .unwrap_or_else(|| Arc::clone(&self.llm));
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
                    AgentRun::sub_agent(task.max_turns.max(1))
                        .with_context_tokens(ctx.max_context_tokens)
                        .run_to_string(llm, tools, messages, cancel),
                )
                .await
                .unwrap_or_else(|_| format!("Timeout after {timeout}s"));

                let duration_ms = start.elapsed().as_millis() as u64;
                results_c.lock().unwrap()[idx] = Some(SubAgentResult {
                    id,
                    description: task.description,
                    status: if content.starts_with("Timeout") {
                        "timeout".into()
                    } else if is_error_content(&content) {
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
            let llm = task
                .model_override
                .clone()
                .unwrap_or_else(|| Arc::clone(&self.llm));
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
                    AgentRun::sub_agent(task.max_turns.max(1))
                        .with_context_tokens(ctx.max_context_tokens)
                        .run_to_string(llm, tools, messages, cancel),
                )
                .await
                .unwrap_or_else(|_| format!("Timeout after {timeout}s"));

                let duration_ms = start.elapsed().as_millis() as u64;
                let _ = tx.send(SubAgentResult {
                    id,
                    description: task.description,
                    status: if content.starts_with("Timeout") {
                        "timeout".into()
                    } else if is_error_content(&content) {
                        "failed".into()
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

// ── Error Classification Helper ─────────────────────────────────────────────

/// Classify sub-agent result content as an error.
///
/// Matches the error patterns produced by:
/// - `run_to_string` (`agent.rs`): "Error: {e}", "Error: LLM stream stalled..."
/// - `HttpClient::stream_chat` (`http.rs`): "Authentication failed (HTTP 401)...",
///   "Rate limited (HTTP 429)...", "Server error (HTTP 500)...",
///   "Model overloaded (HTTP 529)...", "Bad request (HTTP 400)...",
///   "Connection failed: ...", "Network error: ..."
/// - Cancellation/timeout: "[Cancelled]", "Timeout after..."
/// - Empty result (channel dropped before any event)
fn is_error_content(content: &str) -> bool {
    if content.is_empty() {
        return true;
    }
    if content.starts_with("Error:")
        || content.starts_with("Timeout")
        || content.contains("[Cancelled]")
    {
        return true;
    }
    // Match HTTP-level errors smuggled as StreamEvent::Text
    if content.starts_with("Authentication failed")
        || content.starts_with("Rate limited")
        || content.starts_with("Server error")
        || content.starts_with("Model overloaded")
        || content.starts_with("Bad request")
        || content.starts_with("Connection failed")
        || content.starts_with("Network error")
        || content.starts_with("API error")
        || content.starts_with("Invalid request")
        || content.starts_with("Failed to read response")
    {
        return true;
    }
    false
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
            model_override: None,
            cancel_token: None,
        };
        assert_eq!(task.max_turns, 3);
    }
}
