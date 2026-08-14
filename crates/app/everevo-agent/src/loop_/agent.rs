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

use super::config::RunConfig;
use super::driver::run_loop;
use super::event::AgentEvent;
use super::proactivity::ProactivityState;

use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use everevo_core::llm::{LlmMessage, LlmProvider, ToolSchema};
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

/// How an [`AgentRun`] is being used — selects the per-run policy for the
/// hard-question verification gate and the benchmark wall-clock deadline.
///
/// `verify_gate` and wall-clock are *not* individually settable; the mode is
/// the single owner (unified-entry refactor, P0):
/// - `Session` — main session + auto-continue: verification gate ON,
///   benchmark wall-clock derived from env.
/// - `SubAgent` — sub-agent/team/cluster/workflow runs: verification gate OFF
///   (the main loop verifies the final synthesis), no wall-clock.
/// - `Cli` — standalone binary chat: gate ON, wall-clock from env.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentRunMode {
    Session,
    SubAgent,
    Cli,
}

/// The ReAct agent loop — LLM → Tools → Results → LLM cycle.
///
/// Consuming builder (`mut self -> Self`), three mode constructors, one
/// mode-driven run pair. Unified entry for the 11 construction sites
/// (architecture-restructure-plan.md P0).
pub struct AgentRun {
    /// Run mode — drives verification-gate + wall-clock policy.
    mode: AgentRunMode,
    /// Maximum number of ReAct turns before forced termination.
    /// `pub(crate)` — read directly by the loop tests (2026-08-13 mod.rs split).
    pub(crate) max_turns: usize,
    /// Maximum characters per tool result before truncation. 0 = no limit.
    pub(crate) max_tool_result_chars: usize,
    /// Approximate max total characters in the message history before trimming.
    pub(crate) max_context_chars: usize,
    /// Optional telemetry pipeline for recording agent turn metrics.
    telemetry: Option<Arc<TelemetryPipeline>>,
    /// Trace ID for correlating telemetry records.
    trace_id: Option<Uuid>,
    /// Pending subagent results channel — TaskTool pushes, AgentRun drains.
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
    /// LLM-free meta-orchestrator policy state (Scout/DeepDive/Verify/Commit).
    /// None → the driver runs exactly as today (opt-in, benchmark).
    orchestrator_state: Option<Arc<std::sync::Mutex<crate::loop_::MetaOrchestratorState>>>,
    /// Current TodoWrite task list (server-rendered) — lets the driver's
    /// evidence gate reference task progress. None → no todo awareness.
    todo_summary: Option<String>,
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

impl AgentRun {
    pub fn new() -> Self {
        Self {
            mode: AgentRunMode::Session,
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
            orchestrator_state: None,
            todo_summary: None,
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

    /// Enable the LLM-free meta-orchestrator policy layer
    /// (Scout/DeepDive/Verify/Commit). Opt-in: when set, the driver fires
    /// phase directives; when `None` (default) the loop is byte-equivalent to
    /// today.
    pub fn with_meta_orchestrator(
        mut self,
        state: Arc<std::sync::Mutex<crate::loop_::MetaOrchestratorState>>,
    ) -> Self {
        self.orchestrator_state = Some(state);
        self
    }

    /// Provide the current TodoWrite task list (server-rendered). The driver's
    /// evidence gate references it when a hard question claims completed work
    /// but retrieved no tool evidence. None → no todo awareness.
    pub fn with_todo_summary(mut self, summary: Option<String>) -> Self {
        self.todo_summary = summary;
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

    /// Set the context ceiling from a token budget (4 chars/token, matching
    /// driver.rs `max_context_chars / 4`). Single home for the tokens→chars
    /// conversion that previously repeated at 4 call sites.
    #[must_use]
    pub fn with_context_tokens(mut self, tokens: usize) -> Self {
        self.max_context_chars = tokens.saturating_mul(4);
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
    #[must_use]
    pub fn main_session(
        subagent_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
        pending_subagents: Arc<std::sync::atomic::AtomicUsize>,
        cancel_token: tokio_util::sync::CancellationToken,
        compact_focus: Arc<std::sync::Mutex<Option<String>>>,
        proactivity: Arc<std::sync::Mutex<ProactivityState>>,
    ) -> Self {
        let mut s = Self::new();
        *s.subagent_rx.lock().unwrap_or_else(|e| e.into_inner()) = Some(subagent_rx);
        s.pending_subagents = pending_subagents;
        s.cancel_token = Some(cancel_token);
        s.compact_focus = Some(compact_focus);
        s.proactivity_state = Some(proactivity);
        s
    }

    /// Standard sub-agent loop (TaskTool, Team, Workflow, Cluster, A2A).
    /// Sets a turn limit; the caller can chain additional `.with_*()` methods.
    #[must_use]
    pub fn sub_agent(max_turns: usize) -> Self {
        let mut s = Self::new().with_max_turns(max_turns);
        s.mode = AgentRunMode::SubAgent;
        s
    }

    /// CLI chat loop (standalone binary mode). Unbounded by default — the agent
    /// runs until it completes (or the caller stops it); benchmark/eval runs
    /// impose their own official cap (e.g. Terminal-Bench `agent.timeout_sec`),
    /// and `EVEREVO_MAX_TURNS` optionally bounds it.
    #[must_use]
    pub fn cli() -> Self {
        let max_turns = std::env::var("EVEREVO_MAX_TURNS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(0);
        let mut s = Self::new().with_max_turns(max_turns);
        s.mode = AgentRunMode::Cli;
        s
    }

    /// Per-run policy for the verification gate and benchmark wall-clock,
    /// driven solely by the run mode.
    fn mode_gate(&self) -> (bool, Option<std::time::Instant>) {
        match self.mode {
            AgentRunMode::SubAgent => (false, None),
            AgentRunMode::Session | AgentRunMode::Cli => (true, derive_benchmark_wallclock()),
        }
    }

    /// Run the ReAct loop with streaming output via AgentEvent channel.
    #[allow(clippy::type_complexity)]
    pub async fn run(
        &self,
        llm: Arc<dyn LlmProvider>,
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
        let orchestrator = self.orchestrator_state.clone();
        let todo_summary = self.todo_summary.clone();
        let hook_feedback_slot = self.hook_feedback_slot.clone();
        let background = self.background_maintenance.clone();
        let compact_llm = self.compact_llm.clone();
        let tool_cache_dir = self.tool_cache_dir.clone();
        // Run-mode policy computed outside the spawned task (it borrows self);
        // the values are moved into the 'static task below.
        let (verify_gate, wall_deadline) = self.mode_gate();
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

            // Run-mode policy (verify_gate + wall_deadline) was computed
            // before spawning and moved into this task.

            // Bundle the per-run wiring once (unified-entry refactor, P0): the
            // loop engine reads everything from RunConfig instead of ~20
            // positional params re-assembled at every entry point.
            let config = RunConfig {
                max_turns,
                wall_clock_deadline: wall_deadline,
                max_tool_result_chars,
                max_context_chars,
                confirmation,
                telemetry,
                trace_id,
                subagent_rx,
                pending_subagents,
                compact_focus,
                proactivity,
                meta_agent_state: meta_agent,
                orchestrator,
                todo_summary,
                hook_feedback_slot,
                compact_llm,
                background,
                tool_cache_dir,
                cancel,
                verify_gate,
            };
            match AssertUnwindSafe(run_loop(
                llm.as_ref(),
                &tools,
                &tool_schemas,
                &mut messages,
                config,
                &tx,
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
    /// Blocking (non-streaming) — `verify_gate` is driven by the mode.
    ///
    /// Returns the accumulated text from all turns, or an error string.
    pub async fn run_to_string(
        &self,
        llm: Arc<dyn LlmProvider>,
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
        if let Some(native) = llm.native_web_search_tool() {
            tool_schemas.retain(|t| t.name != "web_search");
            tool_schemas.push(native);
        }

        // A sub-agent run reuses the SAME loop engine as the main session
        // (unified entry, architecture-restructure-plan.md P0): run_loop emits
        // AgentEvents into a channel; we collect the streamed text deltas and
        // the final value. Nested TaskTool sub-agents are BLOCKING
        // (delegate/spawn.rs), so a fresh pending counter keeps the loop from
        // yielding on the parent's in-flight sub-agents, and subagent_rx stays
        // None (no async results to drain in a sub-agent run).
        let (verify_gate, _) = self.mode_gate();
        let config = RunConfig {
            max_turns,
            max_tool_result_chars: self.max_tool_result_chars,
            max_context_chars: self.max_context_chars,
            pending_subagents: self.pending_subagents.clone(),
            cancel: Some(cancel.clone()),
            // A sub-agent returns its best answer; the main loop verifies the
            // final synthesis — skip the hard-question verification gate
            // (mode_gate yields false for AgentRunMode::SubAgent).
            verify_gate,
            ..RunConfig::new()
        };
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(32);
        let mut messages = messages;
        let run_result = run_loop(&*llm, &tools, &tool_schemas, &mut messages, config, &tx).await;
        drop(tx);

        match run_result {
            Ok(()) => {
                let mut accumulated = String::new();
                let mut done_value = String::new();
                while let Some(ev) = rx.recv().await {
                    match ev {
                        AgentEvent::TextDelta(t) => accumulated.push_str(&t),
                        AgentEvent::Done { final_text: ft } => done_value = ft,
                        AgentEvent::Error { message } => return format!("Error: {message}"),
                        _ => {}
                    }
                }
                let result = if accumulated.trim().is_empty() {
                    done_value
                } else {
                    accumulated
                };
                if cancel.is_cancelled() {
                    return format!(
                        "{result}
[Cancelled]"
                    );
                }
                result
            }
            Err(e) => format!("Error: {e}"),
        }
    }
}

impl Default for AgentRun {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive the benchmark wall-clock deadline from env, or `None` outside
/// benchmark mode. `EVEREVO_BENCHMARK=1` + `EVEREVO_BENCHMARK_WALLCLOCK`
/// (seconds, default 300). A full GAIA run sets this ~30s UNDER the harness
/// question_timeout so the forced terminal commit lands before the harness
/// kills the request. Non-benchmark runs keep byte-identical behavior (None).
fn derive_benchmark_wallclock() -> Option<std::time::Instant> {
    if std::env::var("EVEREVO_BENCHMARK").is_ok() {
        let secs: u64 = std::env::var("EVEREVO_BENCHMARK_WALLCLOCK")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(300);
        Some(std::time::Instant::now() + std::time::Duration::from_secs(secs))
    } else {
        None
    }
}
