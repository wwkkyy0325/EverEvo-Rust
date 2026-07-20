//! Agent trait — the abstraction for multi-agent orchestration.
//!
//! ## Design
//!
//! Following the same pattern as `Tool`, `LlmProvider`, and `SandboxProvider`:
//! the trait lives in `everevo-core` so any crate can implement an agent
//! without depending on `everevo-agent`.
//!
//! ## References
//! - OpenAI Agents SDK: Agent-as-Tool + Handoff patterns
//! - CrewAI: Manager-Worker with task-level tool scoping

use async_trait::async_trait;

use crate::EverEvoError;

#[async_trait]
pub trait Agent: Send + Sync {
    /// Unique agent name (used for delegation and logging).
    fn name(&self) -> &str;

    /// Human-readable description (used for supervisor planning prompts).
    fn description(&self) -> &str;

    /// Execute the agent with the given input and context.
    async fn run(&self, input: &str, context: &AgentContext) -> Result<AgentOutput, EverEvoError>;
}

// ── Context ────────────────────────────────────────────────────────────

/// Context passed to an agent during execution.
#[derive(Debug, Clone, Default)]
pub struct AgentContext {
    /// Available tool names.
    pub tools: Vec<String>,
    /// Relevant memory facts.
    pub memory_facts: Vec<String>,
    /// Relevant domain knowledge chunks.
    pub domain_chunks: Vec<String>,
    /// Whether the agent can spawn sub-agents.
    pub can_delegate: bool,
    /// Maximum turns before forced termination (0 = unlimited).
    pub max_turns: usize,
    /// Sandbox work directory path.
    pub work_dir: Option<String>,
}

// ── Output ─────────────────────────────────────────────────────────────

/// Result of an agent execution.
#[derive(Debug, Clone)]
pub struct AgentOutput {
    /// Final text response.
    pub content: String,
    /// Number of turns executed.
    pub turns: usize,
    /// Number of tool calls made.
    pub tool_calls: usize,
    /// Whether the agent completed successfully.
    pub success: bool,
    /// Optional structured data result.
    pub data: Option<serde_json::Value>,
}
