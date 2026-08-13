# ADR 0008 — Agent State Machine + Session/Harness Lifecycle States

- **Status:** Accepted
- **Date:** 2026-08-12
- **Decision Makers:** user + Claude Code

## Context

The agent loop had **no top-level FSM**: a flat `while` loop with 12 implicit
terminal `return` arcs and 3 `continue` re-loop arcs. States were not
enumerable or testable; a cancel between turns was only noticed by the next
in-flight LLM/tool call. The server-side `SessionState` enum
(`types.rs:44-56`) existed but was **never written** (dead code — the status
endpoint always reported `idle`). The benchmark harness had no per-question
state machine.

Research (cross-validated 2026-08-12): Agent-as-FSM is the 2026 production
standard (StateFlow, COLM 2024: +25.8% SQL, 4.7× cheaper); deterministic
state transitions separated from in-state LLM solving. Circuit breakers and
rate-limit recovery FSMs are the dominant reliability pattern (rate limiting =
93.75% degradation).

## Decision

1. **Explicit loop FSM** in `loop_/state.rs`: `LoopState` +
   `LoopEvent` + pure `transition()` implementing table T1-T20; unit tests
   assert every row. `run_loop` routes its decision points through it.
2. **Cancellation gap fix**: `Observe` checks `cancel.is_cancelled()` at turn
   start (T16).
3. **Circuit breaker** in `llm/http.rs`: `Closed → Open → HalfOpen → Closed`
   after 5 consecutive transient failures, 30s cooldown, half-open probe. The
   existing exponential backoff (1s/2s/4s, MAX_RETRIES=3) stays as the
   per-call retry.
4. **Revive `SessionState`**: `Running` / `WaitingUser` / `Completed` /
   `Failed` persisted via `update_session_metadata` at the chat lifecycle
   points.
5. **Harness per-question states**: `ok | timed_out | error | verifying`
   recorded in the checkpoint.

## Consequences

- All loop terminal conditions are enumerated, documented
  (`docs/llmwiki/agent-states.md`), and asserted by tests — no more implicit
  or unanticipated exits.
- Cancellation is prompt (≤1 turn) instead of waiting for the next I/O.
- A down / rate-limited provider fails fast across all sessions instead of
  burning backoff on every call.
- Session status reports real state; reconnect can distinguish
  `WaitingUser` from completed/failed.
- Lightweight by design: hand-written enum + `match`, no external state-graph
  library, no semantics change to the loop body (control-flow-only refactor).
