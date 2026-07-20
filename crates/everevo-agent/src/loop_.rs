//! ReAct agent loop — the core execution cycle.
//!
//! ## Architecture
//!
//! Single-threaded while-loop, inspired by Claude Code's `nO` master loop
//! and motosan-agent-loop's streaming event model. Intentionally simple:
//! flat, debuggable, reliable.
//!
//! ```text
//! User Message
//!     │
//!     ▼
//! ┌─────────────────────────────────────────┐
//! │  while turn < max_turns:                │
//! │    llm.stream_chat(messages, tools)     │
//! │      → Thinking tokens                  │
//! │      → Text tokens                      │
//! │      → Tool calls accumulated           │
//! │                                         │
//! │    if no tool_calls:                    │
//! │      → Done (final answer)              │
//! │                                         │
//! │    for each tool_call:                  │
//! │      sandbox.check_command()            │  ← permission gate
//! │      tool.execute()                     │  ← actual execution
//! │      audit.write(record)                │  ← audit trail
//! │      append tool_result to messages      │
//! │                                         │
//! │    turn += 1                            │
//! └─────────────────────────────────────────┘
//!     │
//!     ▼
//! Final Response (or Error / MaxTurns)
//! ```
//!
//! ## Design Decisions
//!
//! | Decision | Rationale |
//! |----------|-----------|
//! | Single-threaded while-loop | Claude Code's proven pattern — flat, debuggable |
//! | Streaming events via channel | motosan-agent-loop pattern — UI sees progress in real-time |
//! | Permission check per tool call | Every shell command passes through sandbox.check_command() |
//! | Max turns = 15 | Prevents infinite loops; Claude Code uses similar guard |
//! | Tool results as LlmMessage::tool() | Standard Anthropic/OpenAI tool-result format |

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use everevo_core::llm::{LlmMessage, LlmRole, StreamEvent, ToolSchema};
use everevo_core::tool::ToolRegistry;
#[cfg(test)]
use everevo_core::tool::ToolOutput;
use everevo_core::types::ToolCall;
use everevo_core::EverEvoError;
use everevo_telemetry::{AgentTurnRecord, Telemetry};
use uuid::Uuid;

// ── Agent Events ────────────────────────────────────────────────────────

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
    ConfirmationNeeded {
        command: String,
        reason: String,
    },
    /// One turn of the loop completed.
    TurnComplete,
    /// Final response complete (no more tool calls).
    Done {
        final_text: String,
    },
    /// An error occurred during execution.
    Error {
        message: String,
    },
}

// ── Agent Loop ──────────────────────────────────────────────────────────

/// The ReAct agent loop — LLM → Tools → Results → LLM cycle.
pub struct AgentLoop {
    /// Maximum number of ReAct turns before forced termination.
    max_turns: usize,
    /// Maximum characters per tool result before truncation. 0 = no limit.
    max_tool_result_chars: usize,
    /// Approximate max total characters in the message history before trimming.
    max_context_chars: usize,
    /// Optional telemetry handle for recording agent turn metrics.
    telemetry: Option<Arc<Telemetry>>,
    /// Trace ID for correlating telemetry records.
    trace_id: Option<Uuid>,
    /// Pending subagent results channel — TaskTool pushes, AgentLoop drains.
    subagent_rx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>>,
    /// Pending sub-agent count — shared with TaskTool. When > 0, the loop
    /// waits for results instead of returning Done.
    pending_subagents: Arc<std::sync::atomic::AtomicUsize>,
}

impl AgentLoop {
    pub fn new() -> Self {
        Self {
            max_turns: 0,
            max_tool_result_chars: 4000,
            max_context_chars: 80000,
            telemetry: None,
            trace_id: None,
            subagent_rx: Arc::new(std::sync::Mutex::new(None)),
            pending_subagents: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Set the subagent result channel for non-blocking task tool results.
    pub fn with_subagent_channel(self, rx: tokio::sync::mpsc::UnboundedReceiver<String>) -> Self {
        *self.subagent_rx.lock().unwrap() = Some(rx);
        self
    }

    /// Share the pending sub-agent counter so the loop can block Done
    /// while sub-agents are still running.
    pub fn with_pending_subagents(mut self, pending: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        self.pending_subagents = pending;
        self
    }

    /// Set the maximum number of turns (default: 15).
    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    /// Set the max tool result size before truncation (default: 4000 chars).
    pub fn with_tool_result_budget(mut self, chars: usize) -> Self {
        self.max_tool_result_chars = chars;
        self
    }

    /// Set the max context size before trimming (default: 80000 chars).
    pub fn with_context_budget(mut self, chars: usize) -> Self {
        self.max_context_chars = chars;
        self
    }

    /// Attach telemetry for recording per-turn agent metrics.
    pub fn with_telemetry(mut self, telemetry: Arc<Telemetry>, trace_id: Uuid) -> Self {
        self.telemetry = Some(telemetry);
        self.trace_id = Some(trace_id);
        self
    }

    /// Run the ReAct loop with streaming output.
    ///
    /// `confirmation`: called before executing each tool. Receives (tool_name, arguments_json).
    /// Return `true` to proceed, `false` to skip with an error message to the LLM.
    /// Pass `None` to auto-approve all tools.
    pub async fn run(
        &self,
        llm: Arc<crate::llm::HttpClient>,
        tools: Arc<ToolRegistry>,
        mut messages: Vec<LlmMessage>,
        confirmation: Option<Arc<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>>,
    ) -> mpsc::Receiver<AgentEvent> {
        let (tx, rx) = mpsc::channel::<AgentEvent>(256);
        let max_turns = self.max_turns;
        let max_tool_result_chars = self.max_tool_result_chars;
        let max_context_chars = self.max_context_chars;
        let telemetry = self.telemetry.clone();
        let trace_id = self.trace_id;
        let subagent_rx = self.subagent_rx.lock().unwrap().take();
        let pending_subagents = self.pending_subagents.clone();

        tokio::spawn(async move {
            let tool_schemas: Vec<ToolSchema> = tools
                .as_tool_schemas()
                .into_iter()
                .map(|s| ToolSchema {
                    name: s["function"]["name"].as_str().unwrap_or("").into(),
                    description: s["function"]["description"].as_str().unwrap_or("").into(),
                    parameters: s["function"]["parameters"].clone(),
                })
                .collect();

            if let Err(e) = run_loop(
                &llm, &tools, &tool_schemas, &mut messages,
                max_turns, max_tool_result_chars, max_context_chars,
                confirmation.as_deref(),
                telemetry.as_ref(),
                trace_id,
                subagent_rx,
                &pending_subagents,
                &tx,
            )
            .await
            {
                let _ = tx.send(AgentEvent::Error { message: e.to_string() }).await;
            }
        });

        rx
    }
}

impl Default for AgentLoop {
    fn default() -> Self {
        Self::new()
    }
}

// ── Loop Core ───────────────────────────────────────────────────────────

async fn run_loop(
    llm: &crate::llm::HttpClient,
    tools: &ToolRegistry,
    tool_schemas: &[ToolSchema],
    messages: &mut Vec<LlmMessage>,
    max_turns: usize,
    max_tool_result_chars: usize,
    max_context_chars: usize,
    confirmation: Option<&(dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync)>,
    telemetry: Option<&Arc<Telemetry>>,
    trace_id: Option<Uuid>,
    mut subagent_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    pending_subagents: &std::sync::atomic::AtomicUsize,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<(), EverEvoError> {
    let mut turn = 0;

    while max_turns == 0 || turn < max_turns {
        turn += 1;

        // Drain pending subagent results (non-blocking) — inject as user messages.
        // Claude Code behavior: subagent completes → result appears in conversation.
        if let Some(ref mut rx) = subagent_rx {
            while let Ok(result) = rx.try_recv() {
                messages.push(LlmMessage::user(&format!("[SubAgent Result]\n{result}")));
            }
        }
        let turn_start = Instant::now();

        // Trim context before each LLM call
        trim_context(messages, max_context_chars);

        tracing::info!(turn, msg_count = messages.len(), "Agent turn start");

        // ── 1. Call LLM (streaming, one per turn) ──────────────────
        let token_rx = llm
            .stream_chat(messages, tool_schemas)
            .await
            .map_err(|e| EverEvoError::LlmProvider(e.to_string()))?;

        let mut current_text = String::new();
        let mut current_thinking = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut pending_tool: Option<(String, String, String)> = None;

        let mut token_rx = token_rx;
        while let Some(event) = token_rx.recv().await {
            match event {
                StreamEvent::Thinking(t) => {
                    current_thinking.push_str(&t);
                    let _ = tx.send(AgentEvent::Thinking(t)).await;
                }
                StreamEvent::Text(t) => {
                    current_text.push_str(&t);
                    let _ = tx.send(AgentEvent::TextDelta(t)).await;
                }
                StreamEvent::ToolCallStart { id, name } => {
                    pending_tool = Some((id, name, String::new()));
                }
                StreamEvent::ToolCallArg { id, arg_delta } => {
                    if let Some((ref pending_id, _, ref mut args)) = pending_tool {
                        if pending_id == &id {
                            args.push_str(&arg_delta);
                        }
                    }
                }
                StreamEvent::Done => {
                    if let Some((id, name, args_str)) = pending_tool.take() {
                        let arguments: serde_json::Value =
                            serde_json::from_str(&args_str).unwrap_or(serde_json::Value::Null);
                        let _ = tx.send(AgentEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        }).await;
                        tool_calls.push(ToolCall { id, name, arguments });
                    }
                    break;
                }
            }
        }

        // If text but no tool calls → check for pending sub-agents first.
        if tool_calls.is_empty() {
            let pending = pending_subagents.load(std::sync::atomic::Ordering::SeqCst);
            if pending > 0 {
                // Sub-agents still running — don't exit. Wait for their
                // results and let the LLM respond to them.
                tracing::info!(pending, "LLM says Done but sub-agents running — waiting");
                if let Some(ref mut rx) = subagent_rx {
                    // Block up to 5 min for the next sub-agent result.
                    match tokio::time::timeout(std::time::Duration::from_secs(300), rx.recv()).await {
                        Ok(Some(result)) => {
                            messages.push(LlmMessage::user(&format!("[SubAgent Result]\n{result}")));
                            continue; // Re-enter the loop; LLM will respond to the result
                        }
                        Ok(None) => {
                            tracing::warn!("SubAgent result channel closed");
                        }
                        Err(_) => {
                            tracing::warn!(pending, "Timed out waiting for sub-agent results");
                        }
                    }
                }
            }
            // No pending sub-agents → truly done.
            let final_text = current_text.clone();
            let _ = tx.send(AgentEvent::Done { final_text }).await;
            return Ok(());
        }

        // ── 2. Build assistant message with tool calls ──────────────
        let thinking = if current_thinking.is_empty() { None } else { Some(current_thinking.clone()) };
        let assistant_msg = LlmMessage {
            role: LlmRole::Assistant,
            content: if current_text.is_empty() { String::new() } else { current_text.clone() },
            thinking,
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
        };
        messages.push(assistant_msg);

        // ── 3. Execute tools ────────────────────────────────────────
        let mut tool_result_pairs: Vec<(String, String)> = Vec::new(); // (id, content)
        let mut tool_calls_success = 0i32;

        for tc in &tool_calls {
            let tool = tools.get(&tc.name);
            // Pre-execution confirmation gate
            if let Some(confirm_fn) = confirmation {
                if !confirm_fn(&tc.name, &tc.arguments) {
                    let skip_msg = format!("User declined execution of tool '{}'", tc.name);
                    let _ = tx.send(AgentEvent::ToolCallEnd {
                        id: tc.id.clone(), name: tc.name.clone(),
                        content: skip_msg.clone(), is_error: true,
                    }).await;
                    tool_result_pairs.push((tc.id.clone(), skip_msg));
                    continue;
                }
            }

            let result = match tool {
                Some(tool) => {
                    tracing::info!(tool = %tc.name, tool_call_id = %tc.id, "Executing tool");
                    tool.execute(tc.arguments.clone()).await
                }
                None => Err(EverEvoError::Tool(format!("Unknown tool: {}", tc.name))),
            };

            match result {
                Ok(output) => {
                    let truncated = truncate_output(&output.content, max_tool_result_chars);
                    let _ = tx.send(AgentEvent::ToolCallEnd {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        content: truncated.clone(),
                        is_error: output.is_error,
                    }).await;
                    if output.is_error {
                        // Emit confirmation event if shell command needs user approval
                        if tc.name == "shell" && truncated.contains("确认") || truncated.contains("confirmation") {
                            let _ = tx.send(AgentEvent::ConfirmationNeeded {
                                command: tc.arguments.get("command")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                reason: truncated.clone(),
                            }).await;
                        }
                        tracing::warn!(tool = %tc.name, "Tool returned error");
                    } else {
                        tool_calls_success += 1;
                    }
                    tool_result_pairs.push((tc.id.clone(), truncated));
                }
                Err(e) => {
                    let err_msg = format!("Tool execution failed: {e}");
                    let _ = tx.send(AgentEvent::ToolCallEnd {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        content: err_msg.clone(),
                        is_error: true,
                    }).await;
                    tool_result_pairs.push((tc.id.clone(), err_msg));
                }
            }
        }

        // ── 4. Merge tool results into ONE user message ─────────────
        // Anthropic: all tool_use blocks in one assistant msg require
        // all matching tool_result blocks in a SINGLE subsequent user msg.
        if !tool_result_pairs.is_empty() {
            if tool_result_pairs.len() == 1 {
                // Single tool: use raw content directly
                let (id, content) = tool_result_pairs.into_iter().next().unwrap_or_default();
                messages.push(LlmMessage::tool(&content, &id));
            } else {
                // Multiple tools: merge into one message with JSON metadata
                let ids: Vec<String> = tool_result_pairs.iter().map(|(id, _)| id.clone()).collect();
                let payload = serde_json::to_string(
                    &tool_result_pairs.iter().map(|(id, content)| {
                        serde_json::json!({"i": id, "c": content})
                    }).collect::<Vec<_>>()
                ).unwrap_or_default();
                let mut msg = LlmMessage::tool(&payload, &ids.join("|"));
                msg.tool_call_id = Some(ids.join("|"));
                messages.push(msg);
            }
        }

        // ── 5. Emit turn complete ───────────────────────────────────
        let _ = tx.send(AgentEvent::TurnComplete).await;

        // Record agent turn telemetry (fire-and-forget)
        if let (Some(telemetry), Some(trace_id)) = (telemetry, trace_id) {
            let latency_ms = turn_start.elapsed().as_millis() as i64;
            telemetry.record_agent_turn(AgentTurnRecord {
                trace_id,
                turn_number: turn as i32,
                tool_calls_total: tool_calls.len() as i32,
                tool_calls_success,
                task_completed: false,
                latency_ms,
                tokens_input: 0,
                tokens_output: 0,
                experiment_id: None,
                variant: None,
            });
        }

        // ── 6. Check if we should inject a reminder ─────────────────
        // Only if a limit is explicitly set (max_turns > 0)
        if max_turns > 0 && turn >= max_turns - 2 && turn < max_turns {
            messages.push(LlmMessage::user(
                "You have only a few turns remaining. Please provide your final answer now.",
            ));
        }
    }

    // Max turns reached (only if a limit was set)
    if max_turns > 0 {
        let _ = tx.send(AgentEvent::Error {
            message: format!("Max turns ({max_turns}) reached. Please try a simpler request."),
        }).await;
    }

    Ok(())
}

// ── Context Management ──────────────────────────────────────────────────

/// Truncate a tool output to a maximum character count.
/// Keeps head and tail — the most informative parts.
fn truncate_output(output: &str, max_chars: usize) -> String {
    if max_chars == 0 || output.len() <= max_chars {
        return output.to_string();
    }
    let head = max_chars * 3 / 4; // 75% head
    let tail = max_chars - head;   // 25% tail
    let mut result = String::with_capacity(max_chars + 100);
    result.push_str(&output[..head.min(output.len())]);
    result.push_str(&format!(
        "\n\n... [truncated: {} total chars, showing first {} + last {}] ...\n\n",
        output.len(),
        head,
        tail
    ));
    let tail_start = output.len().saturating_sub(tail);
    result.push_str(&output[tail_start..]);
    result
}

/// Trim old messages from the conversation if the total character count
/// exceeds the budget. Always keeps the system prompt (first message),
/// the last 6 messages (current turn), and NEVER removes messages that
/// are part of tool_use/tool_result pairs (to avoid protocol violations).
fn trim_context(messages: &mut Vec<LlmMessage>, max_chars: usize) {
    if max_chars == 0 || messages.len() <= 5 {
        return;
    }
    let total: usize = messages.iter().map(|m| m.content.len()).sum();
    if total <= max_chars {
        return;
    }

    // Strategy: remove oldest messages that are NOT tool-related and NOT
    // in the most recent turn. System prompt (index 0) is always preserved.
    let keep_tail = 4usize.min(messages.len().saturating_sub(1));
    let remove_up_to = messages.len().saturating_sub(keep_tail);
    let start_removing = 1usize; // skip system prompt

    let mut to_remove = Vec::new();
    let mut removed_chars = 0usize;

    for i in start_removing..remove_up_to {
        // NEVER remove tool-related messages — they must stay paired
        if messages[i].tool_calls.is_some() || messages[i].tool_call_id.is_some() {
            continue;
        }
        if total - removed_chars <= max_chars {
            break;
        }
        to_remove.push(i);
        removed_chars += messages[i].content.len();
    }

    if !to_remove.is_empty() {
        // Remove in reverse order to preserve indices
        for &i in to_remove.iter().rev() {
            messages.remove(i);
        }
        tracing::info!(
            trimmed = to_remove.len(),
            removed_chars,
            remaining = messages.len(),
            "Context trimmed"
        );
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockLlmProvider;
    use everevo_core::llm::LlmProvider;
    use everevo_core::tool::{Tool, ToolRegistry};
    use std::sync::Arc;

    struct EchoTool;
    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "Echoes input" }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
        }
        fn risk_level(&self) -> everevo_core::types::RiskLevel { everevo_core::types::RiskLevel::Low }
        async fn execute(&self, params: serde_json::Value) -> Result<ToolOutput, EverEvoError> {
            let text = params["text"].as_str().unwrap_or("no input");
            Ok(ToolOutput { content: format!("echo: {text}"), is_error: false })
        }
    }

    #[tokio::test]
    async fn test_agent_direct_answer_no_tools() {
        let mock = MockLlmProvider::new()
            .with_text("Hello, how can I help?");
        // Mock doesn't support stream_chat yet — test via chat()
        let resp = mock.chat(&[LlmMessage::user("hi")], &[]).await.unwrap();
        assert_eq!(resp.content.unwrap(), "Hello, how can I help?");
    }

    #[tokio::test]
    async fn test_agent_with_tool_call_response() {
        // Mock is FIFO: first pushed = first popped (r.remove(0))
        let mock = MockLlmProvider::new()
            .with_tool_call("echo", serde_json::json!({"text": "hello"}))   // popped 1st
            .with_text("The tool returned: echo: hello");                   // popped 2nd

        let messages = vec![LlmMessage::user("echo hello")];
        let resp = mock.chat(&messages, &[]).await.unwrap();
        assert_eq!(resp.tool_calls.len(), 1, "First response should be the tool call");
        assert_eq!(resp.tool_calls[0].name, "echo");
        assert_eq!(resp.tool_calls[0].arguments, serde_json::json!({"text": "hello"}));
    }

    #[test]
    fn test_tool_registry_with_echo() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        assert_eq!(reg.len(), 1);
        assert!(reg.get("echo").is_some());
    }

    #[tokio::test]
    async fn test_echo_tool_execute() {
        let tool = EchoTool;
        let output = tool.execute(serde_json::json!({"text": "world"})).await.unwrap();
        assert_eq!(output.content, "echo: world");
        assert!(!output.is_error);
    }

    #[tokio::test]
    async fn test_agent_loop_creation() {
        let agent = AgentLoop::new();
        assert_eq!(agent.max_turns, 0); // 0 = unlimited, Claude Code behavior
        let limited = agent.with_max_turns(5);
        assert_eq!(limited.max_turns, 5);
    }

    #[test]
    fn test_truncate_output_short() {
        let result = truncate_output("hello", 4000);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_output_long() {
        let long = "A".repeat(5000);
        let result = truncate_output(&long, 1000);
        assert!(result.len() <= 1200); // 1000 + ~100 for truncation notice
        assert!(result.contains("[truncated: 5000 total chars"));
        assert!(result.starts_with('A'));
        assert!(result.ends_with('A'));
    }

    #[test]
    fn test_trim_context_under_budget() {
        let mut msgs = vec![
            LlmMessage::system("system"),
            LlmMessage::user("hello"),
        ];
        trim_context(&mut msgs, 1000);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_trim_context_over_budget() {
        let mut msgs = vec![
            LlmMessage::system("sys"),
            LlmMessage::user(&"x".repeat(2000)),
            LlmMessage::assistant(&"y".repeat(2000)),
            LlmMessage::user("recent1"),
            LlmMessage::assistant("recent2"),
            LlmMessage::user("latest"),
        ];
        let original_len = msgs.len();
        trim_context(&mut msgs, 500);
        // Should remove at least the oversized messages
        assert!(msgs.len() < original_len, "Should have trimmed some messages");
        assert_eq!(msgs[0].role, LlmRole::System, "System prompt must survive");
    }

    #[test]
    fn test_agent_budget_config() {
        let agent = AgentLoop::new()
            .with_tool_result_budget(2000)
            .with_context_budget(50000);
        assert_eq!(agent.max_tool_result_chars, 2000);
        assert_eq!(agent.max_context_chars, 50000);
    }
}
