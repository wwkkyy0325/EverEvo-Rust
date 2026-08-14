//! MetaOrchestrator — an LLM-free policy layer ABOVE the ReAct loop.
//!
//! Turns the agent's wall-clock/turn budget into a coarse phase machine
//! (Scout → DeepDive → Verify → Commit), decides whether to fan out sub-agents
//! (a deterministic 5-gate governor), and emits phase-entry directives that
//! are appended to the conversation as user messages — the agent executes them
//! through its EXISTING tools. The orchestrator itself never calls the LLM and
//! never executes tools (SupervisorAgent, arXiv:2510.26585: an LLM-free
//! supervisor filter saves 29.68% tokens on GAIA with no accuracy loss).
//!
//! The phase thresholds are chosen to be CONSISTENT with the existing
//! benchmark convergence nudges ([`crate::loop_::convergence`]) so the
//! orchestrator EXTENDS — never replaces — the FSM:
//! - `Commit` fires exactly where `Convergence::Commit` fires (15% wall / 85% turns)
//! - `Verify` fires exactly where `Convergence::Converge` fires (30% wall / 70% turns)
//! - the early "None" window is refined into `Scout` (wall > 60%) vs `DeepDive`
//!   (wall 30-60%), with `verified` or `Simple` questions jumping straight to
//!   `Verify`.
//!
//! Fan-out is gated on decomposability (arXiv:2512.08296: parallel +80.9% on
//! independent tasks, but multi-agent chains LOSE 39-70% on sequential ones)
//! and budget (Snell, arXiv:2408.03314: difficulty-adaptive allocation beats
//! fixed strategies). The asymmetric Verify directive implements
//! independence-by-withholding (MARCH, arXiv:2603.24579: the checker sees
//! evidence but NOT the solver's draft).

use everevo_core::llm::{LlmMessage, LlmRole};

use crate::stages::Difficulty;

// ── Phase machine ───────────────────────────────────────────────────────────

/// Coarse execution phase for the current turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Early window: decompose the question, decide parallel vs serial.
    Scout,
    /// Mid window: drive the most promising thread; the ONLY phase where
    /// parallel sub-agent fan-out is permitted.
    DeepDive,
    /// A verified candidate exists (or the question is Simple): run the
    /// asymmetric adversarial check, then commit.
    Verify,
    /// Budget nearly exhausted: force the commit (mirrors `Convergence::Commit`).
    Commit,
}

/// Wall-clock fraction where Scout gives way to DeepDive.
pub const SCOUT_END_FRAC: f64 = 0.60;
/// Wall-clock fraction where DeepDive gives way to Verify (== `Convergence::Converge`).
pub const DEEPDIVE_END_FRAC: f64 = 0.30;
/// Wall-clock fraction where Verify gives way to Commit (== `Convergence::Commit`).
pub const VERIFY_END_FRAC: f64 = 0.15;

/// Turn the current budget into a phase.
///
/// Consistency with [`convergence_stage`](crate::loop_::convergence::convergence_stage):
/// - `wall_left_frac <= 0.15` or `turn_pct >= 0.85` → [`Phase::Commit`]
/// - `wall_left_frac <= 0.30` or `turn_pct >= 0.70` → [`Phase::Verify`]
/// - otherwise (the "None" window): `verified` or `Simple` → [`Phase::Verify`],
///   `wall_left_frac <= 0.60` → [`Phase::DeepDive`], else [`Phase::Scout`].
pub fn phase_stage(
    turn: usize,
    max_turns: usize,
    wall_left_frac: f64,
    difficulty: Difficulty,
    verified: bool,
) -> Phase {
    let turn_pct = if max_turns > 0 {
        turn as f64 / max_turns as f64
    } else {
        0.0
    };
    let commit = (max_turns > 0 && turn_pct >= 0.85) || wall_left_frac <= VERIFY_END_FRAC;
    let verify = (max_turns > 0 && turn_pct >= 0.70) || wall_left_frac <= DEEPDIVE_END_FRAC;
    if commit {
        Phase::Commit
    } else if verify || verified || difficulty == Difficulty::Simple {
        // `verify` mirrors Convergence::Converge; `verified` / `Simple` jump to
        // the verify window early (verified → asymmetric check; Simple → commit fast).
        Phase::Verify
    } else if wall_left_frac <= SCOUT_END_FRAC {
        Phase::DeepDive
    } else {
        Phase::Scout
    }
}

// ── Fan-out governor ────────────────────────────────────────────────────────

/// How many sub-agents may run in parallel at most (arXiv:2512.08296: a
/// handful of parallel workers captures the gains; overspawn dilutes value).
pub const MAX_FANOUT_WORKERS: usize = 6;
/// Fan out only if the parallel cost fits within this fraction of the
/// remaining wall-clock (leaves the main loop room to synthesize).
pub const FANOUT_COST_SLACK: f64 = 0.5;
/// Default per-worker seconds budget used by the cost gate.
pub const WORKER_SECS_DEFAULT: u64 = 90;

/// Whether the question decomposes into parallel-izable sub-parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decomposability {
    /// Not yet established (no `SUBTASKS: n` declaration seen).
    Unknown,
    /// Sub-parts are dependent (each builds on the previous) — do NOT fan out.
    Sequential,
    /// Independent sub-parts that can run concurrently.
    Independent(usize),
}

/// Decision of the fan-out governor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanOutDecision {
    /// Do not dispatch sub-agents this turn.
    NoFanOut,
    /// Dispatch `n` parallel sub-agents.
    FanOut(usize),
}

/// Parse an agent's `SUBTASKS: <n>` decomposition declaration (case-insensitive).
///
/// Returns `None` for anything that is not a `n >= 1` integer declaration.
pub fn parse_subtask_count(text: &str) -> Option<usize> {
    for line in text.to_lowercase().lines() {
        let line = line.trim();
        if let Some(idx) = line.find("subtasks:") {
            let rest = line[idx + "subtasks:".len()..].trim();
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<usize>() {
                if n >= 1 {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// Decide whether to dispatch parallel sub-agents this turn.
///
/// Five deterministic gates, each backed by research:
/// ① `Simple` questions never fan out.
/// ② Fan-out is a `DeepDive` tactic only.
/// ③ Only `Independent(n >= 2)` sub-parts parallelize; `Sequential`/`Unknown` don't.
/// ④ Anti-overspawn: at most once per run.
/// ⑤ Cost: `w = min(n, MAX_FANOUT_WORKERS)`, halved until `w * per_worker <=
///    remaining * FANOUT_COST_SLACK`; `w < 2` → no fan-out.
pub fn decide_fan_out(
    phase: Phase,
    difficulty: Difficulty,
    decomposability: Decomposability,
    remaining_secs: u64,
    per_worker_secs: u64,
    already_fanned_out: bool,
) -> FanOutDecision {
    if difficulty == Difficulty::Simple {
        return FanOutDecision::NoFanOut;
    }
    if phase != Phase::DeepDive {
        return FanOutDecision::NoFanOut;
    }
    let n = match decomposability {
        Decomposability::Independent(n) if n >= 2 => n,
        _ => return FanOutDecision::NoFanOut,
    };
    if already_fanned_out {
        return FanOutDecision::NoFanOut;
    }
    let per = if per_worker_secs > 0 {
        per_worker_secs
    } else {
        WORKER_SECS_DEFAULT
    };
    let mut w = n.min(MAX_FANOUT_WORKERS);
    while w > 1 && (w as u64 * per) as f64 > remaining_secs as f64 * FANOUT_COST_SLACK {
        w /= 2;
    }
    if w < 2 {
        FanOutDecision::NoFanOut
    } else {
        FanOutDecision::FanOut(w)
    }
}

// ── Cross-turn state (Arc<Mutex<>> in RunConfig, mirrors MetaAgentState) ────

/// Orchestrator state advanced by the driver each turn. Pure data — the
/// orchestrator itself never calls the LLM or executes tools.
#[derive(Clone, Debug)]
pub struct MetaOrchestratorState {
    pub phase: Phase,
    pub last_phase: Phase,
    /// Candidate the last verify directive was built from. A change in the
    /// candidate re-triggers the asymmetric check; the same one is never
    /// re-sent (avoids nagging the agent with an identical directive).
    pub last_candidate: Option<String>,
    /// True once a fan-out was issued — the governor only fires once.
    pub already_fanned_out: bool,
}

impl Default for MetaOrchestratorState {
    fn default() -> Self {
        Self::new()
    }
}

impl MetaOrchestratorState {
    pub fn new() -> Self {
        Self {
            phase: Phase::Scout,
            last_phase: Phase::Scout,
            last_candidate: None,
            already_fanned_out: false,
        }
    }

    /// Record that a fan-out was issued (anti-overspawn latch).
    pub fn mark_fanned_out(&mut self) {
        self.already_fanned_out = true;
    }
}

// ── Candidate extraction ────────────────────────────────────────────────────

/// Extract the latest committed `Final answer:` value from ASSISTANT messages.
///
/// Tool results (role `Tool`) are skipped — a "Final answer:" text inside a
/// tool result is data, not a commit. Within the last assistant message the
/// LAST `Final answer:` wins (it is the one the model is about to send).
pub fn extract_candidate(messages: &[LlmMessage]) -> Option<String> {
    for m in messages.iter().rev() {
        if m.role != LlmRole::Assistant {
            continue;
        }
        if let Some(idx) = m.content.rfind("Final answer:") {
            let val = m.content[idx + "Final answer:".len()..].trim();
            let val = val.trim_matches('"').trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

// ── Directive builders ──────────────────────────────────────────────────────

/// Scout-phase directive: decompose and DECLARE the fan-out plan. The actual
/// dispatch happens only in the DeepDive window (governor gate ②).
pub fn scout_directive() -> &'static str {
    "## Orchestrator: Scout phase\n\
     Decompose the question into independent sub-parts. If there are >= 2 \
     genuinely independent parts, emit a line `SUBTASKS: <n>` (your \
     decomposition plan) — you will dispatch them to sub-agents in parallel \
     during the DeepDive window. If the parts are SEQUENTIAL (each depends on \
     the previous), do NOT parallelize: keep focused serial research — parallel \
     chains on dependent tasks lose accuracy. If a single path suffices, \
     proceed directly."
}

/// DeepDive-phase directive: drive the chosen thread; dispatch the declared
/// independent sub-parts to sub-agents now.
pub fn deepdive_directive() -> &'static str {
    "## Orchestrator: DeepDive phase\n\
     You are past the scout window. STOP opening new research threads. If your \
     decomposition declared `SUBTASKS: <n>`, NOW is the time to dispatch those \
     independent sub-parts to sub-agents in parallel (up to 6) and synthesize \
     their results. Drive the single most promising thread to a verifiable \
     result. Do not re-open scout threads."
}

/// Verify-phase directive: ASYMMETRIC adversarial check (MARCH
/// independence-by-withholding) with the SGV two-step prior (arXiv
/// Self-Grounded Verification: the reviewer states an evidence baseline
/// INDEPENDENT of the candidate, then evaluates against it). The candidate is
/// embedded so the directive only changes when the candidate changes.
pub fn verify_directive(candidate: &str) -> String {
    format!(
        "## Orchestrator: Verify phase (asymmetric check)\n\
         Run an ASYMMETRIC adversarial check on the candidate before committing:\n\
         1. `cluster verify` with `claims` = [\"Final answer: {candidate}\"] and \
         `asymmetric` = true.\n\
         2. Pass ONLY the candidate and POINTERS to your evidence (file paths, \
         URLs, exact source lines). Do NOT paste your own derivation or \
         reasoning — the reviewer must judge from the evidence alone \
         (independence-by-withholding).\n\
         3. SGV two-step: each reviewer FIRST states the evidence baseline a \
         correct answer must satisfy (its required value/unit/items, derived \
         WITHOUT looking at the candidate or your draft), THEN evaluates the \
         candidate against that baseline — never by re-deriving your own \
         formula, which re-embeds the same mistake.\n\
         4. If the reviewer refutes it, re-derive from the raw source and \
         re-verify once, then commit your best survivor on `Final answer:`.\n\
         A self-check that restates your own draft proves nothing."
    )
}

/// Verify-phase directive for the rare case where no candidate is committed yet.
pub fn verify_directive_without_candidate() -> &'static str {
    "## Orchestrator: Verify phase\n\
     A candidate is expected. Run `cluster verify` on the candidate with ONLY \
     the claim and evidence pointers — do not paste your derivation. Then \
     commit your best survivor on `Final answer:`."
}

/// Directive text for a sub-agent dispatched while the parent loop is in
/// `phase`. Injected into the sub-agent's system prompt so a fan-out worker
/// knows the parent's tactical context (e.g. DeepDive → focused fan-out worker;
/// Verify → adversarial reviewer judging from evidence alone).
pub fn subagent_phase_directive(phase: Phase) -> String {
    match phase {
        Phase::Scout => "The parent agent is in the SCOUT phase: it is decomposing the question. \
             Return focused findings; the parent will decide whether to fan out."
            .into(),
        Phase::DeepDive => "The parent agent is in the DEEPDIVE phase: you are likely part of a \
             parallel fan-out. Be focused and return a single verifiable result with \
             its evidence."
            .into(),
        Phase::Verify => "The parent agent is VERIFYING a candidate. Your task is adversarial \
             review: judge from EVIDENCE alone, do not reconstruct the parent's \
             derivation — it may embed the same mistake."
            .into(),
        Phase::Commit => "The parent agent is COMMITTING. Return your single best result \
             immediately."
            .into(),
    }
}

// ── Phase driver ────────────────────────────────────────────────────────────

/// Advance the phase machine for the current turn and return the directives to
/// append to the conversation as user messages.
///
/// - On a phase TRANSITION, at most ONE directive is emitted.
/// - Within `Verify`, the asymmetric check is re-emitted ONLY when the
///   candidate changed (same candidate → silent; the driver's own
///   spiral/commit nudges own the repeated pressure).
/// - `Commit` emits nothing: the driver's `Convergence::Commit`/T25 prompt owns
///   the final push, so the orchestrator does not double-message.
pub fn drive_phase(
    state: &mut MetaOrchestratorState,
    turn: usize,
    max_turns: usize,
    wall_left_frac: f64,
    difficulty: Difficulty,
    verified: bool,
    messages: &[LlmMessage],
) -> Vec<String> {
    let next = phase_stage(turn, max_turns, wall_left_frac, difficulty, verified);
    if next == state.phase {
        if next == Phase::Verify {
            let candidate = extract_candidate(messages);
            if let Some(c) = candidate {
                if Some(c.as_str()) != state.last_candidate.as_deref() {
                    state.last_candidate = Some(c.clone());
                    return vec![verify_directive(&c)];
                }
            }
        }
        return Vec::new();
    }
    state.last_phase = state.phase;
    state.phase = next;
    let mut directives = Vec::new();
    match next {
        Phase::Scout => directives.push(scout_directive().to_string()),
        Phase::DeepDive => directives.push(deepdive_directive().to_string()),
        Phase::Verify => {
            let candidate = extract_candidate(messages);
            state.last_candidate = candidate.clone();
            if let Some(c) = candidate {
                directives.push(verify_directive(&c));
            } else {
                directives.push(verify_directive_without_candidate().to_string());
            }
        }
        Phase::Commit => {} // the driver's Commit/T25 prompt owns the final push
    }
    directives
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::llm::LlmRole;

    fn msg(role: LlmRole, content: &str) -> LlmMessage {
        LlmMessage {
            role,
            content: content.to_string(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            images: vec![],
        }
    }

    // ── phase_stage ────────────────────────────────────────────────────────
    #[test]
    fn wall_clock_phases_are_monotonic() {
        let d = Difficulty::Hard;
        assert_eq!(phase_stage(0, 0, 0.90, d, false), Phase::Scout);
        assert_eq!(phase_stage(0, 0, 0.50, d, false), Phase::DeepDive);
        assert_eq!(phase_stage(0, 0, 0.25, d, false), Phase::Verify);
        assert_eq!(phase_stage(0, 0, 0.10, d, false), Phase::Commit);
    }

    #[test]
    fn matches_convergence_boundaries() {
        // 0.30 → Verify (== Convergence::Converge), 0.15 → Commit (== Convergence::Commit)
        assert_eq!(
            phase_stage(0, 0, 0.30, Difficulty::Hard, false),
            Phase::Verify
        );
        assert_eq!(
            phase_stage(0, 0, 0.15, Difficulty::Hard, false),
            Phase::Commit
        );
    }

    #[test]
    fn verified_jumps_straight_to_verify() {
        assert_eq!(
            phase_stage(0, 0, 0.90, Difficulty::Hard, true),
            Phase::Verify
        );
    }

    #[test]
    fn simple_never_scouts() {
        // Simple questions skip Scout/DeepDive entirely (they commit fast).
        assert_eq!(
            phase_stage(0, 0, 0.90, Difficulty::Simple, false),
            Phase::Verify
        );
        assert_eq!(
            phase_stage(0, 0, 0.50, Difficulty::Simple, false),
            Phase::Verify
        );
    }

    #[test]
    fn turn_thresholds_match() {
        let d = Difficulty::Hard;
        assert_eq!(phase_stage(7, 10, 1.0, d, false), Phase::Verify); // 70% turns
        assert_eq!(phase_stage(9, 10, 1.0, d, false), Phase::Commit); // 90% turns
        assert_eq!(phase_stage(5, 10, 1.0, d, false), Phase::Scout); // 50% turns, fresh wall
    }

    // ── parse_subtask_count ────────────────────────────────────────────────
    #[test]
    fn parse_subtask_count_variants() {
        assert_eq!(parse_subtask_count("SUBTASKS: 3"), Some(3));
        assert_eq!(parse_subtask_count("plan:\nsubtasks: 5"), Some(5));
        assert_eq!(parse_subtask_count("no decomposition needed"), None);
        assert_eq!(parse_subtask_count("SUBTASKS: 0"), None);
        assert_eq!(parse_subtask_count("SUBTASKS: 1"), Some(1)); // governor rejects <2
        assert_eq!(parse_subtask_count("SUBTASKS: abc"), None);
    }

    // ── decide_fan_out ─────────────────────────────────────────────────────
    #[test]
    fn fanout_gate_simple() {
        assert_eq!(
            decide_fan_out(
                Phase::DeepDive,
                Difficulty::Simple,
                Decomposability::Independent(3),
                900,
                90,
                false
            ),
            FanOutDecision::NoFanOut
        );
    }

    #[test]
    fn fanout_gate_phase() {
        // Fan-out is a DeepDive tactic only.
        assert_eq!(
            decide_fan_out(
                Phase::Scout,
                Difficulty::Hard,
                Decomposability::Independent(3),
                900,
                90,
                false
            ),
            FanOutDecision::NoFanOut
        );
        assert_eq!(
            decide_fan_out(
                Phase::Verify,
                Difficulty::Hard,
                Decomposability::Independent(3),
                900,
                90,
                false
            ),
            FanOutDecision::NoFanOut
        );
    }

    #[test]
    fn fanout_gate_decomposability() {
        assert_eq!(
            decide_fan_out(
                Phase::DeepDive,
                Difficulty::Hard,
                Decomposability::Sequential,
                900,
                90,
                false
            ),
            FanOutDecision::NoFanOut
        );
        assert_eq!(
            decide_fan_out(
                Phase::DeepDive,
                Difficulty::Hard,
                Decomposability::Independent(1),
                900,
                90,
                false
            ),
            FanOutDecision::NoFanOut
        );
        assert_eq!(
            decide_fan_out(
                Phase::DeepDive,
                Difficulty::Hard,
                Decomposability::Independent(3),
                900,
                90,
                false
            ),
            FanOutDecision::FanOut(3)
        );
    }

    #[test]
    fn fanout_gate_overspawn() {
        assert_eq!(
            decide_fan_out(
                Phase::DeepDive,
                Difficulty::Hard,
                Decomposability::Independent(3),
                900,
                90,
                true
            ),
            FanOutDecision::NoFanOut
        );
    }

    #[test]
    fn fanout_cost_gate() {
        // 6 workers * 90s = 540s > 900*0.5 = 450 → halve to 3: 270 <= 450 → FanOut(3)
        assert_eq!(
            decide_fan_out(
                Phase::DeepDive,
                Difficulty::Hard,
                Decomposability::Independent(8),
                900,
                90,
                false
            ),
            FanOutDecision::FanOut(3)
        );
        // 3 * 90 = 270 > 200*0.5 = 100 → halve to 1 → NoFanOut
        assert_eq!(
            decide_fan_out(
                Phase::DeepDive,
                Difficulty::Hard,
                Decomposability::Independent(3),
                200,
                90,
                false
            ),
            FanOutDecision::NoFanOut
        );
        // plenty of time, capped at MAX_FANOUT_WORKERS
        assert_eq!(
            decide_fan_out(
                Phase::DeepDive,
                Difficulty::Hard,
                Decomposability::Independent(100),
                3600,
                90,
                false
            ),
            FanOutDecision::FanOut(6)
        );
    }

    // ── extract_candidate ──────────────────────────────────────────────────
    #[test]
    fn extract_candidate_ignores_tool_results() {
        let msgs = vec![
            msg(LlmRole::Tool, "Final answer: wrong-data"),
            msg(LlmRole::Assistant, "I computed 41.\nFinal answer: 41"),
        ];
        assert_eq!(extract_candidate(&msgs).as_deref(), Some("41"));

        let msgs2 = vec![msg(LlmRole::Assistant, "still researching")];
        assert_eq!(extract_candidate(&msgs2), None);
    }

    #[test]
    fn extract_candidate_uses_last_commit() {
        let msgs = vec![msg(
            LlmRole::Assistant,
            "Final answer: 40\nwait, recompute gives\nFinal answer: 41",
        )];
        assert_eq!(extract_candidate(&msgs).as_deref(), Some("41"));
    }

    // ── drive_phase ────────────────────────────────────────────────────────
    #[test]
    fn drive_phase_emits_one_directive_per_transition() {
        let mut st = MetaOrchestratorState::new();
        let msgs = vec![msg(LlmRole::User, "hard question 42")];

        // Same phase (Scout, fresh wall) → no directive.
        let d1 = drive_phase(&mut st, 0, 0, 0.90, Difficulty::Hard, false, &msgs);
        assert!(d1.is_empty());
        assert_eq!(st.phase, Phase::Scout);

        // Scout → DeepDive → one directive.
        let d2 = drive_phase(&mut st, 0, 0, 0.50, Difficulty::Hard, false, &msgs);
        assert_eq!(d2.len(), 1);
        assert!(d2[0].contains("DeepDive"));
        assert_eq!(st.phase, Phase::DeepDive);

        // DeepDive → Verify → one directive (no candidate yet → no-candidate text).
        let d3 = drive_phase(&mut st, 0, 0, 0.25, Difficulty::Hard, false, &msgs);
        assert_eq!(d3.len(), 1);
        assert!(d3[0].contains("asymmetric") || d3[0].contains("Verify"));
        assert_eq!(st.phase, Phase::Verify);

        // Verify → Commit → NO directive (driver owns the commit prompt).
        let d4 = drive_phase(&mut st, 0, 0, 0.10, Difficulty::Hard, false, &msgs);
        assert!(d4.is_empty());
        assert_eq!(st.phase, Phase::Commit);
    }

    #[test]
    fn drive_phase_verify_reshot_only_when_candidate_changes() {
        let mut st = MetaOrchestratorState::new();
        st.phase = Phase::Verify;
        st.last_phase = Phase::DeepDive;

        let msgs1 = vec![msg(LlmRole::Assistant, "Final answer: 41")];
        let d1 = drive_phase(&mut st, 0, 0, 0.25, Difficulty::Hard, true, &msgs1);
        assert_eq!(d1.len(), 1);
        assert!(d1[0].contains("41"));

        // Same candidate → silent.
        let d2 = drive_phase(&mut st, 0, 0, 0.25, Difficulty::Hard, true, &msgs1);
        assert!(d2.is_empty());

        // Candidate changed → re-send with the new value.
        let msgs2 = vec![msg(LlmRole::Assistant, "Final answer: 42")];
        let d3 = drive_phase(&mut st, 0, 0, 0.25, Difficulty::Hard, true, &msgs2);
        assert_eq!(d3.len(), 1);
        assert!(d3[0].contains("42"));
    }

    #[test]
    fn drive_phase_embedding_is_withholding_correct() {
        let msgs = vec![msg(LlmRole::Assistant, "Final answer: 41")];
        let mut st = MetaOrchestratorState::new();
        st.phase = Phase::Verify;
        let d = drive_phase(&mut st, 0, 0, 0.25, Difficulty::Hard, true, &msgs);
        assert!(d[0].contains("asymmetric"));
        assert!(d[0].contains("evidence alone"));
        assert!(d[0].contains("Do NOT paste your own derivation"));
    }
}
