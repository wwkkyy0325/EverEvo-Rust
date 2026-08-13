//! Per-run configuration bundle for the agent loop.
//!
//! Previously `run_loop` took ~20 positional parameters, and every entry point
//! (main session, auto-continue, CLI, sub-agents, team, cluster, workflow,
//! web-search delegate, A2A) re-assembled them by hand with inconsistent
//! choices. `RunConfig` bundles them into one struct so a run is configured
//! once and `run_loop` gets a single owned config — the foundation for the
//! unified `AgentRun` entry (architecture-restructure-plan.md P0).

use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use everevo_core::llm::LlmProvider;
use everevo_core::TelemetryPipeline;

use super::proactivity::ProactivityState;
use crate::context::BackgroundMaintenance;
use crate::memory::meta_agent::MetaAgentState;

/// Confirmation gate: called before executing a tool; return true to allow.
pub(crate) type ConfirmFn = dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync;

/// Everything `run_loop` needs beyond the immutable (llm/tools/schemas/messages)
/// and the event sink (`tx`). All fields are owned so a config can be moved
/// into a spawned task; `run_loop` destructures it into locals at the top.
pub(crate) struct RunConfig {
    pub max_turns: usize,
    pub wall_clock_deadline: Option<std::time::Instant>,
    pub max_tool_result_chars: usize,
    pub max_context_chars: usize,
    pub confirmation: Option<Arc<ConfirmFn>>,
    pub telemetry: Option<Arc<TelemetryPipeline>>,
    pub trace_id: Option<Uuid>,
    pub subagent_rx: Option<mpsc::UnboundedReceiver<String>>,
    pub pending_subagents: Arc<AtomicUsize>,
    pub compact_focus: Option<Arc<Mutex<Option<String>>>>,
    pub proactivity: Option<Arc<Mutex<ProactivityState>>>,
    pub meta_agent_state: Option<Arc<Mutex<MetaAgentState>>>,
    pub hook_feedback_slot: Option<Arc<Mutex<Option<String>>>>,
    pub compact_llm: Option<Arc<dyn LlmProvider>>,
    pub background: Option<Arc<BackgroundMaintenance>>,
    pub tool_cache_dir: Option<PathBuf>,
    pub cancel: Option<CancellationToken>,
    /// Whether the hard-question verification commit gate (T8) applies. The
    /// main session enforces it (a hard commit must have run a verification
    /// step); sub-agent runs disable it — a sub-agent returns its best answer
    /// and the MAIN loop verifies the final synthesis.
    pub verify_gate: bool,
}

impl RunConfig {
    /// Conservative defaults: bounded but generous context/tool-result caps,
    /// no wall-clock deadline (non-benchmark), no extras. Callers override the
    /// fields they need (the server session wires telemetry/proactivity/…).
    pub(crate) fn new() -> Self {
        RunConfig {
            max_turns: 0,
            wall_clock_deadline: None,
            max_tool_result_chars: 4000,
            max_context_chars: 80_000,
            confirmation: None,
            telemetry: None,
            trace_id: None,
            subagent_rx: None,
            pending_subagents: Arc::new(AtomicUsize::new(0)),
            compact_focus: None,
            proactivity: None,
            meta_agent_state: None,
            hook_feedback_slot: None,
            compact_llm: None,
            background: None,
            tool_cache_dir: None,
            cancel: None,
            verify_gate: true,
        }
    }
}
