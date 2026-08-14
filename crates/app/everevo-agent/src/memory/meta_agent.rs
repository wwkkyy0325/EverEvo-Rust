//! Meta-Agent — cross-turn pattern diagnosis and improvement proposal.
//!
//! ## Role
//!
//! The Meta-Agent sits ABOVE the execution loop. It observes the trajectory
//! buffer, queries the symbol ontology and paradigm store, and produces
//! concrete hints that are injected into the LLM context on the next turn.
//!
//! ## Trigger conditions
//!
//! - **Interval**: every N turns (default: 5)
//! - **Degradation**: when ProactivityState escalation reaches L2 (ResearchRequired) or higher
//! - **Session start**: on the first turn, it checks for handoff from previous session
//!
//! ## Follows the same fire-and-forget pattern as `reflect_on_turn()`
//!
//! Failures are logged, never surfaced to the user. The Meta-Agent never
//! blocks the main loop — it is spawned as a background task.

use std::sync::Arc;

use everevo_core::llm::{LlmMessage, LlmProvider};
use everevo_core::memory::{FactType, MemoryFact, ProjectionMetadata};

use crate::llm::HttpClient;
use crate::memory::facts::FactManager;
use crate::memory::paradigm::TrajectoryBuffer;

// ── Meta-Agent State (wired into AgentRun) ────────────────────────────────

/// Tracks meta-agent triggering alongside proactivity.
///
/// Follows the same pattern as `ProactivityState` — lightweight state
/// updated synchronously each turn; background LLM calls spawned separately.
#[derive(Clone)]
pub struct MetaAgentState {
    /// Turns since last meta-agent invocation.
    pub turns_since_last_meta: u32,
    /// How many turns between periodic meta-agent triggers.
    pub trigger_interval: u32,
    /// Pending hint from the last meta-agent invocation (injected next turn).
    pub pending_hint: Option<String>,
    /// Shared LLM client for fire-and-forget background work.
    pub llm: Option<Arc<HttpClient>>,
    /// Shared fact manager for saving diagnoses.
    pub fact_manager: Option<Arc<FactManager>>,
}

impl MetaAgentState {
    /// Create a new meta-agent state. Pass `None` for llm/fact_manager to
    /// disable meta-agent functionality (opt-out pattern).
    pub fn new(llm: Option<Arc<HttpClient>>, fact_manager: Option<Arc<FactManager>>) -> Self {
        Self {
            turns_since_last_meta: 0,
            trigger_interval: 5,
            pending_hint: None,
            llm,
            fact_manager,
        }
    }

    /// Whether the meta-agent should be triggered this turn.
    pub fn should_trigger(&self, escalation_level: u32) -> bool {
        // Trigger on interval
        if self.turns_since_last_meta >= self.trigger_interval {
            return true;
        }
        // Trigger on degradation (escalation L2+)
        if escalation_level >= 2 {
            return true;
        }
        false
    }

    /// Whether the meta-agent has an LLM client to execute with.
    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    /// Take and clear the pending hint (for injection into context).
    pub fn take_hint(&mut self) -> Option<String> {
        self.pending_hint.take()
    }

    /// Store a hint for next-turn injection.
    pub fn set_hint(&mut self, hint: String) {
        self.pending_hint = Some(hint);
    }

    /// Reset the turn counter after a meta-agent invocation.
    pub fn mark_triggered(&mut self) {
        self.turns_since_last_meta = 0;
    }

    /// Increment the turn counter (called each turn).
    pub fn increment_turn(&mut self) {
        self.turns_since_last_meta += 1;
    }
}

impl std::fmt::Debug for MetaAgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetaAgentState")
            .field("turns_since_last_meta", &self.turns_since_last_meta)
            .field("trigger_interval", &self.trigger_interval)
            .field("pending_hint", &self.pending_hint)
            .field("llm", &self.llm.as_ref().map(|_| "HttpClient"))
            .field(
                "fact_manager",
                &self.fact_manager.as_ref().map(|_| "FactManager"),
            )
            .finish()
    }
}

impl Default for MetaAgentState {
    fn default() -> Self {
        Self::new(None, None)
    }
}

// ── Meta-Agent Diagnosis ───────────────────────────────────────────────────

/// Run the meta-agent diagnosis on recent trajectory data.
///
/// Fire-and-forget (same pattern as `reflect_on_turn`). Called as a
/// background task from `post_turn.rs` or directly from `AgentRun`.
///
/// Returns `None` if the meta-agent has nothing useful to say, or if
/// the LLM call fails (logged but not surfaced).
pub async fn meta_diagnose(
    llm: &HttpClient,
    fact_manager: Option<&FactManager>,
    trajectory: &TrajectoryBuffer,
    escalation_level: u32,
    recent_turns_summary: &str,
) -> Option<String> {
    let snap = trajectory.snapshot();
    if snap.len() < 2 && escalation_level < 2 {
        // Not enough data and no urgency — skip
        return None;
    }

    // Build diagnosis prompt
    let prompt = build_meta_diagnosis_prompt(&snap, escalation_level, recent_turns_summary);
    let messages = vec![LlmMessage::user(&prompt)];

    let response = match llm.chat(&messages, &[]).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "Meta-agent LLM call failed");
            return None;
        }
    };

    let text = response.content.unwrap_or_default();
    if text.is_empty() || text.contains("[NO_ACTION]") {
        return None;
    }

    // Extract the hint — take everything before the first blank line
    // that starts with "MEMORY:" or "SAVE:"
    let hint: String = text
        .lines()
        .take_while(|l| !l.starts_with("MEMORY:") && !l.starts_with("SAVE:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if hint.is_empty() {
        return None;
    }

    // Optionally save the diagnosis as a Feedback fact for future recall
    // Benchmark mode (EVEREVO_BENCHMARK=1) skips the save — the diagnosis is
    // written to the GLOBAL tier and would leak trajectory into later sessions.
    if let Some(fm) = fact_manager {
        if std::env::var("EVEREVO_BENCHMARK").is_ok() {
            return Some(hint);
        }
        let fact = MemoryFact {
            name: format!("meta-diag-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S")),
            description: truncate_str(&hint, 120),
            content: format!(
                "# Meta-Agent Diagnosis\n\n{hint}\n\n---\nEscalation level: {escalation_level}\nTrajectory size: {}",
                snap.len()
            ),
            fact_type: FactType::Feedback,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("meta-agent", "llm", vec![], 0.6),
            links: vec![],
            // Cross-session diagnosis — deliberately global long-term memory.
            session: Some("global".into()),
        };
        let _ = fm.save_async(fact).await;
    }

    Some(hint)
}

// ── Prompt Builder ─────────────────────────────────────────────────────────

fn build_meta_diagnosis_prompt(
    trajectory: &[crate::memory::paradigm::TurnDigest],
    escalation_level: u32,
    recent_summary: &str,
) -> String {
    let mut turns = String::new();
    for (i, t) in trajectory.iter().enumerate() {
        let status = if t.success { "✓" } else { "✗" };
        let err = t
            .error_type
            .as_ref()
            .map(|e| format!(" [{}]", e))
            .unwrap_or_default();
        turns.push_str(&format!(
            "  T{}: {} {}{err} | intent: {}\n",
            i + 1,
            t.tool_name,
            status,
            truncate_str(&t.user_intent, 80),
        ));
    }

    let escalation_note = if escalation_level >= 2 {
        format!(
            "\n⚠️ Escalation level {escalation_level} — the agent may be stuck. \
             Prioritize breaking the fixation loop.\n"
        )
    } else {
        String::new()
    };

    format!(
        "You are the Meta-Agent — an overseer that diagnoses patterns in an executing AI agent's \
         behavior. Your job is to produce a CONCISE, ACTIONABLE hint that will be injected into \
         the agent's context on the NEXT turn.\n\n\
         ## Recent Trajectory\n\
         {turns}\n\
         {escalation_note}\n\
         ## Context\n\
         {recent_summary}\n\n\
         ## Instructions\n\
         1. Identify the ROOT CAUSE of any failures — not just symptoms.\n\
         2. If the agent is retrying the same approach, suggest a FUNDAMENTALLY different strategy.\n\
         3. If the agent is stuck, suggest a specific next action (e.g., web_search the error).\n\
         4. Be CONCISE — your output goes directly into the agent's context window.\n\
         5. If the trajectory looks normal (just progressing through a task), return: [NO_ACTION]\n\n\
         ## Format\n\
         Write your hint directly (1-3 sentences, no preamble). Optionally add:\n\
         MEMORY: <fact to save for future sessions>\n\
         \n\
         Hint:"
    )
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::paradigm::TurnDigest;

    #[test]
    fn test_meta_agent_state_default_no_trigger() {
        let state = MetaAgentState::default();
        assert!(!state.should_trigger(0)); // not enough turns, no escalation
        assert!(!state.has_llm());
    }

    #[test]
    fn test_meta_agent_state_triggers_on_interval() {
        let llm = None; // no real LLM needed for state test
        let mut state = MetaAgentState::new(llm, None);
        // Manually set turns to trigger
        state.turns_since_last_meta = 5;
        assert!(state.should_trigger(0));
    }

    #[test]
    fn test_meta_agent_state_triggers_on_escalation() {
        let mut state = MetaAgentState::new(None, None);
        state.turns_since_last_meta = 1;
        assert!(state.should_trigger(2)); // L2 = ResearchRequired
        assert!(state.should_trigger(3)); // L3 = ForcedDivergence
    }

    #[test]
    fn test_meta_agent_state_hint_cycle() {
        let mut state = MetaAgentState::new(None, None);
        assert!(state.take_hint().is_none());
        state.set_hint("Try HTTPS instead of SSH".into());
        assert_eq!(state.take_hint().unwrap(), "Try HTTPS instead of SSH");
        assert!(state.take_hint().is_none()); // cleared
    }

    #[test]
    fn test_meta_agent_state_mark_triggered_resets_counter() {
        let mut state = MetaAgentState::new(None, None);
        state.turns_since_last_meta = 7;
        state.mark_triggered();
        assert_eq!(state.turns_since_last_meta, 0);
    }

    #[test]
    fn test_meta_agent_state_increment() {
        let mut state = MetaAgentState::new(None, None);
        assert_eq!(state.turns_since_last_meta, 0);
        state.increment_turn();
        state.increment_turn();
        assert_eq!(state.turns_since_last_meta, 2);
    }

    #[test]
    fn test_diagnosis_prompt_includes_escalation() {
        let digests = vec![TurnDigest::new("shell", false, Some("err"), "test", "fail")];
        let prompt = build_meta_diagnosis_prompt(&digests, 3, "user asked to deploy");
        assert!(prompt.contains("Escalation level 3"));
        assert!(prompt.contains("fixation"));
    }

    #[test]
    fn test_diagnosis_prompt_no_escalation_for_normal() {
        let digests = vec![TurnDigest::new("shell", true, None, "ok", "done")];
        let prompt = build_meta_diagnosis_prompt(&digests, 0, "simple task");
        assert!(!prompt.contains("Escalation level"));
    }
}
