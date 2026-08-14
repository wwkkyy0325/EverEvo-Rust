//! ReAct agent loop — the core execution cycle.
//!
//! Single-threaded while-loop, inspired by Claude Code's `nO` master loop.
//! Intentionally simple: flat, debuggable, reliable.
//!
//! Module map (physical restructure 2026-08-13 — split the 1126-line mod.rs):
//! - `agent` — `AgentRun` struct + builders + run()/run_to_string() (moved here)
//! - `driver` — the shared `run_loop` engine (FSM T1-T26)
//! - `state`  — FSM states/events/transitions
//! - `tests`  — loop tests (moved here)

pub mod event;
pub mod hooks;
pub mod proactivity;
pub mod trim;

mod agent;
mod classify;
mod config;
mod convergence;
mod dedup;
mod driver;
mod final_answer;
mod llm_call;
mod meta_orchestrator;
mod retrospective;
mod state;
mod token_stream;

#[cfg(test)]
mod tests;

pub use agent::{AgentRun, AgentRunMode};

#[allow(unused_imports)]
pub(crate) use convergence::{budget_line, convergence_stage, forced_final_prompt, Convergence};
// The MetaOrchestrator policy API — exported as a coherent unit; MO-2/3 wire
// it into RunConfig + the driver, the server constructs MetaOrchestratorState.
pub use event::AgentEvent;
pub use meta_orchestrator::{
    decide_fan_out, deepdive_directive, drive_phase, extract_candidate, parse_subtask_count,
    phase_stage, scout_directive, subagent_phase_directive, verify_directive,
    verify_directive_without_candidate, Decomposability, FanOutDecision, MetaOrchestratorState,
    Phase, DEEPDIVE_END_FRAC, FANOUT_COST_SLACK, MAX_FANOUT_WORKERS, SCOUT_END_FRAC,
    VERIFY_END_FRAC, WORKER_SECS_DEFAULT,
};
#[allow(unused_imports)]
pub(crate) use proactivity::{hash_args, hash_str};
pub use proactivity::{EscalationLevel, ProactivityState};
