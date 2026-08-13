# EverEvo Error Transition Table(错误转移表)

Single source of truth for how EVERY error category is detected, recovered, and
finally surfaced — the error analog of `agent-states.md`. The rule: **never
throw-and-ignore**. Every error either (a) recovers to a retry/fallback, (b)
returns a HELPFUL error the agent can act on, or (c) escalates to a terminal
event with a clear reason.

## LLM provider errors(`llm/http.rs`)

| Error | Detection | Recovery | On exhaustion | Ref |
|---|---|---|---|---|
| Rate limit 429 | `is_retryable(status)` | exp backoff (1s/2s/4s) + circuit counter | `AgentEvent::Error` "Rate limited — wait and retry" | http.rs chat/stream |
| Server 5xx | `status.is_server_error()` | same backoff | Error event with classified detail | http.rs |
| Auth 401/403 | `classify_http_error` | **no retry** (client error) | clear msg: "check API key in data/config.toml" | http.rs |
| Bad request 400 | `classify_http_error` | no retry, but body sanitizer prevents empty-content 400s | detail included (DeepSeek fixes) | http.rs body.rs |
| Connect / timeout | `e.is_connect() / is_timeout()` | backoff retry | "check network/proxy/base_url" | http.rs |
| Stream stall >120s | per-event timeout | — | `AgentEvent::Error` "LLM stream stalled" | driver.rs |
| Context overflow | error string match (`context_length_exceeded` / "prompt too long" / 413 / "too many tokens") | **trim to half budget → ONE retry → give up** (no emergency LLM autocompact in the overflow path — autocompact only runs preemptively at turn start) | "Context is too long even after emergency compaction — try /compact or a new session" | driver.rs |
| Circuit open | `circuit_allows()` | fast-fail (30s cooldown) | Error "provider circuit open — try again" | http.rs |

## Loop / agent errors(`loop_/driver.rs`)

| Error | Detection | Recovery | On exhaustion | Ref |
|---|---|---|---|---|
| Tool not found | `tools.get(name)==None` | error returned to agent with available-tool hint | agent picks another tool | driver.rs |
| Tool timeout | per-tool timeout (300s shell / 120s other) | `failure_messages` + continue | agent sees the failure and retries | driver.rs |
| Hook-blocked | error contains "blocked" | `ToolCallEnd is_error` + continue | agent re-routes | driver.rs |
| User declines | confirmation gate | skip + continue | — | driver.rs |
| Panic | `catch_unwind` | — | `AgentEvent::Error "Internal agent error"` | mod.rs |
| Cancel | `cancel.is_cancelled()` | T16 → `Cancelled` (≤1 turn) | terminal | driver.rs |
| **Verification spiral (top GAIA timeout cause)** | `post_verify_turns >= POST_VERIFY_STALL_TURNS` (6 non-verify turns after a verification step) | **T21 Act→Stalled** — verified-aware wrap-up nudge (`verified_wrapup_prompt`), once; at Converge/Commit stage **T23/T25 Act→Escalating** replaces the generic prompt | forced terminal commit (T18/T26) extracts the value | convergence.rs / driver.rs / state.rs |
| **Circular verifier warning** (`expected == answer`) | verify_candidate.py reports circular | VerifyCandidate stage 0: re-derive via a DIFFERENT path (raw recompute / `cluster verify`), never dismiss | commit best-effort verified value | verify_candidate.rs |

## Tool errors(tool `execute`)

| Error | Detection | Recovery | On exhaustion | Ref |
|---|---|---|---|---|
| Invalid params | `EverEvoError::InvalidInput` | **return a HELPFUL message the agent can recover from** (e.g. "action required: init\|add_node\|…") | agent retries correctly | tools |
| Missing required arg | schema `required` + runtime check | helpful hint listing valid values | agent corrects | tools |
| Unknown action / name | match fallback | message lists valid actions | agent picks a valid one | problem_model / pipeline |
| Sandbox permission denied | sandbox error | `ToolCallEnd is_error` + reflect gate hint | agent requests permission / alternative | sandbox |

## Session / DB / infra errors

| Error | Detection | Recovery | On exhaustion | Ref |
|---|---|---|---|---|
| DB error (per-turn) | `db.*` Err | best-effort warn + continue (non-fatal) | session still answers | handler.rs / session_content.rs |
| RAG/embedding unavailable | startup check | warn + fallback to memory facts only | non-fatal | app_state.rs |
| Asset integrity | startup check | **Fail when ALL missing** ("No assets installed — run provisioning"); Warn on partial (ok>0); non-fatal for API | start with degraded capability | startup_check.rs |
| HF dataset unreachable | load_gaia_dataset | cache-first offline load | sample fallback | gaia_bench.py |

## Auto-continue escalation

| Error | Detection | Recovery | On exhaustion | Ref |
|---|---|---|---|---|
| Auto-continue restart loop | `auto_cycles > MAX_AUTO_CYCLES` (5) | **force final synthesis** — break the restart loop | session answers with accumulated sub-agent results | handler.rs |
| Sub-agents stalled (no progress) | `pending >= last_pending && auto_cycles > 1` | **force final synthesis** — break instead of waiting forever | session answers | handler.rs |
| Thinking-only turn (LLM reasoned but produced no text) | `current_text.is_empty() && !current_thinking.is_empty()` | **T7 → Converge**: push reasoning as assistant msg + `forced_final_prompt()`, run one no-tool `llm.chat`, commit the result — over-confident guess beats a silent empty answer | commit best-effort value | driver.rs |

> Audit MEDIUM (2026-08-13): the DB row's "non-fatal" claim was WRONG for the
> P2 `SessionContent::persist_user` path — a DB write error propagated with `?`
> and killed the whole turn. Fixed in handler.rs: `persist_user` failure now
> warns and continues. The row above is accurate again.

## Vision / multimodal recovery

| Error | Detection | Recovery | On exhaustion | Ref |
|---|---|---|---|---|
| `describe_image` fails / times out (image question) | vision call errors twice | NO manual pixel/ASCII forensics; use the offline script when one applies (chess_fen.py / fractions_ocr.py) | commit best-effort reading marked [UNVERIFIED] — a value beats an empty timeout | context.rs Critical Rules / describe_image.rs |

## Sandbox recovery

| Error | Detection | Recovery | On exhaustion | Ref |
|---|---|---|---|---|
| Compute cell timeout (30s default) | `killed_by_timeout` && compute command | ONE-shot auto-retry at 3× budget (max 300s) — pure compute only, never interactive/network/mutating | normal timeout message + checkpoint advice | shell.rs |
| Destructive git op / ConfirmRequired | git guard / `needs_confirmation` | gate prompts user; re-invoke with `confirmed: true` after approval | agent surfaces to user | shell.rs |
| Sandbox permission denied | permission decision (block / confirm) | blocked → clear message; confirm → gate | agent requests permission / alternative | sandbox / shell.rs |

## Cross-cutting principle

1. **Retry with backoff** for transient (network, 429, 5xx) — never a hard fail.
2. **Circuit breaker** bounds repeated failures (protect the provider).
3. **Helpful errors** for agent-controllable mistakes (params, unknown action) —
   list the valid options so the agent self-corrects.
4. **Non-fatal degradation** for infra that doesn't block answering (DB, RAG,
   assets).
5. **Terminal escalation** only when no recovery exists, with a clear reason.

The table rows above are asserted in code wherever feasible (tests in
`llm/http.rs` circuit module, `loop_/state.rs` terminal arcs, tool unit tests).
