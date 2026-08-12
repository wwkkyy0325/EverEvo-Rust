//! ReAct agent loop — the core execution cycle.
//!
//! Single-threaded while-loop, inspired by Claude Code's `nO` master loop.
//! Intentionally simple: flat, debuggable, reliable.
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
//! │      tool.execute()                     │
//! │      append tool_result to messages      │
//! │                                         │
//! │    turn += 1                            │
//! └─────────────────────────────────────────┘
//!     │
//!     ▼
//! Final Response (or Error / MaxTurns)
//! ```

pub mod event;
pub mod hooks;
pub mod proactivity;
pub mod trim;

mod convergence;
mod driver;
mod retrospective;

#[allow(unused_imports)]
pub(crate) use convergence::{budget_line, convergence_stage, forced_final_prompt, Convergence};
pub use event::AgentEvent;
#[allow(unused_imports)]
pub(crate) use proactivity::{hash_args, hash_str};
pub use proactivity::{EscalationLevel, ProactivityState};

use driver::run_loop;
use hooks::execute_with_hooks;

use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use everevo_core::llm::{LlmMessage, LlmProvider, LlmRole, StreamEvent, ToolSchema};
use everevo_core::tool::ToolRegistry;
use everevo_core::TelemetryPipeline;
use uuid::Uuid;

// ── Panic Recovery ────────────────────────────────────────────────────────

/// Extract a human-readable message from a panic payload caught by catch_unwind.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
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
    /// Optional telemetry pipeline for recording agent turn metrics.
    telemetry: Option<Arc<TelemetryPipeline>>,
    /// Trace ID for correlating telemetry records.
    trace_id: Option<Uuid>,
    /// Pending subagent results channel — TaskTool pushes, AgentLoop drains.
    subagent_rx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>>,
    /// Pending sub-agent count — shared with TaskTool.
    pending_subagents: Arc<std::sync::atomic::AtomicUsize>,
    /// Cancellation token for aborting LLM calls.
    cancel_token: Option<tokio_util::sync::CancellationToken>,
    /// Shared focus hint from CompactTool → autocompact.
    /// CompactTool writes; autocompact reads and clears.
    compact_focus: Option<Arc<std::sync::Mutex<Option<String>>>>,
    /// Proactivity state — tracks fixation loops and escalation.
    /// Wrapped in Arc<Mutex<>> so run_loop() can update it and the caller can read it.
    proactivity_state: Option<Arc<std::sync::Mutex<ProactivityState>>>,
    /// Meta-agent state — cross-turn pattern diagnosis and hint injection.
    meta_agent_state: Option<Arc<std::sync::Mutex<crate::memory::meta_agent::MetaAgentState>>>,
    /// Hook feedback slot — ReflectGateHook writes tool error feedback here;
    /// the loop reads+clears it after tool execution and injects as a user message.
    hook_feedback_slot: Option<Arc<std::sync::Mutex<Option<String>>>>,
    /// Layer-1 background rolling-summary maintenance (wired by the server for
    /// real sessions; None for sub-agents). Runs at soft-threshold turn
    /// boundaries without blocking the main loop.
    background_maintenance: Option<Arc<crate::context::BackgroundMaintenance>>,
    /// Optional compaction model (cheap model for summarization). None → fall
    /// back to the main loop model (decision 1: "有哪个用哪个").
    compact_llm: Option<Arc<dyn LlmProvider>>,
    /// Directory for paged tool outputs (`data/sessions/<id>/tool_cache`).
    /// None → large tool outputs are truncated in context as before (sub-agents).
    tool_cache_dir: Option<PathBuf>,
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
            cancel_token: None,
            compact_focus: None,
            proactivity_state: None,
            meta_agent_state: None,
            hook_feedback_slot: None,
            background_maintenance: None,
            compact_llm: None,
            tool_cache_dir: None,
        }
    }

    /// Set the directory where large tool outputs are paged to disk (spec
    /// deliverable 6). The agent can re-read them via `tool_cache_read`.
    pub fn with_tool_cache_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.tool_cache_dir = dir;
        self
    }

    /// Wire Layer-1 background rolling-summary maintenance. Pass `None` for
    /// sub-agents and tests.
    pub fn with_background_maintenance(
        mut self,
        bg: Option<Arc<crate::context::BackgroundMaintenance>>,
    ) -> Self {
        self.background_maintenance = bg;
        self
    }

    /// Set the compaction model (used by autocompact and rolling summary).
    /// `None` falls back to the main loop model.
    pub fn with_compact_llm(mut self, llm: Option<Arc<dyn LlmProvider>>) -> Self {
        self.compact_llm = llm;
        self
    }

    pub fn with_compact_focus(mut self, focus: Arc<std::sync::Mutex<Option<String>>>) -> Self {
        self.compact_focus = Some(focus);
        self
    }

    /// Enable proactivity tracking with the given initial state.
    /// When enabled, the loop detects fixation patterns (same tool + same error
    /// across turns) and injects escalating intervention messages at L1-L3.
    pub fn with_proactivity(mut self, state: Arc<std::sync::Mutex<ProactivityState>>) -> Self {
        self.proactivity_state = Some(state);
        self
    }

    /// Enable meta-agent cross-turn pattern diagnosis.
    /// When enabled, the loop injects meta-agent hints at turn boundaries
    /// and triggers background diagnosis on interval or escalation.
    pub fn with_meta_agent(
        mut self,
        state: Arc<std::sync::Mutex<crate::memory::meta_agent::MetaAgentState>>,
    ) -> Self {
        self.meta_agent_state = Some(state);
        self
    }

    /// Wire the ReflectGateHook feedback slot so the loop reads tool error
    /// feedback after each tool execution and injects it into the conversation.
    pub fn with_hook_feedback(mut self, slot: Arc<std::sync::Mutex<Option<String>>>) -> Self {
        self.hook_feedback_slot = Some(slot);
        self
    }

    pub fn with_cancel_token(mut self, token: tokio_util::sync::CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    pub fn with_subagent_channel(self, rx: tokio::sync::mpsc::UnboundedReceiver<String>) -> Self {
        *self.subagent_rx.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
        self
    }

    pub fn with_pending_subagents(mut self, pending: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        self.pending_subagents = pending;
        self
    }

    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    pub fn with_tool_result_budget(mut self, chars: usize) -> Self {
        self.max_tool_result_chars = chars;
        self
    }

    pub fn with_context_budget(mut self, chars: usize) -> Self {
        self.max_context_chars = chars;
        self
    }

    pub fn with_telemetry(mut self, telemetry: Arc<TelemetryPipeline>, trace_id: Uuid) -> Self {
        self.telemetry = Some(telemetry);
        self.trace_id = Some(trace_id);
        self
    }

    // ── Factory constructors ────────────────────────────────────────────

    /// Full-featured main session loop (chat in server mode).
    /// Wires sub-agent channels, cancellation, compaction focus, and proactivity.
    pub fn main_session(
        subagent_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
        pending_subagents: Arc<std::sync::atomic::AtomicUsize>,
        cancel_token: tokio_util::sync::CancellationToken,
        compact_focus: Arc<std::sync::Mutex<Option<String>>>,
        proactivity: Arc<std::sync::Mutex<ProactivityState>>,
    ) -> Self {
        Self::new()
            .with_subagent_channel(subagent_rx)
            .with_pending_subagents(pending_subagents)
            .with_cancel_token(cancel_token)
            .with_compact_focus(compact_focus)
            .with_proactivity(proactivity)
    }

    /// Standard sub-agent loop (TaskTool, Team, Workflow, Cluster, A2A).
    /// Sets a turn limit; the caller can chain additional `.with_*()` methods.
    pub fn sub_agent(max_turns: usize) -> Self {
        Self::new().with_max_turns(max_turns)
    }

    /// CLI chat loop (standalone binary mode, 30-turn limit).
    pub fn cli() -> Self {
        Self::new().with_max_turns(30)
    }

    /// Run the ReAct loop with streaming output via AgentEvent channel.
    #[allow(clippy::type_complexity)]
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
        let subagent_rx = self
            .subagent_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let pending_subagents = self.pending_subagents.clone();

        let cancel = self.cancel_token.clone();
        let compact_focus = self.compact_focus.clone();
        let proactivity = self.proactivity_state.clone();
        let meta_agent = self.meta_agent_state.clone();
        let hook_feedback_slot = self.hook_feedback_slot.clone();
        let background = self.background_maintenance.clone();
        let compact_llm = self.compact_llm.clone();
        let tool_cache_dir = self.tool_cache_dir.clone();
        tokio::spawn(async move {
            let mut tool_schemas: Vec<ToolSchema> = tools
                .as_tool_schemas()
                .into_iter()
                .map(|s| ToolSchema {
                    name: s["function"]["name"].as_str().unwrap_or("").into(),
                    description: s["function"]["description"].as_str().unwrap_or("").into(),
                    parameters: s["function"]["parameters"].clone(),
                    native_type: None,
                })
                .collect();
            // Native server-side web search: drop any registry schema named
            // "web_search" (webagent MCP collision), then declare the native tool.
            if let Some(native) = llm.native_web_search_tool() {
                tool_schemas.retain(|t| t.name != "web_search");
                tool_schemas.push(native);
            }

            // Benchmark mode (EVEREVO_BENCHMARK=1): thread a task-level wall-clock
            // deadline so the loop can inject escalating convergence nudges and a
            // forced terminal commit instead of the plain max_turns error. The
            // default 300s matches the harness per-question timeout. Non-benchmark
            // runs pass None and keep byte-identical behavior.
            let wall_deadline = if std::env::var("EVEREVO_BENCHMARK").is_ok() {
                Some(std::time::Instant::now() + std::time::Duration::from_secs(300))
            } else {
                None
            };

            match AssertUnwindSafe(run_loop(
                &llm,
                &tools,
                &tool_schemas,
                &mut messages,
                max_turns,
                wall_deadline,
                max_tool_result_chars,
                max_context_chars,
                confirmation.as_deref(),
                telemetry.as_ref(),
                trace_id,
                subagent_rx,
                &pending_subagents,
                &tx,
                cancel.as_ref(),
                &compact_focus,
                &proactivity,
                &meta_agent,
                &hook_feedback_slot,
                compact_llm.as_deref(),
                background.as_ref(),
                tool_cache_dir.as_deref(),
            ))
            .catch_unwind()
            .await
            {
                Ok(Ok(())) => {} // normal completion
                Ok(Err(e)) => {
                    // run_loop returned a normal error
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: e.to_string(),
                        })
                        .await;
                }
                Err(panic) => {
                    // run_loop panicked — catch_unwind recovered it
                    let msg = panic_message(&panic);
                    tracing::error!(%msg, "Agent loop panicked — recovered by catch_unwind");
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: format!("Internal agent error: {msg}"),
                        })
                        .await;
                }
            }
        });

        rx
    }

    /// Run the ReAct loop synchronously and collect the final response as a String.
    /// Designed for sub-agents and workflow tasks that need a simple text result.
    ///
    /// Returns the accumulated text from all turns, or an error string.
    pub async fn run_subagent(
        &self,
        llm: Arc<crate::llm::HttpClient>,
        tools: Arc<ToolRegistry>,
        messages: Vec<LlmMessage>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> String {
        if cancel.is_cancelled() {
            return "Cancelled.".into();
        }

        let max_turns = if self.max_turns == 0 {
            3
        } else {
            self.max_turns
        };
        let mut tool_schemas: Vec<ToolSchema> = tools
            .as_tool_schemas()
            .into_iter()
            .map(|s| ToolSchema {
                name: s["function"]["name"].as_str().unwrap_or("").into(),
                description: s["function"]["description"].as_str().unwrap_or("").into(),
                parameters: s["function"]["parameters"].clone(),
                native_type: None,
            })
            .collect();
        // Native server-side web search (see run()).
        if let Some(native) = llm.native_web_search_tool() {
            tool_schemas.retain(|t| t.name != "web_search");
            tool_schemas.push(native);
        }

        let mut text = String::new();
        let mut messages = messages;
        // Bound native-server-search truncation-continue retries across turns.
        let mut truncation_continues = 0;

        for _turn in 0..max_turns {
            if cancel.is_cancelled() {
                return format!("{text}\n[Cancelled]");
            }

            // ── Notify hooks of new turn (resets per-turn state) ──
            for hook in &tools.hooks {
                hook.on_turn_start().await;
            }

            let token_rx = match llm
                .stream_chat(&messages, &tool_schemas, Some(cancel.clone()))
                .await
            {
                Ok(rx) => rx,
                Err(e) => return format!("Error: {e}"),
            };

            let mut turn_text = String::new();
            let mut current_thinking = String::new();
            let mut tool_calls: Vec<everevo_core::types::ToolCall> = Vec::new();
            let mut pending_tool: Option<(String, String, String)> = None;
            let mut saw_server_tool = false;
            let mut last_stop_reason: Option<String> = None;

            let mut rx = token_rx;
            loop {
                let event =
                    tokio::time::timeout(std::time::Duration::from_secs(120), rx.recv()).await;
                let event = match event {
                    Ok(Some(e)) => e,
                    Ok(None) => break, // channel closed
                    Err(_elapsed) => {
                        return format!(
                            "{text}{turn_text}\nError: LLM stream stalled (no events for 120s)"
                        );
                    }
                };
                if cancel.is_cancelled() {
                    return format!("{text}{turn_text}\n[Cancelled]");
                }
                match event {
                    StreamEvent::Text(t) => {
                        turn_text.push_str(&t);
                    }
                    StreamEvent::Thinking(t) => {
                        current_thinking.push_str(&t);
                    }
                    StreamEvent::ToolCallStart { id, name } => {
                        pending_tool = Some((id, name, String::new()));
                    }
                    StreamEvent::ToolCallArg { id, arg_delta } => {
                        if let Some((ref pid, _, ref mut args)) = pending_tool {
                            if pid == &id {
                                args.push_str(&arg_delta);
                            }
                        }
                    }
                    StreamEvent::ServerToolUse { .. } => {
                        // Provider-executed tool (native web search) — nothing to dispatch.
                        saw_server_tool = true;
                    }
                    StreamEvent::Done { stop_reason, .. } => {
                        last_stop_reason = stop_reason;
                        if let Some((id, name, args_str)) = pending_tool.take() {
                            let args: serde_json::Value =
                                serde_json::from_str(&args_str).unwrap_or_default();
                            tool_calls.push(everevo_core::types::ToolCall {
                                id,
                                name,
                                arguments: args,
                            });
                        }
                        break;
                    }
                    StreamEvent::Error(msg) => {
                        turn_text.push_str(&format!("\n[LLM Error] {msg}"));
                        break;
                    }
                }
            }

            // Native server-side search truncated (stop_reason=max_tokens): continue
            // with the partial context; server blocks are NOT replayed (400 risk).
            if tool_calls.is_empty()
                && saw_server_tool
                && last_stop_reason.as_deref() == Some("max_tokens")
                && truncation_continues < 4
            {
                truncation_continues += 1;
                messages.push(LlmMessage {
                    role: LlmRole::Assistant,
                    content: turn_text.clone(),
                    thinking: if current_thinking.is_empty() {
                        None
                    } else {
                        Some(std::mem::take(&mut current_thinking))
                    },
                    tool_calls: None,
                    tool_call_id: None,
                    images: Vec::new(),
                });
                continue;
            }

            // No tool calls → check for pending sub-agents before declaring Done
            if tool_calls.is_empty() {
                let pending = self
                    .pending_subagents
                    .load(std::sync::atomic::Ordering::SeqCst);
                if pending > 0 {
                    // Sub-agents still running — inject reminder and continue
                    messages.push(LlmMessage::user(format!(
                        "You have {} sub-agent(s) still running. Wait for their results \
                         before providing a final answer. Do NOT call Done yet.",
                        pending
                    )));
                    continue;
                }
                text.push_str(&turn_text);
                break;
            }

            // Push assistant message with tool calls
            messages.push(LlmMessage {
                role: LlmRole::Assistant,
                content: turn_text.clone(),
                thinking: if current_thinking.is_empty() {
                    None
                } else {
                    Some(std::mem::take(&mut current_thinking))
                },
                tool_calls: Some(tool_calls.clone()),
                tool_call_id: None,
                images: Vec::new(),
            });
            text.push_str(&turn_text);

            // Execute tools and collect results
            let mut results: Vec<(String, String)> = Vec::new();
            for tc in &tool_calls {
                if cancel.is_cancelled() {
                    break;
                }
                let result = match tools.get(&tc.name) {
                    Some(tool) => match execute_with_hooks(
                        tool.as_ref(),
                        &tc.name,
                        &tc.arguments,
                        None,
                        &tools.hooks,
                    )
                    .await
                    {
                        Ok(o) => format!("[{}]: {}", tc.name, o.content),
                        Err(e) => format!("[{} error]: {e}", tc.name),
                    },
                    None => format!("Unknown tool: {}", tc.name),
                };
                results.push((tc.id.clone(), result));
            }

            // Push tool results as user message
            if results.len() == 1 {
                let (id, content) = results.into_iter().next().unwrap();
                messages.push(LlmMessage::tool(&content, &id));
            } else if !results.is_empty() {
                let payload = results
                    .iter()
                    .map(|(id, c)| format!("[{id}]: {c}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let ids = results
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
                    .join("|");
                messages.push(LlmMessage::tool(&payload, &ids));
            }
        }

        text
    }
}

impl Default for AgentLoop {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockLlmProvider;
    use everevo_core::llm::LlmProvider;
    use everevo_core::tool::{Tool, ToolOutput, ToolRegistry};
    use everevo_core::EverEvoError;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct EchoTool;
    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes input"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
        }
        fn risk_level(&self) -> everevo_core::types::RiskLevel {
            everevo_core::types::RiskLevel::Low
        }
        async fn execute(
            &self,
            params: serde_json::Value,
            _cancel: Option<&CancellationToken>,
        ) -> Result<ToolOutput, EverEvoError> {
            let text = params["text"].as_str().unwrap_or("no input");
            Ok(ToolOutput {
                content: format!("echo: {text}"),
                is_error: false,
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn test_agent_direct_answer_no_tools() {
        let mock = MockLlmProvider::new().with_text("Hello, how can I help?");
        let resp = mock.chat(&[LlmMessage::user("hi")], &[]).await.unwrap();
        assert_eq!(resp.content.unwrap(), "Hello, how can I help?");
    }

    #[tokio::test]
    async fn test_agent_with_tool_call_response() {
        let mock = MockLlmProvider::new()
            .with_tool_call("echo", serde_json::json!({"text": "hello"}))
            .with_text("The tool returned: echo: hello");

        let messages = vec![LlmMessage::user("echo hello")];
        let resp = mock.chat(&messages, &[]).await.unwrap();
        assert_eq!(
            resp.tool_calls.len(),
            1,
            "First response should be the tool call"
        );
        assert_eq!(resp.tool_calls[0].name, "echo");
        assert_eq!(
            resp.tool_calls[0].arguments,
            serde_json::json!({"text": "hello"})
        );
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
        let output = tool
            .execute(serde_json::json!({"text": "world"}), None)
            .await
            .unwrap();
        assert_eq!(output.content, "echo: world");
        assert!(!output.is_error);
    }

    #[tokio::test]
    async fn test_agent_loop_creation() {
        let agent = AgentLoop::new();
        assert_eq!(agent.max_turns, 0);
        let limited = agent.with_max_turns(5);
        assert_eq!(limited.max_turns, 5);
    }

    // ── Convergence nudge thresholds (benchmark mode) ─────────────────────

    fn is_commit(s: Convergence) -> bool {
        matches!(s, Convergence::Commit)
    }
    fn is_converge(s: Convergence) -> bool {
        matches!(s, Convergence::Converge)
    }

    #[test]
    fn test_convergence_turn_thresholds() {
        // max_turns=10: turn 7 → 70% → Converge; turn 9 → 90% → Commit.
        assert!(is_converge(convergence_stage(7, 10, 1.0)));
        assert!(is_commit(convergence_stage(9, 10, 1.0)));
        // Early turns stay None regardless of wall-clock.
        assert!(matches!(convergence_stage(3, 10, 1.0), Convergence::None));
        // Boundary: exactly 70% is Converge, 85% is Commit.
        assert!(is_converge(convergence_stage(7, 10, 1.0)));
        assert!(is_commit(convergence_stage(9, 10, 1.0)));
    }

    #[test]
    fn test_convergence_wall_clock_thresholds() {
        // Wall-clock alone drives convergence when turns are unbounded.
        assert!(matches!(convergence_stage(1, 0, 0.5), Convergence::None));
        assert!(is_converge(convergence_stage(1, 0, 0.30)));
        assert!(is_commit(convergence_stage(1, 0, 0.15)));
        // Wall-clock can force Commit even when turn budget is fresh.
        assert!(is_commit(convergence_stage(1, 10, 0.10)));
    }

    #[test]
    fn test_budget_line_format() {
        assert_eq!(
            budget_line(Some(3), Some(90)),
            "[Budget: 3 turns left, ~90s wall-clock left]"
        );
        assert_eq!(
            budget_line(None, Some(90)),
            "[Budget: unbounded turns left, ~90s wall-clock left]"
        );
        assert_eq!(budget_line(Some(1), None), "[Budget: 1 turns left]");
    }

    #[test]
    fn test_forced_final_prompt_contains_marker() {
        let p = forced_final_prompt();
        assert!(p.contains("Final answer: <value>"));
        assert!(p.contains("Do NOT call any tools"));
    }

    #[test]
    fn test_truncate_output_short() {
        let result = trim::truncate_output("hello", 4000);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_output_long() {
        let long = "A".repeat(5000);
        let result = trim::truncate_output(&long, 1000);
        assert!(result.len() <= 1200);
        assert!(result.contains("[truncated: 5000 total chars"));
        assert!(result.starts_with('A'));
        assert!(result.ends_with('A'));
    }

    // ── ProactivityState tests ──────────────────────────────────────────

    #[test]
    fn test_proactivity_starts_normal() {
        let ps = ProactivityState::new();
        assert_eq!(ps.level, EscalationLevel::Normal);
        assert!(ps.intervention_message().is_none());
    }

    #[test]
    fn test_escalation_detects_fixation() {
        let mut ps = ProactivityState::new();
        let args_h = hash_args(&serde_json::json!({"cmd": "cargo build"}));

        // Same tool, same args, error — once (no escalation yet)
        ps.update("shell", true, args_h, None);
        assert_eq!(ps.level, EscalationLevel::Normal);

        // Same again — now L1 Hint
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        assert_eq!(ps.level, EscalationLevel::Hint);
        assert!(ps.intervention_message().is_some());

        // Same again — L2 ResearchRequired
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        assert_eq!(ps.level, EscalationLevel::ResearchRequired);

        // Same again — L3 ForcedDivergence
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        assert_eq!(ps.level, EscalationLevel::ForcedDivergence);
    }

    #[test]
    fn test_escalation_resets_on_new_approach() {
        let mut ps = ProactivityState::new();
        let shell_args = hash_args(&serde_json::json!({"cmd": "cargo build"}));

        // Get to L2
        ps.update("shell", true, shell_args, None);
        ps.update("shell", true, shell_args, Some(("shell", shell_args)));
        ps.update("shell", true, shell_args, Some(("shell", shell_args)));
        assert_eq!(ps.level, EscalationLevel::ResearchRequired);

        // Switch to a different tool (web_search) — should not escalate further
        let ws_args = hash_args(&serde_json::json!({"query": "cargo build error"}));
        ps.update("web_search", false, ws_args, Some(("shell", shell_args)));
        // Web_search succeeded, approach changed → full reset
        assert_eq!(ps.level, EscalationLevel::Normal);
        assert!(ps.intervention_message().is_none());
    }

    #[test]
    fn test_escalation_ignores_different_errors() {
        let mut ps = ProactivityState::new();
        let args1 = hash_args(&serde_json::json!({"cmd": "cargo build"}));
        let args2 = hash_args(&serde_json::json!({"file": "src/main.rs"}));

        // First error with shell
        ps.update("shell", true, args1, None);
        assert_eq!(ps.level, EscalationLevel::Normal);

        // Different tool (read_file), different error — resets because approach changed
        // AND the error sig is different (different tool name)
        ps.update("read_file", true, args2, Some(("shell", args1)));
        // Different tool + different error → not a fixation pattern
        assert_eq!(ps.level, EscalationLevel::Normal);
    }

    #[test]
    fn test_different_tool_name_resets_error_sig() {
        let mut ps = ProactivityState::new();
        let args_h = hash_args(&serde_json::json!({"cmd": "bad"}));

        // Get to L2 with shell errors
        ps.update("shell", true, args_h, None);
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        assert_eq!(ps.level, EscalationLevel::ResearchRequired);

        // Switch to a different tool that also errors — new error sig starts fresh
        let new_args = hash_args(&serde_json::json!({"file": "missing.txt"}));
        ps.update("read_file", true, new_args, Some(("shell", args_h)));
        // First time this tool errors → Normal (new error pattern)
        assert_eq!(ps.level, EscalationLevel::Normal);
    }

    #[test]
    fn test_intervention_messages_per_level() {
        let mut ps = ProactivityState::new();
        let args_h = hash_args(&serde_json::json!({"cmd": "test"}));

        // L0: no message
        assert!(ps.intervention_message().is_none());

        // L1: hint message
        ps.update("shell", true, args_h, None);
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        let msg = ps.intervention_message().unwrap();
        assert!(msg.contains("DIFFERENT tool"));

        // L2: research required
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        let msg2 = ps.intervention_message().unwrap();
        assert!(msg2.contains("Research Before Retrying"));
        assert!(msg2.contains("web_search"));

        // L3: forced divergence
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        let msg3 = ps.intervention_message().unwrap();
        assert!(msg3.contains("Forced Divergence"));
        assert!(msg3.contains("fundamentally different"));
    }

    #[test]
    fn test_mark_researched() {
        let mut ps = ProactivityState::new();
        assert!(!ps.has_researched);
        ps.mark_researched();
        assert!(ps.has_researched);
    }

    #[test]
    fn test_successful_execution_does_not_escalate() {
        let mut ps = ProactivityState::new();
        let args_h = hash_args(&serde_json::json!({"cmd": "echo hello"}));

        // Successful execution repeated 5 times — no escalation
        for _ in 0..5 {
            ps.update("shell", false, args_h, Some(("shell", args_h)));
        }
        assert_eq!(ps.level, EscalationLevel::Normal);
    }

    #[test]
    fn test_trim_context_under_budget() {
        let mut msgs = vec![LlmMessage::system("system"), LlmMessage::user("hello")];
        trim::trim_context(&mut msgs, 1000);
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
        trim::trim_context(&mut msgs, 500);
        assert!(
            msgs.len() < original_len,
            "Should have trimmed some messages"
        );
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
