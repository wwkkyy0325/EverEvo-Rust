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

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use everevo_core::llm::{LlmMessage, LlmRole, StreamEvent, ToolSchema};
use everevo_core::tool::ToolRegistry;
use everevo_core::types::ToolCall;
use everevo_core::EverEvoError;
use everevo_core::{AgentTurnRecord, Telemetry};
use uuid::Uuid;

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
If you try the same tool with only minor parameter changes, you are likely \
to fail again. Consider: is there a DIFFERENT tool, library, or strategy \
you can use instead?".into(),
            ),
            EscalationLevel::ResearchRequired => Some(
                "\
## [REQUIRED] Research Before Retrying\n\n\
You have attempted the same approach twice and both attempts failed. \
Before your next coding attempt you MUST:\n\
1. Call web_search for at least 2 relevant queries about this problem\n\
2. Read any promising results\n\
3. Formulate an approach that is FUNDAMENTALLY different from what you tried \
(different library, different algorithm, different architecture — NOT just \
different parameter values)\n\
4. Explain your new approach before executing it.".into(),
            ),
            EscalationLevel::ForcedDivergence => Some(
                "\
## [REQUIRED] Forced Divergence — Same Approach Failed 3+ Times\n\n\
You are stuck in a fixation loop. Complete ALL of these before retrying:\n\
- [ ] Re-read the LAST error message word-for-word — what EXACTLY failed?\n\
- [ ] web_search: the exact error message\n\
- [ ] web_search: \"{your task} alternative approach\" or \"{your task} library\"\n\
- [ ] List 3 DISTINCT hypotheses for why the current approach fails\n\
- [ ] Choose the most promising alternative and explain WHY it should work\n\
- [ ] Your next attempt MUST use a fundamentally different approach\n\n\
Parameter tweaks, retry loops, and \"let me try one more time\" are NOT \
acceptable at this stage. If you genuinely cannot find an alternative, \
spawn a sub-agent with fresh context to review the problem.".into(),
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
    /// Optional telemetry handle for recording agent turn metrics.
    telemetry: Option<Arc<Telemetry>>,
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
        }
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

    pub fn with_telemetry(mut self, telemetry: Arc<Telemetry>, trace_id: Uuid) -> Self {
        self.telemetry = Some(telemetry);
        self.trace_id = Some(trace_id);
        self
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
                &llm,
                &tools,
                &tool_schemas,
                &mut messages,
                max_turns,
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
            )
            .await
            {
                let _ = tx
                    .send(AgentEvent::Error {
                        message: e.to_string(),
                    })
                    .await;
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
        let tool_schemas: Vec<ToolSchema> = tools
            .as_tool_schemas()
            .into_iter()
            .map(|s| ToolSchema {
                name: s["function"]["name"].as_str().unwrap_or("").into(),
                description: s["function"]["description"].as_str().unwrap_or("").into(),
                parameters: s["function"]["parameters"].clone(),
            })
            .collect();

        let mut text = String::new();
        let mut messages = messages;

        for _turn in 0..max_turns {
            if cancel.is_cancelled() {
                return format!("{text}\n[Cancelled]");
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

            let mut rx = token_rx;
            while let Some(event) = rx.recv().await {
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
                    StreamEvent::Done => {
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
                }
            }

            // No tool calls → done
            if tool_calls.is_empty() {
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

// ── Loop Core ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
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
    cancel: Option<&tokio_util::sync::CancellationToken>,
    compact_focus: &Option<Arc<std::sync::Mutex<Option<String>>>>,
    proactivity: &Option<Arc<std::sync::Mutex<ProactivityState>>>,
) -> Result<(), EverEvoError> {
    let mut turn = 0;
    // Track the previous turn's tool signature for fixation detection.
    let mut prev_tool_sig: Option<(String, u64)> = None;

    while max_turns == 0 || turn < max_turns {
        turn += 1;

        // Drain pending subagent results (non-blocking)
        if let Some(ref mut rx) = subagent_rx {
            while let Ok(result) = rx.try_recv() {
                messages.push(LlmMessage::user(format!("[SubAgent Result]\n{result}")));
            }
        }
        let turn_start = Instant::now();

        // ── Context management (Claude Code-aligned multi-layer) ────
        // Layer 1: Snip — zero-cost pruning of low-value tool results
        trim::snip_low_value_messages(messages);
        // Layer 3+4: Autocompact (LLM summarization) → Trim (hard drop fallback)
        // Trigger when approximate token count exceeds (context limit - buffer)
        let token_usage = trim::approx_tokens(messages.iter().map(|m| m.content.len()).sum());
        let token_limit = max_context_chars / 4;
        if token_usage > token_limit.saturating_sub(trim::COMPACTION_BUFFER_TOKENS) {
            tracing::info!(token_usage, token_limit, "Context compaction triggered");
            // Read focus hint from CompactTool (if set), then clear it
            let focus = compact_focus.as_ref().and_then(|f| {
                let mut guard = f.lock().unwrap_or_else(|e| e.into_inner());
                guard.take()
            });
            if trim::autocompact(messages, max_context_chars, llm, focus.as_deref()).await == 0 {
                trim::trim_context(messages, max_context_chars);
            }
        }

        tracing::info!(turn, msg_count = messages.len(), "Agent turn start");

        // ── 1. Call LLM with context overflow recovery ─────────────
        // Claude Code error recovery waterfall:
        //   1. Force emergency compaction → retry
        //   2. Force aggressive trim → retry
        //   3. Give up → propagate error to user
        let token_rx = match llm.stream_chat(messages, tool_schemas, cancel.cloned()).await {
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
            }
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
        };
        messages.push(assistant_msg);

        // ── 3. Execute tools ────────────────────────────────────────
        let mut tool_result_pairs: Vec<(String, String)> = Vec::new();
        let mut tool_calls_success = 0i32;

        for tc in &tool_calls {
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
                        })
                        .await;
                    tool_result_pairs.push((tc.id.clone(), skip_msg));
                    continue;
                }
            }

            let result = match tool {
                Some(tool) => {
                    let result = execute_with_hooks(
                        tool.as_ref(),
                        &tc.name,
                        &tc.arguments,
                        None,
                        &tools.hooks,
                    )
                    .await;
                    // Report hook blocks via SSE
                    if let Err(ref e) = result {
                        if e.to_string().contains("blocked") {
                            let _ = tx
                                .send(AgentEvent::ToolCallEnd {
                                    id: tc.id.clone(),
                                    name: tc.name.clone(),
                                    content: format!("Tool blocked: {e}"),
                                    is_error: true,
                                })
                                .await;
                            tool_result_pairs.push((tc.id.clone(), format!("Tool blocked: {e}")));
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
                    let truncated = trim::truncate_output(&output.content, max_tool_result_chars);
                    let _ = tx
                        .send(AgentEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: truncated.clone(),
                            is_error: output.is_error,
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
                        tracing::warn!(tool = %tc.name, "Tool returned error");
                    } else {
                        tool_calls_success += 1;
                    }
                    tool_result_pairs.push((tc.id.clone(), truncated));
                }
                Err(e) => {
                    let err_msg = format!("Tool execution failed: {e}");
                    let _ = tx
                        .send(AgentEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: err_msg.clone(),
                            is_error: true,
                        })
                        .await;
                    tool_result_pairs.push((tc.id.clone(), err_msg));
                }
            }
        }

        // ── 4. Merge tool results into ONE user message ─────────────
        if !tool_result_pairs.is_empty() {
            if tool_result_pairs.len() == 1 {
                let (id, content) = tool_result_pairs.into_iter().next().unwrap_or_default();
                messages.push(LlmMessage::tool(&content, &id));
            } else {
                let ids: Vec<String> = tool_result_pairs.iter().map(|(id, _)| id.clone()).collect();
                let payload = serde_json::to_string(
                    &tool_result_pairs
                        .iter()
                        .map(|(id, content)| serde_json::json!({"i": id, "c": content}))
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default();
                let ids_joined = ids.join("|");
                let mut msg = LlmMessage::tool(&payload, &ids_joined);
                msg.tool_call_id = Some(ids_joined);
                messages.push(msg);
            }
        }

        // ── 4.5 Proactivity: detect fixation and inject intervention ─
        if let Some(ref state) = proactivity {
            // Collect first-tool info for this turn's fixation tracking.
            let this_tool = tool_calls.first().map(|tc| {
                (tc.name.clone(), hash_args(&tc.arguments))
            });
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

        // ── 5. Emit turn complete ───────────────────────────────────
        let _ = tx.send(AgentEvent::TurnComplete).await;

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
        if max_turns > 0 && turn >= max_turns - 2 && turn < max_turns {
            messages.push(LlmMessage::user(
                "You have only a few turns remaining. Please provide your final answer now.",
            ));
        }
    }

    if max_turns > 0 {
        let _ = tx
            .send(AgentEvent::Error {
                message: format!("Max turns ({max_turns}) reached. Please try a simpler request."),
            })
            .await;
    }

    Ok(())
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
