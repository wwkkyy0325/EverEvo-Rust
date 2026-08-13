# Agent State Machine & Transition Table

Single source of truth for the agent's explicit state machines. The FSM is
implemented in `crates/app/everevo-agent/src/loop_/state.rs`; `run_loop`
routes its decisions through `transition()`. **Unit tests in `state.rs` assert
every row below (T1–T26), so this doc and the code cannot drift.**

## Loop states

| State | Meaning |
|---|---|
| `Init` | Entry — compute difficulty / verified flags |
| `Observe` | Turn start — drain sub-agents, trim/mask, rolling summary, compact |
| `Solve` | LLM call — produces text / tools / thinking |
| `Act` | Execute tool calls, track `verified`, merge results |
| `Verify` | Commit gate — hard + unverified → re-prompt (≤2) |
| `Stalled` | Verification spiral — verified candidate exists but still exploring (T21), emits verified-aware wrap-up nudge |
| `Escalating` | Convergence escalation — wall-clock crossed Converge/Commit stage (T23/T25), emits budget-tight nudge |
| `Converge` | Thinking without a committed value → forced convergence call |
| `WaitSubAgents` | LLM says done but sub-agents pending → yield |
| `TerminalCommit` | Loop-boundary forced no-tool commit (benchmark / wall-clock) |
| `Done` / `Error` / `Cancelled` | Terminals |

## Transition table

| # | From | Event / guard | To | Action |
|---|---|---|---|---|
| T1 | Init | — | Observe | difficulty/verified computed |
| T2 | Observe | — | Solve | context ready |
| T3 | Solve | stream error / stall >120s | Error | `AgentEvent::Error` |
| T4 | Solve | context overflow persists | Error | `/compact` message |
| T5 | Solve | tool_calls non-empty | Act | build assistant msg |
| T6 | Solve | tool_calls empty ∧ pending>0 | WaitSubAgents | drain + yield |
| T7 | Solve | tool_calls empty ∧ thinking-only | Converge | forced convergence |
| T8 | Solve | tool_calls empty ∧ text ∧ Hard ∧ !verified ∧ <2 | Verify | re-prompt |
| T9 | Solve | tool_calls empty ∧ text ∧ else | Done | retrospective + Done |
| T10 | Act | — | Observe | merge results, re-loop |
| T11 | Act | tool failure / user decline / blocked | Observe | failure_messages, re-loop |
| T12 | Verify | re-prompt count ≤ cap | Observe | continue |
| T13 | Verify | cap reached | Done | best-effort commit |
| T14 | Converge | — | Done | retrospective + Done |
| T15 | WaitSubAgents | server auto-continue re-entry | Init | new run_loop |
| T16 | any | cancel token | Cancelled | `AgentEvent::Error` |
| T17 | any | max_turns exhausted (no wall-clock) | Error | max-turns error |
| T18 | any | wall-clock ≤30s | TerminalCommit | forced commit |
| T19 | Solve | native-search truncated ∧ <4 retries | Solve | continue (self-loop) |
| T20 | Solve | context overflow (proactive) | Solve | autocompact → trim, retry |
| T21 | Act | `post_verify_turns >= POST_VERIFY_STALL_TURNS` ∧ !nudged (no escalation yet) | Stalled | emit verified-wrapup nudge (once) |
| T22 | Stalled | — (loop-boundary reset) | Observe | re-loop, next turn |
| T23 | Act | wall-clock crossed Converge stage | Escalating | emit converge nudge (verified-aware if verified) |
| T24 | Escalating | — (loop-boundary reset) | Observe | re-loop, next turn |
| T25 | Act | wall-clock crossed Commit stage | Escalating | emit commit/deadline nudge (verified-aware if verified) |
| T26 | Escalating | wall-clock ≤ 30s (global WallClockLow rule) | TerminalCommit | forced commit |

## Boundary notes

- `panic` is caught OUTSIDE the FSM by `mod.rs` `catch_unwind` →
  `AgentEvent::Error "Internal agent error"`.
- T19/T20 are `Solve` internal self-loops (the `continue` arcs), not new states.
- T16 (cancel) was a **known gap**: previously `run_loop` only noticed
  cancellation inside in-flight LLM/tool calls; now `Observe` checks it at turn
  start.
- T21 (Stalled) and T23/T25 (Escalating) fire in driver section 6 — the end of
  an `Act` turn, before the loop-boundary reset re-enters `Observe`. T22/T24
  are the loop-boundary re-entry rows, implemented collectively by the
  per-turn `state = LoopState::Observe` reset (same pattern as T10/T11/T12).
- T21 vs T23/T25 are mutually exclusive per turn (escalation wins): when the
  wall-clock has crossed into Converge/Commit, the escalation prompt (which is
  verified-aware when a verified candidate exists) supersedes the standalone
  stall nudge.

## Session lifecycle states

`everevo_core::types::SessionState` (`Idle | Running | WaitingUser | Completed
| Failed`) is persisted into the session's JSON metadata. Wiring:

| Transition | When |
|---|---|
| → Running | chat handler starts an agent run |
| → WaitingUser | `ask_user` tool blocks (awaiting_user SSE) |
| → Completed | `finalize_response` succeeds |
| → Failed | handler error / panic propagates |

The status endpoint (`GET /api/sessions/{id}`) reports the persisted state,
falling back to in-memory `session_actors` membership for live runs.

## Harness per-question states

`scripts/gaia_bench.py` `classify_terminal_state()` maps a `chat()` result to
`ok | timed_out | error`, with an intermediate `verifying` during the Phase-1b
terminal re-prompt. Every terminal condition the harness can produce is
classified explicitly; the checkpoint row carries a `state` field.
