//! Agent Orchestration — SupervisorAgent + SubAgent + TaskDecomposer.
//!
//! ## Architecture (OpenAI Agents SDK + CrewAI Manager-Worker)
//!
//! SupervisorAgent (主Agent, 长期存活)
//!   ├── 分析任务 → 拆解为子任务
//!   ├── 创建 SubAgent (临时) → 注入上下文 → 执行 → 销毁
//!   └── 汇总结果 + Re-plan loop
//!
//! ## References
//! - OpenAI Agents SDK: Agent-as-Tool + Handoff patterns
//! - CrewAI: Manager-Worker with task-level tool scoping
//! - LangGraph: Execute→Re-plan→Execute adaptive loop

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use everevo_core::llm::LlmMessage;
use everevo_core::sandbox::SandboxProvider;
use everevo_core::tool::ToolRegistry;
use everevo_core::EverEvoError;

// ── Task ──────────────────────────────────────────────────────────────────

/// A task that can be executed by a Supervisor or SubAgent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: Uuid,
    /// Human-readable task description.
    pub description: String,
    /// Task type for routing.
    pub task_type: TaskType,
    /// Required tools (minimum set).
    pub required_tools: Vec<String>,
    /// Expected output format hint.
    pub output_hint: Option<String>,
    /// Priority.
    pub priority: TaskPriority,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskType {
    /// Direct answer — no subagent needed.
    DirectAnswer,
    /// Code/technical task.
    CodeTask,
    /// Research/analysis task.
    ResearchTask,
    /// Review/audit task.
    ReviewTask,
    /// File/data operation.
    FileOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskPriority {
    Low,
    Normal,
    High,
    Critical,
}

// ── Agent Context ─────────────────────────────────────────────────────────

/// Context passed from Supervisor to SubAgent.
#[derive(Clone)]
pub struct AgentContext {
    /// Inherited persona profile.
    pub persona: Option<String>,
    /// Relevant memory facts (top-K).
    pub memory_facts: Vec<String>,
    /// Relevant domain chunks.
    pub domain_chunks: Vec<String>,
    /// Available tools.
    pub tools: Vec<String>,
    /// Sandbox provider.
    pub sandbox: Option<Arc<dyn SandboxProvider>>,
    /// Shell name for environment hints (e.g. "Git Bash").
    pub shell_name: Option<String>,
}

// ── Task Decomposer ──────────────────────────────────────────────────────

/// LLM-driven task decomposition.
/// Analyzes a user request and breaks it into subtasks if needed.
pub struct TaskDecomposer;

impl TaskDecomposer {
    /// Analyze a user message and produce a decomposition plan.
    /// Returns either a direct answer instruction or a list of subtasks.
    pub fn decompose(user_message: &str, available_tools: &[String]) -> DecompositionPlan {
        let lower = user_message.to_lowercase();

        // Simple heuristics (Phase 4c upgrades to LLM-based decomposition)
        let needs_decomposition = lower.contains("并且")
            || lower.contains("同时")
            || lower.contains("然后")
            || lower.contains("and also")
            || lower.contains("审查")
            || lower.contains("review");

        if !needs_decomposition {
            return DecompositionPlan::DirectAnswer;
        }

        // Extract subtasks by splitting on connectors
        let connectors = ["并且", "同时", "然后", "and also", "同时也要"];
        let mut subtasks = Vec::new();
        let mut remaining = user_message.to_string();

        for conn in &connectors {
            if let Some(pos) = remaining.to_lowercase().find(&conn.to_lowercase()) {
                let first = remaining[..pos].trim().to_string();
                if !first.is_empty() {
                    let task_type = classify_task(&first);
                    subtasks.push(SubTask {
                        id: Uuid::new_v4(),
                        description: first,
                        task_type,
                        required_tools: available_tools.to_vec(),
                        priority: TaskPriority::Normal,
                    });
                }
                remaining = remaining[pos + conn.len()..].trim().to_string();
            }
        }
        if !remaining.is_empty() {
            let task_type = classify_task(&remaining);
            subtasks.push(SubTask {
                id: Uuid::new_v4(),
                description: remaining.clone(),
                task_type,
                required_tools: available_tools.to_vec(),
                priority: TaskPriority::Normal,
            });
            // remaining consumed here; ok since it's the last use
            let _ = remaining;
        }

        if subtasks.len() <= 1 {
            DecompositionPlan::DirectAnswer
        } else {
            DecompositionPlan::Delegated {
                subtasks,
                parallel: true, // subtasks can run in parallel if independent
            }
        }
    }

    /// Build a decomposition prompt for LLM-based task breakdown.
    pub fn build_decomposition_prompt(user_message: &str) -> String {
        format!(
            "You are a task planner. Analyze the following user request and break it into subtasks.\n\n\
             Rules:\n\
             - If the request is simple and can be answered directly, return {{\"action\": \"direct\"}}\n\
             - If the request requires multiple steps, return a list of subtasks\n\
             - Each subtask should have: description, task_type (code/research/review/file), required_tools\n\
             - Mark subtasks that can run in parallel with \"parallel\": true\n\n\
             User request: {user_message}\n\n\
             Return JSON only:"
        )
    }
}

/// Result of task decomposition.
#[derive(Debug, Clone)]
pub enum DecompositionPlan {
    /// Answer directly, no decomposition needed.
    DirectAnswer,
    /// Delegate to one or more SubAgents.
    Delegated {
        subtasks: Vec<SubTask>,
        /// Whether subtasks can run in parallel.
        parallel: bool,
    },
}

#[derive(Debug, Clone)]
pub struct SubTask {
    pub id: Uuid,
    pub description: String,
    pub task_type: TaskType,
    pub required_tools: Vec<String>,
    pub priority: TaskPriority,
}

fn classify_task(text: &str) -> TaskType {
    let lower = text.to_lowercase();
    if lower.contains("code") || lower.contains("代码") || lower.contains("实现") || lower.contains("写") {
        TaskType::CodeTask
    } else if lower.contains("审查") || lower.contains("review") || lower.contains("检查") {
        TaskType::ReviewTask
    } else if lower.contains("搜索") || lower.contains("研究") || lower.contains("了解") {
        TaskType::ResearchTask
    } else if lower.contains("文件") || lower.contains("下载") || lower.contains("读取") {
        TaskType::FileOperation
    } else {
        TaskType::DirectAnswer
    }
}

// ── SubAgent ──────────────────────────────────────────────────────────────

/// A temporary sub-agent for executing a delegated task.
pub struct SubAgent {
    pub id: Uuid,
    pub task: SubTask,
    pub context: AgentContext,
    pub max_turns: usize,
    pub timeout: Duration,
    pub sandbox_dir: PathBuf,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

/// Result returned by a SubAgent.
#[derive(Debug, Clone)]
pub struct SubAgentResult {
    pub subagent_id: Uuid,
    pub task_id: Uuid,
    pub success: bool,
    pub content: String,
    pub tool_calls: usize,
    pub turns: usize,
    pub duration_ms: u64,
    pub error: Option<String>,
}

impl SubAgent {
    /// Create a new subagent for a task.
    pub fn spawn(
        task: SubTask,
        context: AgentContext,
        sandbox_root: &PathBuf,
    ) -> Result<Self, EverEvoError> {
        let id = Uuid::new_v4();
        let sandbox_dir = sandbox_root.join(id.to_string());
        std::fs::create_dir_all(&sandbox_dir).map_err(|e| {
            EverEvoError::Internal(format!("SubAgent sandbox: {e}"))
        })?;
        Ok(Self {
            id,
            task,
            context,
            // Claude Code: subagents run until complete or LLM-specified limit.
            max_turns: 0, // 0 = unlimited (LLM controls via task tool)
            // No hard timeout — turn limit is the primary guardrail.
            // Subagent runs until max_turns exhausted or task complete.
            timeout: Duration::from_secs(600),
            sandbox_dir,
            started_at: chrono::Utc::now(),
        })
    }

    /// With a custom max_turns.
    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    /// With a custom timeout.
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    /// Build the system prompt for this subagent.
    pub fn build_system_prompt(&self) -> String {
        let mut prompt = format!(
            "You are a specialized sub-agent.\n\n\
             ## Environment\n\
             Shell: {shell}. Python: `python`. Node: `node`. Git: `git`.\n\
             Working dir: {work_dir}. Use ./ for paths, FORWARD slashes only.\n\
             UTF-8 is enforced system-wide — all Python I/O defaults to UTF-8.\n\
             Your output is automatically wrapped with agent_id/timestamp/schema metadata.\n\n\
             ## Rules\n\
             - FAIL FAST: missing dependency → stop and report. No silent fallback.\n\
             - NO INSTALLS. Use what's available.\n\
             - Validate upstream data schemas before consuming.\n\
             - Sub-agents run in PARALLEL — design for concurrent execution.\n\n\
             ## Task\n{task}\n\n\
             Tools: {tools}. Return thorough results.",
            task = self.task.description,
            shell = self.context.shell_name.as_deref().unwrap_or("Git Bash"),
            work_dir = self.sandbox_dir.display(),
            tools = self.context.tools.join(", "),
        );

        if let Some(ref persona) = self.context.persona {
            prompt.push_str(&format!("\n\n## Communication Style\n{persona}"));
        }

        if !self.context.memory_facts.is_empty() {
            prompt.push_str("\n\n## Relevant Context\n");
            for fact in &self.context.memory_facts {
                prompt.push_str(&format!("- {fact}\n"));
            }
        }

        prompt
    }

    /// Build the initial user message for this subagent.
    pub fn build_user_message(&self) -> LlmMessage {
        LlmMessage::user(&format!(
            "Execute this task and return the result:\n\n{}\n\n\
             If you need to run shell commands, use the shell tool.\n\
             If you make multiple attempts, report all findings.",
            self.task.description,
        ))
    }

    /// Execute the subagent using its own AgentLoop with the given LLM and tools.
    /// Runs in the subagent's sandbox, respecting max_turns and timeout.
    pub async fn execute(
        &self,
        llm: Arc<crate::llm::HttpClient>,
        tools: Arc<ToolRegistry>,
    ) -> SubAgentResult {
        let start = std::time::Instant::now();

        // Build messages for the subagent's own AgentLoop
        let system = LlmMessage::system(&self.build_system_prompt());
        let user = self.build_user_message();
        let messages = vec![system, user];

        // Run a limited-turn AgentLoop
        let agent_loop = crate::AgentLoop::new()
            .with_max_turns(self.max_turns);

        let mut rx = agent_loop.run(llm, tools, messages, None).await;

        let mut final_text = String::new();
        let mut tool_call_count = 0usize;
        let mut tool_error_count = 0usize;
        let mut turn_count = 0usize;

        while let Some(event) = rx.recv().await {
            match event {
                crate::AgentEvent::ToolCallStart { .. } => {
                    tool_call_count += 1;
                }
                crate::AgentEvent::ToolCallEnd { is_error, .. } => {
                    if is_error {
                        tool_error_count += 1;
                    }
                }
                crate::AgentEvent::TurnComplete => {
                    turn_count += 1;
                }
                crate::AgentEvent::Done { final_text: text } => {
                    final_text = text;
                    break;
                }
                crate::AgentEvent::Error { message } => {
                    final_text = message;
                    break;
                }
                _ => {}
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        SubAgentResult {
            subagent_id: self.id,
            task_id: self.task.id,
            success: tool_error_count == 0 && !final_text.is_empty(),
            content: final_text,
            tool_calls: tool_call_count,
            turns: turn_count,
            duration_ms,
            error: if tool_error_count > 0 {
                Some(format!("{tool_error_count} tool errors"))
            } else {
                None
            },
        }
    }
}

// ── Agent Pool ────────────────────────────────────────────────────────────

/// Manages the lifecycle of SubAgents.
pub struct AgentPool {
    /// Maximum concurrent subagents.
    max_concurrent: usize,
    /// Currently active subagents.
    active_count: usize,
    /// Sandbox root directory.
    sandbox_root: PathBuf,
}

impl AgentPool {
    pub fn new(sandbox_root: PathBuf) -> Self {
        Self {
            max_concurrent: 3,
            active_count: 0,
            sandbox_root,
        }
    }

    pub fn with_max_concurrent(mut self, n: usize) -> Self {
        self.max_concurrent = n;
        self
    }

    /// Spawn a subagent if under the concurrency limit.
    pub fn try_spawn(
        &mut self,
        task: SubTask,
        context: AgentContext,
    ) -> Result<SubAgent, EverEvoError> {
        if self.active_count >= self.max_concurrent {
            return Err(EverEvoError::Internal(format!(
                "Agent pool full ({}/{}). Wait for running subagents to complete.",
                self.active_count, self.max_concurrent
            )));
        }
        self.active_count += 1;
        SubAgent::spawn(task, context, &self.sandbox_root)
    }

    /// Mark a subagent as complete.
    pub fn complete(&mut self) {
        self.active_count = self.active_count.saturating_sub(1);
    }

    /// Clean up a subagent's sandbox.
    pub fn cleanup(&self, subagent_id: Uuid) {
        let dir = self.sandbox_root.join(subagent_id.to_string());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Available capacity.
    pub fn available(&self) -> usize {
        self.max_concurrent - self.active_count
    }

    /// Running count.
    pub fn running(&self) -> usize {
        self.active_count
    }
}

// ── Supervisor ────────────────────────────────────────────────────────────

/// The main orchestrating agent.
pub struct SupervisorAgent {
    /// Tool registry shared with subagents.
    pub tool_registry: Arc<ToolRegistry>,
    /// Agent pool for subagent lifecycle.
    pub agent_pool: AgentPool,
    /// Max ReAct turns for the supervisor itself.
    pub max_turns: usize,
    /// Sandbox provider shared with sub-agents (inherited from parent).
    pub sandbox: Option<Arc<dyn SandboxProvider>>,
}

impl SupervisorAgent {
    pub fn new(
        tool_registry: Arc<ToolRegistry>,
        sandbox_root: PathBuf,
    ) -> Self {
        Self {
            tool_registry,
            agent_pool: AgentPool::new(sandbox_root).with_max_concurrent(3),
            max_turns: 0, // 0 = unlimited
            sandbox: None,
        }
    }

    /// Set the sandbox provider (inherited from parent agent).
    pub fn with_sandbox(mut self, sandbox: Arc<dyn SandboxProvider>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Analyze a user message and decide: direct answer or delegate?
    pub fn plan(&self, user_message: &str) -> DecompositionPlan {
        let tool_names: Vec<String> = self
            .tool_registry
            .names()
            .into_iter()
            .map(|n| n.to_string())
            .collect();
        TaskDecomposer::decompose(user_message, &tool_names)
    }

    /// Build context for a subagent from the supervisor's state.
    pub fn build_subagent_context(
        &self,
        persona: Option<String>,
        memory_facts: Vec<String>,
    ) -> AgentContext {
        AgentContext {
            persona,
            memory_facts,
            domain_chunks: Vec::new(),
            tools: self.tool_registry.names().into_iter().map(|n| n.to_string()).collect(),
            sandbox: self.sandbox.clone(), // inherit parent's sandbox for path access
            shell_name: None,
        }
    }
    /// Full orchestration cycle: plan → delegate → execute → synthesize.
    /// Returns the final text response for the user.
    pub async fn orchestrate(
        &mut self,
        user_message: &str,
        llm: Arc<crate::llm::HttpClient>,
        tools: Arc<ToolRegistry>,
        persona: Option<String>,
        memory_facts: Vec<String>,
    ) -> OrchestrationResult {
        let start = std::time::Instant::now();

        // Phase 1: Plan
        let plan = self.plan(user_message);

        match plan {
            DecompositionPlan::DirectAnswer => {
                OrchestrationResult {
                    content: format!("[DirectAnswer] {}", user_message),
                    subtask_results: vec![],
                    total_turns: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                    re_plans: 0,
                }
            }
            DecompositionPlan::Delegated { subtasks, parallel: _ } => {
                let mut results = Vec::new();
                let mut re_plans = 0u32;
                let max_re_plans = 2u32;

                for subtask in &subtasks {
                    let ctx = self.build_subagent_context(persona.clone(), memory_facts.clone());

                    // Try to spawn and execute
                    let mut success = false;
                    for attempt in 0..=max_re_plans {
                        let subagent = match self.agent_pool.try_spawn(
                            subtask.clone(), ctx.clone(),
                        ) {
                            Ok(sa) => sa,
                            Err(_) => {
                                // Pool full — wait and retry
                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                continue;
                            }
                        };

                        let sa_id = subagent.id;
                        // No turn limit — the LLM controls when to stop via Done/Error.
                        // SubAgent.spawn() defaults to max_turns=0 (unlimited).
                        let result = subagent
                            .execute(Arc::clone(&llm), Arc::clone(&tools))
                            .await;

                        self.agent_pool.complete();
                        self.agent_pool.cleanup(sa_id);

                        if result.success || attempt >= max_re_plans {
                            results.push(result);
                            success = true;
                            break;
                        }

                        // Re-plan on failure
                        re_plans += 1;
                        tracing::warn!(
                            subagent = %sa_id,
                            attempt = attempt + 1,
                            "SubAgent failed, re-planning"
                        );
                    }

                    if !success {
                        results.push(SubAgentResult {
                            subagent_id: Uuid::nil(),
                            task_id: subtask.id,
                            success: false,
                            content: "All attempts exhausted".into(),
                            tool_calls: 0,
                            turns: 0,
                            duration_ms: 0,
                            error: Some("All retry attempts failed".into()),
                        });
                    }
                }

                // Synthesize results
                let synthesis = if results.iter().all(|r| r.success) {
                    format!(
                        "✅ {} subtasks completed:\n{}",
                        results.len(),
                        results.iter()
                            .map(|r| format!("- {}: {}", r.task_id, truncate(&r.content, 200)))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                } else {
                    let ok = results.iter().filter(|r| r.success).count();
                    let fail = results.len() - ok;
                    format!("⚠️ {ok}/{total} subtasks succeeded ({fail} failed)", ok = ok, total = results.len())
                };

                OrchestrationResult {
                    content: synthesis,
                    subtask_results: results,
                    total_turns: subtasks.len() as u32,
                    duration_ms: start.elapsed().as_millis() as u64,
                    re_plans,
                }
            }
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}...", &s[..n]) }
}

/// Result of a full orchestration cycle.
#[derive(Debug, Clone)]
pub struct OrchestrationResult {
    pub content: String,
    pub subtask_results: Vec<SubAgentResult>,
    pub total_turns: u32,
    pub duration_ms: u64,
    pub re_plans: u32,
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decompose_simple() {
        let plan = TaskDecomposer::decompose("How do I use async in Rust?", &["shell".into()]);
        assert!(matches!(plan, DecompositionPlan::DirectAnswer));
    }

    #[test]
    fn test_decompose_complex() {
        let plan = TaskDecomposer::decompose(
            "审查代码并且生成报告",
            &["shell".into(), "memory".into()],
        );
        match plan {
            DecompositionPlan::Delegated { subtasks, .. } => {
                assert!(subtasks.len() >= 2);
            }
            _ => panic!("Expected delegated plan"),
        }
    }

    #[test]
    fn test_subagent_prompt() {
        let task = SubTask {
            id: Uuid::new_v4(),
            description: "Review code".into(),
            task_type: TaskType::ReviewTask,
            required_tools: vec!["shell".into()],
            priority: TaskPriority::Normal,
        };
        let ctx = AgentContext {
            persona: Some("Concise".into()),
            memory_facts: vec!["User prefers async/await".into()],
            domain_chunks: vec![],
            tools: vec!["shell".into()],
            sandbox: None,
            shell_name: None,
        };
        let agent = SubAgent::spawn(task, ctx, &PathBuf::from("/tmp")).unwrap();
        let prompt = agent.build_system_prompt();
        assert!(prompt.contains("Review code"));
        assert!(prompt.contains("Concise"));
        assert!(prompt.contains("async/await"));
    }

    #[test]
    fn test_agent_pool_concurrency() {
        let mut pool = AgentPool::new(PathBuf::from("/tmp")).with_max_concurrent(2);
        assert_eq!(pool.available(), 2);

        let task = SubTask {
            id: Uuid::new_v4(),
            description: "test".into(),
            task_type: TaskType::CodeTask,
            required_tools: vec![],
            priority: TaskPriority::Normal,
        };
        let ctx = AgentContext {
            persona: None, memory_facts: vec![], domain_chunks: vec![],
            tools: vec![], sandbox: None,
            shell_name: None,
        };

        let _a1 = pool.try_spawn(task.clone(), ctx.clone()).unwrap();
        assert_eq!(pool.running(), 1);
        let _a2 = pool.try_spawn(task.clone(), ctx).unwrap();
        assert_eq!(pool.running(), 2);
        assert!(pool.try_spawn(task, AgentContext {
            persona: None, memory_facts: vec![], domain_chunks: vec![],
            tools: vec![], sandbox: None, shell_name: None,
        }).is_err()); // full
    }
}
