# ADR-0005: Blocking ask_user Tool + Model Context-Window Adaptation Pipeline

**Status:** accepted
**Date:** 2026-08-12
**Deciders:** wwkkyy0325 (user decisions: "照抄 claude" for ask timeout; "按 token 预算从新到旧滑窗" for history; "落库持久化" for ask persistence)

---

## Context

Two gaps blocked long-running product sessions:

1. **No blocking human-in-the-loop tool.** The agent could run shell commands (after a 300s approval gate) but had no way to ask the user a free-form question and block until the reply arrived. Benchmarks under `EVEREVO_BENCHMARK=1` need the opposite — a short-circuit so GAIA never hangs on a question nobody can answer.

2. **Context assembly ignored the model's real window.** `AppConfig.max_context_tokens` (100k) was a global knob; the main session hardcoded `max_context_chars=80_000`; history was truncated by `max_messages=50` regardless of the actual provider window. A 1M-token model and a 32K-token vision model got identical budgets, and no provider advertised a window to the main loop.

## Decision

### Feature 1 — `ask_user` tool, Claude-Code semantics

- New tool `ask_user` (params: `question`, required), registered **only** on the main agent loop (not sub-agents). `risk_level: Low`.
- **Infinite block, no auto-timeout.** The tool parks a `oneshot::Sender<String>` in `AppState.ask_user`, emits an `awaiting_user` SSE event, and awaits the reply. It is exempted from the per-tool timeout in `loop_/driver.rs`; termination happens only via SSE disconnect or `/interrupt` cancel (`tokio::select!` on a cancellation token).
- **Short-circuit under headless:** when `EVEREVO_BENCHMARK=1` or the session is `fully_auto`, `execute()` returns immediately with `"User not available (auto mode). Use best judgment and proceed."` — never blocks.
- **Persistence:** the question is stored as an assistant message when the SSE event fires; the reply is stored as a user message when the answer lands. Refresh-safe, and the agent sees its own Q&A in history.
- **Transport:** reuses the existing confirmation gate pattern — `PendingAsk` + oneshot in an `Arc<RwLock<HashMap<Uuid, PendingAsk>>>`, forwarded over a per-session `mpsc::unbounded` channel to the SSE handler. New REST endpoint `POST /api/sessions/{id}/ask` fires the oneshot.

### Feature 2 — `ContextBudget` adaptation pipeline, 128k floor

- New `ContextBudget` in `everevo-core::context::budget`. Computed once per request from the **main provider's** `context_window`:
  - `window` = `context_window` (post-floor). **128k is a floor, not a clamp**: `None` → 128_000; `Some(32768)` stays 32768; tiny windows guarded with `saturating_sub` and a 1_000 minimum.
  - `safety_margin` = window/10; `output_reserve` = `(window/50).clamp(2048, 8192)`.
  - `available` = window − safety − output. Split: fixed stages 14.5%, memory+domain 15%, rolling_summary 4%, **history = remainder** (~66.5%). Invariant: fixed + memory + summary + history == available.
- `ContextBuildContext` gains `budget: ContextBudget`; `ContextSnapshot` exposes `available_tokens` / `safety_reserved` / `output_reserved`.
- `ConversationHistoryStage` switches from `max_messages=50` to a **token-budget sliding window, newest-first** (`history_window()` accumulates `estimate_tokens` until the history budget is exhausted) whenever `budget.window > 0`; legacy `max_messages` remains as fallback.
- `MemoryStage` and `DomainStage` clamp their assembled text by `budget.stage("memory")` / `budget.stage("domain_knowledge")`, appending a truncation note.
- Main loop wiring: `AppState.main_llm: RwLock<Option<ResolvedProvider>>` resolved from `[routing] mainModelId` (fallback `"primary"`); handler computes `ContextBudget::resolve(...)` and feeds both `ContextBuildContext` and `AgentLoop::with_context_budget(window * 4)` (chars-per-token ×4).
- `LlmProviderConfig` (core types) gains `#[serde(default)] context_window: Option<u32>` + `from_env_*` env plumbing (`*_CONTEXT_WINDOW`).
- `AppConfig.max_context_tokens` no longer drives the main input budget; it remains only for `summarize_threshold` and compact-provider maintenance.

## Alternatives Considered

| Option | Pros | Cons | Why Rejected |
|--------|------|------|-------------|
| ask_user with a configurable timeout | bounded worst-case | user may legitimately take minutes; timeout forces a fake answer | 照抄 Claude Code: infinite block, cancel-only |
| ask_user as a non-blocking "suggestion" event | never stalls the loop | the agent can't act on the answer in the same turn | user explicitly asked for 阻塞等待 (block-and-wait) |
| Use `max_context_tokens` / `max_messages` as-is | no new code | ignores real provider window; 1M vs 32K treated identically | user asked for per-model adaptation |
| Clamp every window up to 128k floor | uniform floor | shrinks small vision models to nothing meaningful; the 128k floor was specified as "only when unset" | 128k is a floor, not a clamp |
| Token-budget oldest-first history | keeps oldest context | drops the most recent turns — worse for continuity | user chose newest-first |

## Consequences

**Easier:** agents can genuinely ask for user intent mid-task; every model gets a proportional input budget; small-window vision models won't overflow; the main session finally uses the provider's real window instead of the 80k hardcode.

**Harder:** `ask_user` blocks the main loop indefinitely by design (the UI must surface the dialog clearly; SSE disconnect is the escape hatch); token-based history means very long sessions keep only the newest turns unless summaries grow; `resolve_main_provider` must be re-run on every routing/config reload to stay in sync.

## Affected Interfaces

- [x] `everevo-server::ask_user_tool::AskUserTool` — new `Tool` impl (`ask_user`, `question` required)
- [x] `AppState.ask_user` / `AppState.main_llm` — new fields (`Arc<RwLock<HashMap<Uuid, PendingAsk>>>`, `RwLock<Option<ResolvedProvider>>`)
- [x] `PendingAsk` / `AskNotification` — new structs
- [x] `AppState::resolve_main_provider()` — new method
- [x] `POST /api/sessions/{id}/ask` — new endpoint (body `{reply}`), 400 on empty / 404 when no pending ask
- [x] SSE event `awaiting_user` — new event (payload `{session_id, question}`)
- [x] `everevo-core::context::{ContextBudget, DEFAULT_CONTEXT_WINDOW}` — new (128_000)
- [x] `ContextBuildContext.budget` — new field (`ContextBudget`, serde-defaulted)
- [x] `ContextSnapshot.{available_tokens,safety_reserved,output_reserved}` — new fields
- [x] `LlmProviderConfig.context_window` — new core field (`Option<u32>`, `#[serde(default)]`)
- [x] `ConversationHistoryStage` — token-budget newest-first window when `budget.window > 0`
- [x] `MemoryStage` / `DomainStage` — budget-clamped output
- [x] `frontend/src/store.ts` — `askQueue` / `resolveAsk` / `awaiting_user` SSE case
- [x] `frontend/src/components/AskUserDialog.tsx` — new dialog
- [x] `frontend/src/components/SettingsView.tsx` — routing selects show `context_window` (e.g. `model · 128K`)

## Migration Path

Backward compatible. `ContextBudget::default()` is a legacy sentinel (`window == 0`), so any `ContextBuildContext` literal that predates the `budget` field compiles unchanged and behaves as before (fallback `max_messages` path, legacy >40% oversized threshold). `AppState::new` resolves `main_llm` automatically; config/reload paths re-resolve it. Existing configs without `context_window` get the 128k floor for the main model.
