//! Agent events — emitted by the agent loop during execution.

/// Events emitted by the agent loop during execution.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Model is reasoning (chain-of-thought).
    Thinking(String),
    /// A token of the final response text.
    TextDelta(String),
    /// A tool call is about to be executed.
    ToolCallStart {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// A tool call completed (success or failure).
    ToolCallEnd {
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
    /// A shell command needs user confirmation before execution.
    ConfirmationNeeded { command: String, reason: String },
    /// One turn of the loop completed.
    TurnComplete,
    /// Final response complete (no more tool calls).
    Done { final_text: String },
    /// A sub-agent was dispatched.
    SubAgentStarted { id: String, description: String },
    /// A sub-agent completed with a result.
    SubAgentResult {
        id: String,
        description: String,
        result: String,
    },
    /// LLM says Done but sub-agents are still running.
    /// The caller should keep the SSE connection open and wait
    /// for SubAgentResult events, then auto-resume the agent loop.
    WaitingForSubAgents { pending: usize },
    /// An error occurred during execution.
    Error { message: String },
}
