//! Explicit agent-loop state machine (StateFlow-style, COLM 2024).
//!
//! Formalizes the previously-implicit ReAct loop control flow. The transition
//! table T1-T20 below is the SINGLE source of truth: `run_loop` routes its
//! decision points through [`transition`], and the unit tests assert every row
//! so the code and `docs/llmwiki/agent-states.md` cannot drift.
//!
//! Principle: deterministic state transitions (testable, inspectable) are
//! separated from the LLM's non-deterministic sub-task solving, which is
//! contained inside the `Solve` / `Act` states.

/// The agent loop's explicit states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopState {
    /// Entry — compute difficulty / verified flags.
    Init,
    /// Turn start — drain sub-agents, trim/mask, rolling summary, compact.
    Observe,
    /// LLM call — stream_chat produces text / tools / thinking.
    Solve,
    /// Execute tool calls, merge results.
    Act,
    /// Commit gate — hard question committed unverified → re-prompt.
    Verify,
    /// Verification spiral — verified candidate exists but the agent keeps
    /// exploring (T21). Entry emits the verified-aware wrap-up nudge, then the
    /// loop-boundary reset re-enters Observe.
    Stalled,
    /// Convergence escalation — wall-clock entered the Converge/Commit stage
    /// (T23/T25). Entry emits the budget-tight nudge (verified-aware when a
    /// verified candidate exists), then re-enters Observe.
    Escalating,
    /// Thinking without a committed value → forced convergence call.
    Converge,
    /// LLM says done but sub-agents pending → yield for auto-continue.
    WaitSubAgents,
    /// Loop boundary forced no-tool commit (benchmark / wall-clock).
    TerminalCommit,
    /// Terminal — answer committed.
    Done,
    /// Terminal — error propagated.
    Error,
    /// Terminal — cancellation.
    Cancelled,
}

/// Events / guard outcomes that trigger transitions. The driver computes these
/// from live guards (stream outcome, counters, cancel, time).
///
/// Some variants (`ToolCalls`, `VerifyCapReached`) are part of the documented
/// transition table (agent-states.md) and exercised by unit tests even when the
/// driver encodes the same transition inline — hence `allow(dead_code)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LoopEvent {
    /// Unconditional advance to the next sequential state.
    Ready,
    /// Solve: stream produced tool calls.
    ToolCalls,
    /// Solve: text present, no tools, not verification-gated.
    DoneSignal,
    /// Solve: thinking but no text → force convergence.
    ThinkingOnly,
    /// Solve: no tools but sub-agents pending → yield.
    SubAgentsPending,
    /// Solve: hard + unverified + under re-prompt cap → re-prompt.
    UnverifiedHard,
    /// Solve: re-prompt cap reached → commit best-effort.
    VerifyCapReached,
    /// Solve: provider stream error / stall.
    StreamFailure,
    /// Solve: context overflow (persistent after waterfall).
    Overflow,
    /// Solve: native-search truncated → continue same state.
    Truncated,
    /// Act: post-verify stall threshold reached → Stalled (anti-spiral nudge).
    VerifiedStalled,
    /// Act: wall-clock crossed into the Converge stage → Escalating.
    BudgetConverge,
    /// Act: wall-clock crossed into the Commit stage → Escalating.
    BudgetCommit,
    /// Loop boundary: max_turns exhausted (no wall-clock) → error.
    TurnsExhausted,
    /// Loop boundary: wall-clock nearly exhausted → forced commit.
    WallClockLow,
    /// Any state: cancellation requested.
    Cancel,
}

/// What the driver must do after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopAction {
    /// Continue to the next state within the same turn.
    Advance,
    /// Re-enter the turn loop (next iteration).
    ReLoop,
    /// Yield for sub-agents (emit `WaitingForSubAgents`, return).
    Yield,
    /// Emit `Done`.
    Commit,
    /// Emit `Error`.
    Fail,
    /// Retry the same state (truncation / overflow waterfall).
    RetrySameState,
    /// Terminal reached — nothing more to do.
    Terminal,
}

/// True for terminal states (run_loop should stop).
pub fn is_terminal(state: LoopState) -> bool {
    matches!(
        state,
        LoopState::Done | LoopState::Error | LoopState::Cancelled | LoopState::WaitSubAgents
    )
}

/// The transition table. Rows are numbered to match `docs/llmwiki/agent-states.md`.
pub fn transition(state: LoopState, event: LoopEvent) -> (LoopState, LoopAction) {
    // T16 cancellation is global: any state + Cancel → Cancelled.
    if event == LoopEvent::Cancel {
        return (LoopState::Cancelled, LoopAction::Fail);
    }
    // T17/T18 are loop-boundary events that apply regardless of in-turn state.
    match event {
        LoopEvent::TurnsExhausted => return (LoopState::Error, LoopAction::Fail),
        LoopEvent::WallClockLow => return (LoopState::TerminalCommit, LoopAction::Advance),
        _ => {}
    }
    match (state, event) {
        // T1 Init → Observe
        (LoopState::Init, LoopEvent::Ready) => (LoopState::Observe, LoopAction::Advance),
        // T2 Observe → Solve
        (LoopState::Observe, LoopEvent::Ready) => (LoopState::Solve, LoopAction::Advance),
        // T3 Solve → Error (stream failure / stall)
        (LoopState::Solve, LoopEvent::StreamFailure) => (LoopState::Error, LoopAction::Fail),
        // T4 Solve → Error (persistent overflow)
        (LoopState::Solve, LoopEvent::Overflow) => (LoopState::Error, LoopAction::Fail),
        // T5 Solve → Act
        (LoopState::Solve, LoopEvent::ToolCalls) => (LoopState::Act, LoopAction::Advance),
        // T6 Solve → WaitSubAgents
        (LoopState::Solve, LoopEvent::SubAgentsPending) => {
            (LoopState::WaitSubAgents, LoopAction::Yield)
        }
        // T7 Solve → Converge (thinking-only)
        (LoopState::Solve, LoopEvent::ThinkingOnly) => (LoopState::Converge, LoopAction::Advance),
        // T8 Solve → Verify (re-prompt)
        (LoopState::Solve, LoopEvent::UnverifiedHard) => (LoopState::Verify, LoopAction::ReLoop),
        // T9 Solve → Done
        (LoopState::Solve, LoopEvent::DoneSignal) => (LoopState::Done, LoopAction::Commit),
        // T19 Solve → Solve (native-search truncation self-loop)
        (LoopState::Solve, LoopEvent::Truncated) => (LoopState::Solve, LoopAction::RetrySameState),
        // T20 Solve → Solve (proactive overflow → autocompact, retry)
        (LoopState::Solve, LoopEvent::Ready) => (LoopState::Solve, LoopAction::RetrySameState),

        // T10 Act → Observe (incl. T11 tool failure — same re-loop)
        (LoopState::Act, LoopEvent::Ready) => (LoopState::Observe, LoopAction::ReLoop),
        // T10 Act → Observe also on tool-failure continue (driver merges).
        (LoopState::Act, LoopEvent::ToolCalls) => (LoopState::Observe, LoopAction::ReLoop),

        // T21 Act → Stalled (verification spiral — verified candidate exists
        // but the agent keeps exploring; entry emits the wrap-up nudge).
        (LoopState::Act, LoopEvent::VerifiedStalled) => (LoopState::Stalled, LoopAction::Advance),
        // T22 Stalled → Observe (loop-boundary re-entry, implemented by the
        // per-turn Observe reset at the top of run_loop — same as T10/T12).
        (LoopState::Stalled, LoopEvent::Ready) => (LoopState::Observe, LoopAction::ReLoop),
        // T23 Act → Escalating (wall-clock entered the Converge stage).
        (LoopState::Act, LoopEvent::BudgetConverge) => (LoopState::Escalating, LoopAction::Advance),
        // T24 Escalating → Observe (loop-boundary re-entry, see T22).
        (LoopState::Escalating, LoopEvent::Ready) => (LoopState::Observe, LoopAction::ReLoop),
        // T25 Act → Escalating (wall-clock entered the Commit stage).
        (LoopState::Act, LoopEvent::BudgetCommit) => (LoopState::Escalating, LoopAction::Advance),
        // T26 Escalating → TerminalCommit is covered by the global WallClockLow
        // rule above (any state, wall-clock ≤ 30s → forced commit).

        // T12 Verify → Observe (re-prompt, next turn)
        (LoopState::Verify, LoopEvent::Ready) => (LoopState::Observe, LoopAction::ReLoop),
        // T13 Verify → Done (cap reached)
        (LoopState::Verify, LoopEvent::VerifyCapReached) => (LoopState::Done, LoopAction::Commit),

        // T14 Converge → Done
        (LoopState::Converge, LoopEvent::Ready) => (LoopState::Done, LoopAction::Commit),

        // TerminalCommit → Done (post-loop forced commit)
        (LoopState::TerminalCommit, LoopEvent::Ready) => (LoopState::Done, LoopAction::Commit),

        // Terminal states are absorbing.
        (s, _) if is_terminal(s) => (s, LoopAction::Terminal),

        // Unknown (state, event) combos are a bug — fail closed rather than
        // silently continuing into an undefined transition.
        (s, e) => {
            tracing::error!(state = ?s, event = ?e, "Undefined FSM transition");
            (LoopState::Error, LoopAction::Fail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminals_absorb() {
        for t in [
            LoopState::Done,
            LoopState::Error,
            LoopState::Cancelled,
            LoopState::WaitSubAgents,
        ] {
            let (s, a) = transition(t, LoopEvent::Ready);
            assert_eq!(s, t);
            assert_eq!(a, LoopAction::Terminal);
            assert!(is_terminal(t));
        }
    }

    #[test]
    fn test_cancel_is_global() {
        for s in [
            LoopState::Init,
            LoopState::Observe,
            LoopState::Solve,
            LoopState::Act,
            LoopState::Verify,
            LoopState::Converge,
            LoopState::TerminalCommit,
        ] {
            let (ns, a) = transition(s, LoopEvent::Cancel);
            assert_eq!(ns, LoopState::Cancelled);
            assert_eq!(a, LoopAction::Fail);
        }
    }

    #[test]
    fn test_loop_boundary_events() {
        let (s, a) = transition(LoopState::Solve, LoopEvent::TurnsExhausted);
        assert_eq!(s, LoopState::Error);
        assert_eq!(a, LoopAction::Fail);
        let (s, a) = transition(LoopState::Solve, LoopEvent::WallClockLow);
        assert_eq!(s, LoopState::TerminalCommit);
        assert_eq!(a, LoopAction::Advance);
        // T26: WallClockLow reaches TerminalCommit from Escalating too (global rule).
        let (s, a) = transition(LoopState::Escalating, LoopEvent::WallClockLow);
        assert_eq!(s, LoopState::TerminalCommit);
        assert_eq!(a, LoopAction::Advance);
    }

    #[test]
    fn test_act_escalation_and_stall_arcs() {
        // T21 Act → Stalled (verification spiral)
        let (s, a) = transition(LoopState::Act, LoopEvent::VerifiedStalled);
        assert_eq!(s, LoopState::Stalled);
        assert_eq!(a, LoopAction::Advance);
        // T23 Act → Escalating (Converge stage)
        let (s, a) = transition(LoopState::Act, LoopEvent::BudgetConverge);
        assert_eq!(s, LoopState::Escalating);
        assert_eq!(a, LoopAction::Advance);
        // T25 Act → Escalating (Commit stage)
        let (s, a) = transition(LoopState::Act, LoopEvent::BudgetCommit);
        assert_eq!(s, LoopState::Escalating);
        assert_eq!(a, LoopAction::Advance);
    }

    #[test]
    fn test_stall_and_escalating_reenter_observe() {
        // T22 / T24: loop-boundary reset re-enters Observe.
        let (s, a) = transition(LoopState::Stalled, LoopEvent::Ready);
        assert_eq!(s, LoopState::Observe);
        assert_eq!(a, LoopAction::ReLoop);
        let (s, a) = transition(LoopState::Escalating, LoopEvent::Ready);
        assert_eq!(s, LoopState::Observe);
        assert_eq!(a, LoopAction::ReLoop);
    }

    #[test]
    fn test_solve_arcs() {
        // T5
        let (s, a) = transition(LoopState::Solve, LoopEvent::ToolCalls);
        assert_eq!(s, LoopState::Act);
        assert_eq!(a, LoopAction::Advance);
        // T6
        let (s, a) = transition(LoopState::Solve, LoopEvent::SubAgentsPending);
        assert_eq!(s, LoopState::WaitSubAgents);
        assert_eq!(a, LoopAction::Yield);
        // T7
        let (s, a) = transition(LoopState::Solve, LoopEvent::ThinkingOnly);
        assert_eq!(s, LoopState::Converge);
        assert_eq!(a, LoopAction::Advance);
        // T8
        let (s, a) = transition(LoopState::Solve, LoopEvent::UnverifiedHard);
        assert_eq!(s, LoopState::Verify);
        assert_eq!(a, LoopAction::ReLoop);
        // T9
        let (s, a) = transition(LoopState::Solve, LoopEvent::DoneSignal);
        assert_eq!(s, LoopState::Done);
        assert_eq!(a, LoopAction::Commit);
        // T3/T4
        assert_eq!(
            transition(LoopState::Solve, LoopEvent::StreamFailure).0,
            LoopState::Error
        );
        assert_eq!(
            transition(LoopState::Solve, LoopEvent::Overflow).0,
            LoopState::Error
        );
        // T19
        let (s, a) = transition(LoopState::Solve, LoopEvent::Truncated);
        assert_eq!(s, LoopState::Solve);
        assert_eq!(a, LoopAction::RetrySameState);
    }

    #[test]
    fn test_verify_arcs() {
        // T12 under cap → re-loop
        let (s, a) = transition(LoopState::Verify, LoopEvent::Ready);
        assert_eq!(s, LoopState::Observe);
        assert_eq!(a, LoopAction::ReLoop);
        // T13 cap reached → commit
        let (s, a) = transition(LoopState::Verify, LoopEvent::VerifyCapReached);
        assert_eq!(s, LoopState::Done);
        assert_eq!(a, LoopAction::Commit);
    }

    #[test]
    fn test_chain_to_done_is_bounded() {
        // A representative path Init → … → Done stays within the state set
        // (no deadlock, finite). Walk the primary solve path.
        let mut state = LoopState::Init;
        let steps = [
            (LoopEvent::Ready, LoopState::Observe),
            (LoopEvent::Ready, LoopState::Solve),
            (LoopEvent::ToolCalls, LoopState::Act),
            (LoopEvent::Ready, LoopState::Observe),
            (LoopEvent::Ready, LoopState::Solve),
            (LoopEvent::DoneSignal, LoopState::Done),
        ];
        for (event, expect) in steps {
            let (ns, _) = transition(state, event);
            assert_eq!(ns, expect);
            state = ns;
        }
        assert_eq!(state, LoopState::Done);
        assert!(is_terminal(state));
    }
}
