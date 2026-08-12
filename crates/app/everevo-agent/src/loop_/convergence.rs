//! Benchmark-mode (EVEREVO_BENCHMARK) budget/convergence nudges plus the
//! forced terminal-commit prompt. Pure logic, unit-tested.

/// Escalating convergence stage for the turn budget. Pure logic, unit-tested.
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

/// Prompt for the forced terminal commit (benchmark mode).
pub(crate) fn forced_final_prompt() -> &'static str {
    "⏰ Turn budget exhausted. Do NOT call any tools. Based on everything you \
     have already gathered, output exactly one line: Final answer: <value>. \
     Nothing else."
}
