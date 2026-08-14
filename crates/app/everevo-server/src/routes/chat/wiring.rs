//! Shared session→agent wiring applied to every AgentRun construction.
//! Extracted from handler.rs during the 2026-08-13 physical restructure.

#[allow(clippy::too_many_arguments)] // shared session wiring bundle; mirrors driver.rs
pub(crate) fn apply_session_agent_wiring(
    mut agent: everevo_agent::AgentRun,
    proactivity: &std::sync::Arc<std::sync::Mutex<everevo_agent::ProactivityState>>,
    meta_agent: &Option<std::sync::Arc<std::sync::Mutex<everevo_agent::memory::MetaAgentState>>>,
    orchestrator: &Option<
        std::sync::Arc<std::sync::Mutex<everevo_agent::loop_::MetaOrchestratorState>>,
    >,
    hook_feedback: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
    context_window_tokens: usize,
    trace_id: Option<uuid::Uuid>,
    telemetry: std::sync::Arc<everevo_core::TelemetryPipeline>,
) -> everevo_agent::AgentRun {
    agent = agent
        .with_proactivity(std::sync::Arc::clone(proactivity))
        // Drive the agent loop's own context ceiling from the per-model budget
        // (4 chars/token — `with_context_tokens` is the single conversion site).
        .with_context_tokens(context_window_tokens)
        .with_hook_feedback(std::sync::Arc::clone(hook_feedback));
    if let Some(ma) = meta_agent {
        agent = agent.with_meta_agent(std::sync::Arc::clone(ma));
    }
    if let Some(orch) = orchestrator {
        agent = agent.with_meta_orchestrator(std::sync::Arc::clone(orch));
    }
    if let Some(tid) = trace_id {
        agent = agent.with_telemetry(telemetry, tid);
    }
    // Benchmark mode: cap the ReAct loop so a question that cannot be verified
    // (blocked web / junk search results from the host network) is still forced
    // to commit to a final answer instead of churning tools until the
    // wall-clock cap. Tunable via EVEREVO_MAX_TURNS — a full GAIA run with the
    // 1800s per-question cap sets this ~60-80 so the model has room to converge
    // (default stays 20).
    if std::env::var("EVEREVO_BENCHMARK").is_ok() {
        let max_turns: usize = std::env::var("EVEREVO_MAX_TURNS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(20);
        agent = agent.with_max_turns(max_turns);
    }
    agent
}
