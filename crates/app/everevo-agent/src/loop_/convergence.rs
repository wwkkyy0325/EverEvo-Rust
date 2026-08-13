//! Benchmark-mode (EVEREVO_BENCHMARK) budget/convergence nudges plus the
//! forced terminal-commit prompt. Pure logic, unit-tested.

/// Escalating convergence stage for the turn budget. Pure logic, unit-tested.
///
/// The wall-clock/turn thresholds stay conservative (Converge at ~70% turns /
/// 30% wall-left, Commit at ~85% / 15%) — the anti-timeout fix is the
/// verified-aware runtime nudge in the driver, not earlier threshold firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Convergence {
    /// Keep exploring (budget not yet tight).
    None,
    /// ~70% of turn budget / ~30% wall-clock left — start converging.
    Converge,
    /// ~85% of turn budget / ~15% wall-clock left — commit now.
    Commit,
}

pub(crate) fn convergence_stage(turn: usize, max_turns: usize, wall_left_frac: f64) -> Convergence {
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
pub(crate) fn budget_line(turns_left: Option<usize>, wall_left_secs: Option<u64>) -> String {
    let turns = match turns_left {
        Some(n) => format!("{n} turns left"),
        None => "unbounded turns left".to_string(),
    };
    match wall_left_secs {
        Some(s) => format!("[Budget: {turns}, ~{s}s wall-clock left]"),
        None => format!("[Budget: {turns}]"),
    }
}

/// Non-verification tool-call turns after a verification step ran, before the
/// driver nudges a commit. Tuned on the dominant GAIA timeout mode (batches
/// 4-7): agents verified a candidate, then kept re-searching / re-fetching
/// instead of committing, hitting the 400s hard-stop with an empty prediction.
/// 6 gives a legitimate compound-question agent room to resolve its remaining
/// independent sub-parts before the nudge fires.
pub(crate) const POST_VERIFY_STALL_TURNS: usize = 6;

/// Balanced "wrap up" nudge for a verified-but-still-exploring agent. Re-arms
/// the satisficing rule (AnswerDiscipline) at runtime: a candidate with a
/// direct source and no contradiction is SUFFICIENT and is committed
/// immediately. Compound-safe — it forces wrap-up within 2 calls rather than
/// demanding a premature partial commit.
pub(crate) fn verified_wrapup_prompt(post_verify_turns: usize) -> String {
    format!(
        "Reminder: you already ran a verification step and have a candidate that \
         survived it ({post_verify_turns} non-verification tool calls since). Per the \
         satisficing rule, a candidate with a direct source and no contradiction is \
         SUFFICIENT and is committed immediately. If the verified evidence answers the \
         whole question, STOP and commit it on a single `Final answer:` line now. If an \
         open, answer-changing sub-question still remains, resolve it within AT MOST 2 \
         more tool calls, then commit. Do not keep expanding research."
    )
}

/// Hard deadline prompt for a verified-but-still-exploring agent (wall-clock
/// nearly gone). Unlike the generic deadline, it names the verified value the
/// model already owns so extraction beats narration.
pub(crate) fn verified_deadline_prompt() -> &'static str {
    "⏰ Deadline with a verified candidate: STOP. You already verified a value that \
     survived verification and you are still exploring. Do NOT call more tools. Your \
     very next response MUST end with a single `Final answer:` line containing the \
     verified value (or the corrected value if new evidence contradicted it). An \
     uncommitted verified answer scores 0 — commit it now."
}

/// Prompt for the forced terminal commit (benchmark mode).
///
/// Must force a VALUE, not narration: the run-4 Q3/Q46 failures committed
/// planning text ("Python is available... build the simulation") here because
/// the old prompt allowed the model to keep planning instead of extracting.
/// The rewrite forbids new plans/tools/code and demands extraction from the
/// reasoning already in the conversation, making an uncertain value acceptable.
pub(crate) fn forced_final_prompt() -> &'static str {
    "⏰ Turn budget exhausted. Do NOT call any tools, do NOT start new research, \
     do NOT write plans, code, or explanations. Read back the reasoning you have \
     ALREADY produced above — it contains either a computed answer or a strong \
     partial result. Extract the single best value from it and output exactly \
     one line: Final answer: <value>. An uncertain value beats an empty answer. \
     Nothing else — no prose before or after the line."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_turn_thresholds() {
        // Turn budget drives convergence when wall-clock is fresh.
        assert_eq!(convergence_stage(7, 10, 1.0), Convergence::Converge); // 70%
        assert_eq!(convergence_stage(9, 10, 1.0), Convergence::Commit); // 90%
        assert_eq!(convergence_stage(3, 10, 1.0), Convergence::None); // 30%
    }

    #[test]
    fn test_wall_clock_thresholds() {
        // Wall-clock alone drives convergence when turns are unbounded.
        assert_eq!(convergence_stage(0, 0, 0.30), Convergence::Converge);
        assert_eq!(convergence_stage(0, 0, 0.15), Convergence::Commit);
        assert_eq!(convergence_stage(0, 0, 0.99), Convergence::None);
    }

    #[test]
    fn test_unbounded_turns_uses_wall_frac() {
        assert_eq!(convergence_stage(0, 0, 0.10), Convergence::Commit);
        assert_eq!(convergence_stage(0, 0, 0.99), Convergence::None);
    }

    #[test]
    fn test_verified_wrapup_prompt_names_stall_count() {
        let p = verified_wrapup_prompt(7);
        assert!(p.contains("7 non-verification tool calls since"));
        assert!(p.contains("Final answer:"));
        assert!(p.contains("AT MOST 2"));
    }

    #[test]
    fn test_verified_deadline_prompt_forces_commit() {
        let p = verified_deadline_prompt();
        assert!(p.contains("verified"));
        assert!(p.contains("Final answer:"));
        assert!(p.contains("Do NOT call more tools"));
    }

    #[test]
    fn test_stall_threshold_is_sane() {
        // 6 non-verification turns is enough room for legitimate compound-part
        // research after a verification step, while still catching spirals.
        assert!(POST_VERIFY_STALL_TURNS >= 4);
        assert!(POST_VERIFY_STALL_TURNS <= 10);
    }
}
