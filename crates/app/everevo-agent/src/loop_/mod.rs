//! ReAct agent loop — the core execution cycle.
//!
//! Single-threaded while-loop, inspired by Claude Code's `nO` master loop.
//! Intentionally simple: flat, debuggable, reliable.
//!
//! Module map (physical restructure 2026-08-13 — split the 1126-line mod.rs):
//! - `agent` — `AgentLoop` struct + builders + run()/run_subagent() (moved here)
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
mod llm_call;
mod retrospective;
mod state;
mod token_stream;

#[cfg(test)]
mod tests;

pub use agent::AgentLoop;
#[allow(unused_imports)]
pub(crate) use convergence::{budget_line, convergence_stage, forced_final_prompt, Convergence};
pub use event::AgentEvent;
#[allow(unused_imports)]
pub(crate) use proactivity::{hash_args, hash_str};
pub use proactivity::{EscalationLevel, ProactivityState};
