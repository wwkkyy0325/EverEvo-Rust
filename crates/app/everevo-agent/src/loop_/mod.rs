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
pub mod trim;
pub use event::AgentEvent;

use hooks::execute_with_hooks;

use futures::FutureExt;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use everevo_core::llm::{LlmMessage, LlmProvider, LlmRole, StreamEvent, ToolSchema};
use everevo_core::tool::ToolRegistry;
use everevo_core::types::ToolCall;
use everevo_core::EverEvoError;
use everevo_core::{TelemetryEmitContext, TelemetryPipeline};
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

// ── Proactivity State ────────────────────────────────────────────────────

/// Tracks fixation patterns across ReAct turns and escalates from gentle hints
/// to forced divergence. Design references:
/// - PUA Skill (tanweai): L1-L4 escalating pressure with mandatory actions
/// - Replit Decision-Time Guidance: ephemeral injections at decision points
/// - HASP (arXiv 2605.17734): executable guardrails with activation predicates
#[derive(Debug, Clone)]
pub struct ProactivityState {
    /// Current escalation level.
    pub level: EscalationLevel,
    /// Hash of last error signature (tool_name + error_substr) for dedup.
    last_error_sig: Option<u64>,
    /// Consecutive turns with the same error signature.
    same_error_count: u32,
    /// Whether WebSearch was used since the last escalation trigger.
    pub has_researched: bool,
    /// Count of distinct tool+arg combinations tried (proxy for "approaches").
    distinct_approaches: u32,
}

/// Escalation levels for fixation-loop intervention.
///
/// Level 0 (Normal) carries no overhead — no messages injected, no state tracked
/// beyond the default struct. Cost scales with escalation: nothing at L0, a single
/// line at L1, a paragraph at L2, a checklist at L3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EscalationLevel {
    /// Normal operation — no fixation detected.
    Normal = 0,
    /// First repeat: same tool + same error once. Gentle nudge.
    Hint = 1,
    /// Second repeat: web research required before retrying.
    ResearchRequired = 2,
    /// Third+ repeat: must enumerate fundamentally different approaches.
    ForcedDivergence = 3,
}

impl ProactivityState {
    pub fn new() -> Self {
        Self {
            level: EscalationLevel::Normal,
            last_error_sig: None,
            same_error_count: 0,
            has_researched: false,
            distinct_approaches: 0,
        }
    }

    /// Update state after a tool execution. Call once per tool result.
    ///
    /// `tool_name` — name of the executed tool.
    /// `is_error` — whether the result is an error.
    /// `args_hash` — a stable hash of the tool arguments, for distinguishing
    ///   "same approach" from "different approach."
    /// `prev_tool_sig` — the previous turn's (tool_name, args_hash), if any.
    pub fn update(
        &mut self,
        tool_name: &str,
        is_error: bool,
        args_hash: u64,
        prev_tool_sig: Option<(&str, u64)>,
    ) {
        if !is_error {
            // Non-error result → check if approach changed.
            if let Some((prev_name, prev_hash)) = prev_tool_sig {
                if prev_name != tool_name || prev_hash != args_hash {
                    // New approach + success → reset completely.
                    self.reset();
                    self.distinct_approaches += 1;
                    return;
                }
            }
            // Same approach succeeded — no escalation needed, but don't reset
            // (the success might be fragile; keep light tracking).
            return;
        }

        // Error path: compute signature and compare.
        let sig = hash_str(tool_name);

        if self.last_error_sig == Some(sig) {
            self.same_error_count += 1;
        } else {
            self.same_error_count = 1;
            self.last_error_sig = Some(sig);
            // New error type → check if approach changed.
            if let Some((prev_name, prev_hash)) = prev_tool_sig {
                if prev_name != tool_name || prev_hash != args_hash {
                    self.distinct_approaches += 1;
                }
            }
        }

        // Escalate based on same_error_count.
        self.level = match self.same_error_count {
            0..=1 => EscalationLevel::Normal,
            2 => EscalationLevel::Hint,
            3 => EscalationLevel::ResearchRequired,
            _ => EscalationLevel::ForcedDivergence,
        };
    }

    /// Build the intervention message to inject into the conversation, if any.
    /// Returns None at L0, a one-liner at L1, a paragraph at L2, a checklist at L3.
    pub fn intervention_message(&self) -> Option<String> {
        match self.level {
            EscalationLevel::Normal => None,
            EscalationLevel::Hint => Some(
                "\
[SYSTEM NOTE] Your last attempt with the same approach failed. \
Do NOT retry with minor parameter changes — it will fail again. \
Consider: is there a DIFFERENT tool or strategy? \
(SSH failing? Use HTTPS + token. API call failing? Use a different library. \
Command not found? Check what's installed with `which`.)"
                    .into(),
            ),
            EscalationLevel::ResearchRequired => Some(
                "\
## [REQUIRED] Research Before Retrying\n\n\
You have attempted the same approach twice and both failed. \
Before your next attempt you MUST:\n\
1. Call web_search for at least 2 relevant queries (include the exact error)\n\
2. Read the results and identify root causes\n\
3. Choose a FUNDAMENTALLY different approach — not just parameter tweaks\n\
   (e.g., SSH→HTTPS, one library→another, direct call→CLI tool)\n\
4. Explain your NEW approach before executing it\n\n\
If this is a connectivity issue (SSH, network), check: do you have a token \
configured? Use HTTPS with the token — it's already in the sandbox env."
                    .into(),
            ),
            EscalationLevel::ForcedDivergence => Some(
                "\
## [REQUIRED] Forced Divergence — Same Approach Failed 3+ Times\n\n\
You are stuck in a fixation loop. STOP retrying immediately.\n\n\
Complete ALL of these before ANY further action:\n\
- [ ] Re-read the LAST error message word-for-word — what EXACTLY failed?\n\
- [ ] web_search: the exact error message (copy-paste it)\n\
- [ ] web_search: alternative approaches to {your task}\n\
- [ ] List 3 DISTINCT hypotheses for why this fails\n\
- [ ] Choose the best alternative and explain WHY it will work\n\n\
**Common root causes for persistent failures**:\n\
- SSH to GitHub fails → use HTTPS + GH_TOKEN (it's in sandbox env)\n\
- Package install fails → check if the runtime is available (`which python`)\n\
- Build fails → read the ACTUAL error line, not the summary\n\
- Connection refused → the service may not be running; check with curl\n\
- Permission denied → you're in a sandbox; explain what you need\n\n\
Your NEXT action MUST be fundamentally different. If you truly cannot find \
an alternative, say: \"I've tried X, Y, Z. Here's what failed and what I need.\" \
Honesty about failure is better than an infinite retry loop."
                    .into(),
            ),
        }
    }

    /// Record that the agent used a research tool (web_search, web_fetch).
    pub fn mark_researched(&mut self) {
        self.has_researched = true;
    }

    fn reset(&mut self) {
        self.level = EscalationLevel::Normal;
        self.last_error_sig = None;
        self.same_error_count = 0;
        self.has_researched = false;
    }
}

impl Default for ProactivityState {
    fn default() -> Self {
        Self::new()
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn hash_args(args: &serde_json::Value) -> u64 {
    let mut h = DefaultHasher::new();
    args.to_string().hash(&mut h);
    h.finish()
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

// ── Retrospective ──────────────────────────────────────────────────────
// End-of-run execution summary: turns, tool calls, and failures classified
// as transient (environment) vs structural (implementation defect).

/// Cap a failure message for the retrospective (keep it compact).
fn truncate_for_retro(msg: &str) -> String {
    const MAX: usize = 160;
    if msg.len() <= MAX {
        msg.to_string()
    } else {
        format!("{}…", &msg[..MAX])
    }
}

/// Classify a failure message as transient (environmental, retryable) or
/// structural (needs a code fix). Mirrors `HttpClient::is_retryable` semantics
/// for tool/LLM failures surfaced in the loop.
fn classify_failure(msg: &str) -> &'static str {
    let lower = msg.to_ascii_lowercase();
    const TRANSIENT: &[&str] = &[
        "timed out",
        "timeout",
        "stalled",
        "network",
        "connection reset",
        "connection refused",
        "rate limit",
        "temporarily unavailable",
        "429",
        "502",
        "503",
        "504",
        "retry",
    ];
    if TRANSIENT.iter().any(|k| lower.contains(k)) {
        "transient"
    } else {
        "structural"
    }
}

/// Build the end-of-run retrospective markdown block.
fn build_retrospective(
    turns: i32,
    total_tool_calls: i32,
    total_tool_success: i32,
    failures: &[String],
) -> String {
    let failed = total_tool_calls - total_tool_success;
    let transient = failures
        .iter()
        .filter(|f| classify_failure(f) == "transient")
        .count();
    let structural = failures.len() - transient;

    let mut out = format!(
        "## 执行复盘\n\n- 轮次：{turns}\n- 工具调用：{total_tool_calls} 次（成功 {total_tool_success}，失败 {failed}）"
    );
    if failures.is_empty() {
        out.push_str("\n- 故障：无");
    } else {
        out.push_str(&format!(
            "\n- 故障：{} 处（临时性 {}，结构性 {}）",
            failures.len(),
            transient,
            structural
        ));
        for f in failures.iter().take(3) {
            out.push_str(&format!("\n  - {f}"));
        }
        if failures.len() > 3 {
            out.push_str(&format!("\n  - … 另有 {} 处", failures.len() - 3));
        }
    }
    if structural > 0 {
        out.push_str("\n- 优化点：结构性故障需修复底层逻辑；临时性故障可在后续轮次重试。");
    } else {
        out.push_str("\n- 优化点：本轮无结构性故障。");
    }
    out
}

// ── Loop Core ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
async fn run_loop(
    llm: &crate::llm::HttpClient,
    tools: &ToolRegistry,
    tool_schemas: &[ToolSchema],
    messages: &mut Vec<LlmMessage>,
    max_turns: usize,
    wall_clock_deadline: Option<std::time::Instant>,
    max_tool_result_chars: usize,
    max_context_chars: usize,
    confirmation: Option<&(dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync)>,
    telemetry: Option<&Arc<TelemetryPipeline>>,
    trace_id: Option<Uuid>,
    mut subagent_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    pending_subagents: &std::sync::atomic::AtomicUsize,
    tx: &mpsc::Sender<AgentEvent>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    compact_focus: &Option<Arc<std::sync::Mutex<Option<String>>>>,
    proactivity: &Option<Arc<std::sync::Mutex<ProactivityState>>>,
    meta_agent_state: &Option<Arc<std::sync::Mutex<crate::memory::meta_agent::MetaAgentState>>>,
    hook_feedback_slot: &Option<Arc<std::sync::Mutex<Option<String>>>>,
    compact_llm: Option<&dyn LlmProvider>,
    background: Option<&Arc<crate::context::BackgroundMaintenance>>,
    tool_cache_dir: Option<&Path>,
) -> Result<(), EverEvoError> {
    let mut turn = 0;
    // Bound the native-server-search truncation-continue retries across turns.
    let mut truncation_continues = 0;
    // Track the previous turn's tool signature for fixation detection.
    let mut prev_tool_sig: Option<(String, u64)> = None;
    // ── Run-level stats for the end-of-run retrospective ──────────
    let mut total_tool_calls = 0i32;
    let mut total_tool_success = 0i32;
    let mut failure_messages: Vec<String> = Vec::new();

    while max_turns == 0 || turn < max_turns {
        turn += 1;

        // ── Notify hooks of new turn (resets per-turn state) ──────
        for hook in &tools.hooks {
            hook.on_turn_start().await;
        }

        // Drain pending subagent results (non-blocking)
        if let Some(ref mut rx) = subagent_rx {
            while let Ok(result) = rx.try_recv() {
                messages.push(LlmMessage::user(format!("[SubAgent Result]\n{result}")));
            }
        }
        let turn_start = Instant::now();

        // ── Context management (Claude Code-aligned multi-layer) ────
        // Layer 0: Snip — zero-cost pruning of low-value tool results
        trim::snip_low_value_messages(messages);
        // Layer 1: Observation Masking — keep last N tool results, header older ones
        trim::mask_observations(messages);
        // Layer 2 (background): per-turn incremental rolling summary at the soft
        // threshold — non-blocking, writes only persisted state (spec rules
        // 5/6/7). Keeps the watermark low so Layer 3 rarely fires.
        let token_usage = trim::approx_tokens(messages.iter().map(|m| m.content.len()).sum());
        let token_limit = max_context_chars / 4;
        if let Some(bg) = background {
            use std::sync::atomic::Ordering;
            if !bg.in_flight.load(Ordering::Relaxed) && token_usage > (token_limit * 7) / 10
            // soft threshold 70%
            {
                bg.in_flight.store(true, Ordering::Relaxed);
                let bg = Arc::clone(bg);
                tokio::spawn(async move {
                    if let Err(e) = bg.maintain().await {
                        tracing::warn!(error = %e, "Background rolling-summary maintenance failed");
                    }
                    bg.in_flight.store(false, Ordering::Relaxed);
                });
                tracing::info!(
                    token_usage,
                    soft_limit = (token_limit * 7) / 10,
                    "Background rolling-summary maintenance spawned"
                );
            }
        }
        // Layer 3+4: Autocompact (LLM summarization) → Trim (hard drop fallback)
        // Trigger when approximate token count exceeds (context limit - buffer).
        // Uses the compaction model when configured, else the main model.
        if token_usage > token_limit.saturating_sub(trim::COMPACTION_BUFFER_TOKENS) {
            tracing::info!(token_usage, token_limit, "Context compaction triggered");
            // Read focus hint from CompactTool (if set), then clear it
            let focus = compact_focus.as_ref().and_then(|f| {
                let mut guard = f.lock().unwrap_or_else(|e| e.into_inner());
                guard.take()
            });
            let compact_model = compact_llm.unwrap_or(llm as &dyn LlmProvider);
            if trim::autocompact(messages, max_context_chars, compact_model, focus.as_deref()).await
                == 0
            {
                trim::trim_context(messages, max_context_chars);
            }
        }

        tracing::info!(turn, msg_count = messages.len(), "Agent turn start");

        // ── 1. Call LLM with context overflow recovery ─────────────
        // Claude Code error recovery waterfall:
        //   1. Force emergency compaction → retry
        //   2. Force aggressive trim → retry
        //   3. Give up → propagate error to user
        let token_rx = match llm
            .stream_chat(messages, tool_schemas, cancel.cloned())
            .await
        {
            Ok(rx) => rx,
            Err(e) => {
                let err_str = e.to_string();
                let is_overflow = err_str.contains("context_length_exceeded")
                    || err_str.contains("prompt too long")
                    || err_str.contains("413")
                    || err_str.contains("too many tokens")
                    || err_str.contains("maximum context length");

                if is_overflow {
                    tracing::warn!(
                        error = %err_str,
                        msg_count = messages.len(),
                        "Context overflow detected — attempting emergency compaction"
                    );
                    // Waterfall step 1: aggressive trim (no API call needed)
                    let before = messages.len();
                    trim::trim_context(messages, max_context_chars / 2); // halve the budget
                    let after = messages.len();
                    tracing::info!(before, after, trimmed = before - after, "Emergency trim");

                    // Retry
                    llm.stream_chat(messages, tool_schemas, cancel.cloned())
                        .await
                        .map_err(|e2| {
                            let e2_str = e2.to_string();
                            tracing::error!(error = %e2_str, "Context overflow persists after emergency trim");
                            EverEvoError::Agent(format!(
                                "Context is too long even after emergency compaction. \
                                 Try using /compact or starting a new session. Detail: {e2_str}"
                            ))
                        })?
                } else {
                    return Err(e);
                }
            }
        };

        let mut current_text = String::new();
        let mut current_thinking = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut pending_tool: Option<(String, String, String)> = None;
        let mut saw_server_tool = false;
        let mut last_stop_reason: Option<String> = None;

        let mut token_rx = token_rx;
        loop {
            // Stall guard — mirror the sub-agent loop's 120s per-event timeout so
            // a hung LLM stream can't block the main loop indefinitely.
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(120), token_rx.recv()).await;
            let event = match event {
                Ok(Some(e)) => e,
                Ok(None) => break, // channel closed
                Err(_elapsed) => {
                    let msg = "LLM stream stalled (no events for 120s)".to_string();
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: msg.clone(),
                        })
                        .await;
                    return Err(EverEvoError::Agent(msg));
                }
            };
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
                StreamEvent::ServerToolUse { .. } => {
                    // Provider-executed tool (native web search) — the provider
                    // runs it within this turn; nothing to dispatch.
                    saw_server_tool = true;
                }
                StreamEvent::Done { stop_reason, .. } => {
                    last_stop_reason = stop_reason;
                    if let Some((id, name, args_str)) = pending_tool.take() {
                        let arguments: serde_json::Value =
                            serde_json::from_str(&args_str).unwrap_or(serde_json::Value::Null);
                        let _ = tx
                            .send(AgentEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                            })
                            .await;
                        tool_calls.push(ToolCall {
                            id,
                            name,
                            arguments,
                        });
                    }
                    break;
                }
                StreamEvent::Error(msg) => {
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: msg.clone(),
                        })
                        .await;
                    return Err(EverEvoError::LlmProvider(msg));
                }
            }
        }

        // Native server-side search truncated (stop_reason=max_tokens): continue
        // the turn with the partial context instead of emitting a premature Done.
        // Server blocks are intentionally NOT replayed (an incomplete
        // `server_tool_use` in history makes the API reject with 400).
        if tool_calls.is_empty()
            && saw_server_tool
            && last_stop_reason.as_deref() == Some("max_tokens")
            && truncation_continues < 4
        {
            truncation_continues += 1;
            let thinking = if current_thinking.is_empty() {
                None
            } else {
                Some(current_thinking.clone())
            };
            messages.push(LlmMessage {
                role: LlmRole::Assistant,
                content: current_text.clone(),
                thinking,
                tool_calls: None,
                tool_call_id: None,
                images: Vec::new(),
            });
            tracing::info!(
                truncation_continues,
                "Native server-side search truncated (max_tokens) — continuing turn"
            );
            continue;
        }

        // If text but no tool calls → check for pending sub-agents first.
        if tool_calls.is_empty() {
            let pending = pending_subagents.load(std::sync::atomic::Ordering::SeqCst);
            if pending > 0 {
                if let Some(ref mut rx) = subagent_rx {
                    while let Ok(result) = rx.try_recv() {
                        messages.push(LlmMessage::user(format!("[SubAgent Result]\n{result}")));
                    }
                }
                tracing::info!(pending, "LLM says Done but sub-agents running — yielding");
                if !current_text.is_empty() {
                    let _ = tx.send(AgentEvent::TextDelta(current_text.clone())).await;
                }
                let _ = tx.send(AgentEvent::WaitingForSubAgents { pending }).await;
                return Ok(());
            }
            // No pending sub-agents → truly done.
            let final_text = current_text.clone();
            let summary = build_retrospective(
                turn as i32,
                total_tool_calls,
                total_tool_success,
                &failure_messages,
            );
            let _ = tx.send(AgentEvent::Retrospective { summary }).await;
            let _ = tx.send(AgentEvent::Done { final_text }).await;
            return Ok(());
        }

        // ── 2. Build assistant message with tool calls ──────────────
        let thinking = if current_thinking.is_empty() {
            None
        } else {
            Some(current_thinking.clone())
        };
        let assistant_msg = LlmMessage {
            role: LlmRole::Assistant,
            content: if current_text.is_empty() {
                String::new()
            } else {
                current_text.clone()
            },
            thinking,
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            images: Vec::new(),
        };
        messages.push(assistant_msg);

        // ── 3. Execute tools ────────────────────────────────────────
        let mut tool_result_pairs: Vec<(String, String, Vec<everevo_core::ImageData>)> = Vec::new();
        let mut tool_calls_success = 0i32;

        for tc in &tool_calls {
            total_tool_calls += 1;
            let tool = tools.get(&tc.name);
            if let Some(confirm_fn) = confirmation {
                if !confirm_fn(&tc.name, &tc.arguments) {
                    let skip_msg = format!("User declined execution of tool '{}'", tc.name);
                    let _ = tx
                        .send(AgentEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: skip_msg.clone(),
                            is_error: true,
                            images: Vec::new(),
                        })
                        .await;
                    tool_result_pairs.push((tc.id.clone(), skip_msg, Vec::new()));
                    continue;
                }
            }

            let result = match tool {
                Some(tool) => {
                    // Per-tool timeout: 300s for shell/build, 120s default.
                    // Prevents hung tools from blocking the agent loop indefinitely.
                    let timeout_secs = if tc.name == "shell" || tc.name.contains("build") {
                        300u64
                    } else {
                        120u64
                    };
                    let exec_fut = execute_with_hooks(
                        tool.as_ref(),
                        &tc.name,
                        &tc.arguments,
                        None,
                        &tools.hooks,
                    );
                    let result = match tokio::time::timeout(
                        std::time::Duration::from_secs(timeout_secs),
                        exec_fut,
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(_elapsed) => Err(EverEvoError::Tool {
                            tool: tc.name.clone(),
                            message: format!("Timed out after {timeout_secs}s"),
                        }),
                    };
                    // Report hook blocks via SSE
                    if let Err(ref e) = result {
                        if e.to_string().contains("blocked") {
                            let _ = tx
                                .send(AgentEvent::ToolCallEnd {
                                    id: tc.id.clone(),
                                    name: tc.name.clone(),
                                    content: format!("Tool blocked: {e}"),
                                    is_error: true,
                                    images: Vec::new(),
                                })
                                .await;
                            failure_messages.push(format!("{}: blocked", tc.name));
                            tool_result_pairs.push((
                                tc.id.clone(),
                                format!("Tool blocked: {e}"),
                                Vec::new(),
                            ));
                            continue;
                        }
                    }
                    result
                }
                None => Err(EverEvoError::Tool {
                    tool: tc.name.to_string(),
                    message: "Unknown tool".into(),
                }),
            };

            match result {
                Ok(output) => {
                    // Large tool outputs are paged to disk (spec deliverable 6):
                    // the context keeps a 2KB preview + absolute path, and the
                    // full text is retrievable via the `tool_cache_read` tool.
                    let truncated = match trim::page_tool_output(
                        &tc.name,
                        &tc.id,
                        &output.content,
                        tool_cache_dir,
                    )
                    .await
                    {
                        Some(paged) => paged,
                        None => trim::truncate_output(&output.content, max_tool_result_chars),
                    };
                    let _ = tx
                        .send(AgentEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: truncated.clone(),
                            is_error: output.is_error,
                            images: output.images.clone(),
                        })
                        .await;
                    if output.is_error {
                        if tc.name == "shell" && truncated.contains("确认")
                            || truncated.contains("confirmation")
                        {
                            let _ = tx
                                .send(AgentEvent::ConfirmationNeeded {
                                    command: tc
                                        .arguments
                                        .get("command")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    reason: truncated.clone(),
                                })
                                .await;
                        }
                        failure_messages.push(format!(
                            "{}: {}",
                            tc.name,
                            truncate_for_retro(&truncated)
                        ));
                        tracing::warn!(tool = %tc.name, "Tool returned error");
                    } else {
                        tool_calls_success += 1;
                        total_tool_success += 1;
                    }
                    tool_result_pairs.push((tc.id.clone(), truncated, output.images.clone()));
                }
                Err(e) => {
                    let err_msg = format!("Tool execution failed: {e}");
                    failure_messages.push(format!("{}: {err_msg}", tc.name));
                    let _ = tx
                        .send(AgentEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: err_msg.clone(),
                            is_error: true,
                            images: Vec::new(),
                        })
                        .await;
                    tool_result_pairs.push((tc.id.clone(), err_msg, Vec::new()));
                }
            }
        }

        // ── 3.5 Deduplicate near-identical tool results ─────────────
        // When N sub-agents/tools return the SAME observation (e.g.
        // "list_dir vs shell path inconsistency"), pushing all N results
        // floods the context with duplicates → model loops its thinking.
        if tool_result_pairs.len() > 3 {
            let original = tool_result_pairs.len();
            deduplicate_tool_results(&mut tool_result_pairs);
            if tool_result_pairs.len() < original {
                // At least one group was collapsed — log the reduction.
            }
        }

        // ── 4. Merge tool results into ONE user message ─────────────
        if !tool_result_pairs.is_empty() {
            if tool_result_pairs.len() == 1 {
                let (id, content, images) =
                    tool_result_pairs.into_iter().next().unwrap_or_default();
                let mut msg = LlmMessage::tool(&content, &id);
                if !images.is_empty() {
                    msg.images = images;
                }
                messages.push(msg);
            } else {
                let ids: Vec<String> = tool_result_pairs
                    .iter()
                    .map(|(id, _, _)| id.clone())
                    .collect();
                let all_images: Vec<_> = tool_result_pairs
                    .iter()
                    .flat_map(|(_, _, imgs)| imgs.clone())
                    .collect();
                let payload = serde_json::to_string(
                    &tool_result_pairs
                        .iter()
                        .map(|(id, content, _)| serde_json::json!({"i": id, "c": content}))
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default();
                let ids_joined = ids.join("|");
                let mut msg = LlmMessage::tool(&payload, &ids_joined);
                msg.tool_call_id = Some(ids_joined);
                if !all_images.is_empty() {
                    msg.images = all_images;
                }
                messages.push(msg);
            }
        }

        // ── 4.4 Hook feedback: read ReflectGateHook feedback ──────
        if let Some(ref slot) = hook_feedback_slot {
            let mut fb = slot.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(feedback) = fb.take() {
                messages.push(LlmMessage::user(format!("[TOOL FEEDBACK]\n{feedback}")));
                tracing::debug!(feedback_len = feedback.len(), "Hook feedback injected");
            }
        }

        // ── 4.5 Meta-Agent: inject pending hint at turn start ──────
        if let Some(ref meta_state) = meta_agent_state {
            let mut ms = meta_state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hint) = ms.take_hint() {
                messages.push(LlmMessage::user(format!("[META-AGENT HINT]\n{hint}")));
                tracing::debug!(hint_len = hint.len(), "Meta-agent hint injected");
            }
        }

        // ── 4.6 Proactivity: detect fixation and inject intervention ─
        if let Some(ref state) = proactivity {
            // Collect first-tool info for this turn's fixation tracking.
            let this_tool = tool_calls
                .first()
                .map(|tc| (tc.name.clone(), hash_args(&tc.arguments)));
            // Determine if this turn had any error.
            let has_error = (tool_calls_success as usize) < tool_calls.len();

            if let Some((ref name, args_h)) = this_tool {
                let prev_sig = prev_tool_sig.as_ref().map(|(n, h)| (n.as_str(), *h));
                let mut ps = state.lock().unwrap_or_else(|e| e.into_inner());
                ps.update(name, has_error, args_h, prev_sig);

                // Track web_search / web_fetch usage to mark research done.
                if name == "web_search" || name == "web_fetch" {
                    ps.mark_researched();
                }

                // Inject intervention message if escalation triggered.
                if let Some(intervention) = ps.intervention_message() {
                    messages.push(LlmMessage::user(&intervention));
                }

                prev_tool_sig = Some((name.clone(), args_h));
            }
        }

        // ── 4.7 Meta-Agent: trigger on interval or degradation ─────
        if let Some(ref meta) = meta_agent_state {
            let mut ms = meta.lock().unwrap_or_else(|e| e.into_inner());
            ms.increment_turn();
            let escalation = proactivity
                .as_ref()
                .map(|p| {
                    let ps = p.lock().unwrap_or_else(|e| e.into_inner());
                    ps.level as u32
                })
                .unwrap_or(0);
            if ms.should_trigger(escalation) && ms.has_llm() {
                ms.mark_triggered();
                // Fire-and-forget: spawn meta-diagnosis in background
                if let Some(ref llm) = ms.llm {
                    let llm = Arc::clone(llm);
                    let fm = ms.fact_manager.clone();
                    let meta_state = Arc::clone(meta);
                    // Build a summary of recent messages for the prompt
                    let recent_summary = messages
                        .iter()
                        .rev()
                        .take(10)
                        .map(|m| {
                            let role = match m.role {
                                everevo_core::llm::LlmRole::User => "U",
                                everevo_core::llm::LlmRole::Assistant => "A",
                                _ => "S",
                            };
                            let content = if m.content.chars().count() > 100 {
                                let truncated: String = m.content.chars().take(100).collect();
                                format!("{truncated}…")
                            } else {
                                m.content.clone()
                            };
                            format!("[{role}] {content}")
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    tokio::spawn(async move {
                        let hint = crate::memory::meta_agent::meta_diagnose(
                            &llm,
                            fm.as_deref(),
                            &crate::memory::paradigm::TrajectoryBuffer::default(),
                            escalation,
                            &recent_summary,
                        )
                        .await;
                        if let Some(h) = hint {
                            let mut ms = meta_state.lock().unwrap_or_else(|e| e.into_inner());
                            ms.set_hint(h);
                        }
                    });
                }
            }
        }

        // ── 5. Emit turn complete ───────────────────────────────────
        let _ = tx.send(AgentEvent::TurnComplete).await;

        if let Some(telemetry) = telemetry {
            let turn_error = (tool_calls_success as usize) < tool_calls.len();
            let (error_type, error_message) = if turn_error {
                let failed = tool_calls.len() as i32 - tool_calls_success;
                (
                    Some("tool_error".to_string()),
                    Some(format!(
                        "{failed} of {} tool calls failed",
                        tool_calls.len()
                    )),
                )
            } else {
                (None, None)
            };
            telemetry.emit(&TelemetryEmitContext {
                trace_id,
                turn_number: Some(turn as i32),
                tool_calls_total: Some(tool_calls.len() as i32),
                tool_calls_success: Some(tool_calls_success),
                task_completed: Some(false),
                turn_latency_ms: Some(turn_start.elapsed().as_millis() as i64),
                error_type,
                error_message,
                ..Default::default()
            });
        }

        // ── 6. Check if we should inject a reminder ─────────────────
        if let Some(deadline) = wall_clock_deadline {
            // Benchmark mode: escalating convergence nudges + a per-turn budget
            // line so the model feels both turn and wall-clock pressure.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let wall_frac = (remaining.as_secs_f64() / 300.0).clamp(0.0, 1.0);
            match convergence_stage(turn, max_turns, wall_frac) {
                Convergence::Commit => {
                    messages.push(LlmMessage::user(
                        "⏰ Deadline: STOP exploring. Your very next response MUST end with a \
                         single `Final answer:` line containing ONLY the value — best-effort \
                         beats no answer.",
                    ));
                }
                Convergence::Converge => {
                    messages.push(LlmMessage::user(
                        "⏰ Time check: start converging. Commit to a root cause, stop new \
                         exploration, and prepare a single `Final answer:` line.",
                    ));
                }
                Convergence::None => {}
            }
            let turns_left = if max_turns > 0 {
                Some(max_turns.saturating_sub(turn))
            } else {
                None
            };
            messages.push(LlmMessage::user(budget_line(
                turns_left,
                Some(remaining.as_secs()),
            )));
        } else if max_turns > 0 && turn >= max_turns - 2 && turn < max_turns {
            messages.push(LlmMessage::user(
                "You have only a few turns remaining. Please provide your final answer now.",
            ));
        }
    }

    if max_turns > 0 {
        if wall_clock_deadline.is_some() {
            // Benchmark forced terminal commit: one last no-tool LLM call for
            // ONLY the final answer, seeded from the full conversation (which
            // holds the model's own prior committed text), then emit Done so the
            // harness scorer / re-prompt sees a final_text instead of an error.
            messages.push(LlmMessage::user(forced_final_prompt()));
            let final_text = match llm.chat(messages, &[]).await {
                Ok(resp) => resp.content.unwrap_or_default(),
                Err(_) => String::new(),
            };
            let _ = tx.send(AgentEvent::Done { final_text }).await;
        } else {
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!(
                        "Max turns ({max_turns}) reached. Please try a simpler request."
                    ),
                })
                .await;
        }
    }

    Ok(())
}

// ── Convergence nudges (benchmark mode) ─────────────────────────────────────

/// Escalating convergence stage for the turn budget. Pure logic, unit-tested.
enum Convergence {
    /// Keep exploring (budget not yet tight).
    None,
    /// ~70% of turn budget / ~30% wall-clock left — start converging.
    Converge,
    /// ~85% of turn budget / ~15% wall-clock left — commit now.
    Commit,
}

fn convergence_stage(turn: usize, max_turns: usize, wall_left_frac: f64) -> Convergence {
    let turn_pct = if max_turns > 0 {
        turn as f64 / max_turns as f64
    } else {
        0.0
    };
    if (max_turns > 0 && turn_pct >= 0.85) || wall_left_frac <= 0.15 {
        Convergence::Commit
    } else if (max_turns > 0 && turn_pct >= 0.70) || wall_left_frac <= 0.30 {
        Convergence::Converge
    } else {
        Convergence::None
    }
}

/// Per-turn budget line appended to the conversation (benchmark mode).
fn budget_line(turns_left: Option<usize>, wall_left_secs: Option<u64>) -> String {
    let turns = match turns_left {
        Some(n) => format!("{n} turns left"),
        None => "unbounded turns left".to_string(),
    };
    match wall_left_secs {
        Some(s) => format!("[Budget: {turns}, ~{s}s wall-clock left]"),
        None => format!("[Budget: {turns}]"),
    }
}

/// Prompt for the forced terminal commit (benchmark mode).
fn forced_final_prompt() -> &'static str {
    "⏰ Turn budget exhausted. Do NOT call any tools. Based on everything you \
     have already gathered, output exactly one line: Final answer: <value>. \
     Nothing else."
}

// ── Tool Result Deduplication ──────────────────────────────────────────────

/// When N tool results in the same turn are near-identical (e.g. 3 sub-agents
/// all reporting the same path inconsistency bug), keep the first 2 and replace
/// the rest with a collapsed summary. This prevents flooding the LLM context
/// with duplicate observations that cause repetition loops in the thinking output.
fn deduplicate_tool_results(results: &mut [(String, String, Vec<everevo_core::ImageData>)]) {
    if results.len() < 3 {
        return;
    }

    // Phase 1: fingerprint each result
    let fingerprints: Vec<u64> = results
        .iter()
        .map(|(_, content, _)| {
            let prefix: String = content.chars().take(200).collect();
            hash_str(&prefix)
        })
        .collect();

    // Phase 2: find groups with high similarity
    let mut seen: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
    for (i, &fp) in fingerprints.iter().enumerate() {
        seen.entry(fp).or_default().push(i);
    }

    // Phase 3: collapse groups with >2 members
    for indices in seen.values() {
        if indices.len() <= 2 {
            continue;
        }
        let keep_id = results[indices[0]].0.clone();
        let dup_count = indices.len() - 2;
        for &idx in &indices[2..] {
            results[idx] = (
                results[idx].0.clone(),
                format!(
                    "(duplicate of {keep_id} — {dup_count} similar results collapsed to save context)"
                ),
                Vec::new(),
            );
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockLlmProvider;
    use everevo_core::llm::LlmProvider;
    use everevo_core::tool::{Tool, ToolOutput, ToolRegistry};
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
