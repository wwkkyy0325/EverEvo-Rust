# ADR-0004: Telemetry Injection Pipeline

**Status:** accepted
**Date:** 2026-08-10
**Deciders:** wwkkyy0325

---

## Context

EverEvo already had a complete telemetry **storage layer** (`everevo-core/src/telemetry/`:
`Telemetry` sink + SQLite background writer + `Trace`/`SpanGuard` + `AgentTurnRecord`/`RetrievalRecord`),
but the **record layer was dead code and scattered**:

- `chat/handler.rs` started a trace and got a `trace_id`, but `AppState::build_memory_stage(_trace_id)`
  **discarded** it (parameter prefixed `_`, never used).
- `AgentLoop::main_session(...)` and the auto-continue `AgentLoop::new()` **never** called
  `.with_telemetry(...)`, so `loop_/mod.rs`'s `record_agent_turn` and `stages/memory.rs`'s
  `record_retrieval` could never fire in production (telemetry/trace_id were both `None`).

Result: the performance/effect monitoring the storage layer was built for never wrote a single row
in production. The user asked to make a **telemetry content-injection pipeline** analogous to the
context-injection pipeline (`ContextStage`/`ContextPipeline`): one registered pipeline, not scattered
`record_*()` call sites.

## Decision

Introduce `crates/kernel/everevo-core/src/telemetry/pipeline.rs`, mirroring the `ContextStage` pattern:

- **`TelemetryStage`** trait — `priority()`, `name()`, `emit(&TelemetryEmitContext) -> Vec<TelemetryRecord>`.
  An empty `Vec` means "no contribution" (mirrors `ContextStage::build` returning `None`).
- **`TelemetryPipeline`** — priority-sorted registered stages + a `Telemetry` sink. `with_stage()` sorts
  by priority; `emit()` runs every stage, collects a `TelemetrySnapshot`, and dispatches records to the
  sink; `start_trace()` delegates to the sink.
- **`TelemetryEmitContext`** — one all-Option struct per emit cycle with per-slice fields
  (agent-turn slice, retrieval slice, experiment). Stages act only on the slice(s) they own.
- **`TelemetryRecord`** — `AgentTurn(AgentTurnRecord)` / `Retrieval(RetrievalRecord)` enum.
- **`default_telemetry_pipeline(sink)`** — registers `RetrievalTelemetryStage` (priority 10) and
  `TurnTelemetryStage` (priority 20), the two emitters that existed as hand-written call sites.

Wiring (this is the fix, not just the abstraction):

- `AppState::init_telemetry` now returns `Arc<TelemetryPipeline>` (field renamed `telemetry` →
  `telemetry_pipeline`); `build_memory_stage(trace_id)` now passes the trace to
  `MemoryStage::with_telemetry(pipeline, trace_id)`.
- `chat/handler.rs` starts the trace via `state.telemetry_pipeline` and calls
  `AgentLoop::with_telemetry(...)` on both the main session and the auto-continue loop.
- `loop_/mod.rs` and `stages/memory.rs` emit through the pipeline with a `TelemetryEmitContext`
  instead of hand-building records.

## Alternatives Considered

| Option | Pros | Cons | Why Rejected |
|--------|------|------|-------------|
| A: Just thread `trace_id` into the existing `record_*()` calls | Minimal diff | Still scattered call sites; no uniform observability snapshot; doesn't answer "one registered pipeline" | Rejected — user explicitly asked for a registered pipeline |
| B (chosen): `ContextStage`-style registered pipeline | Mirrors existing `ContextStage` idiom; one registration point; snapshot observability; extension point for future emitters | Slightly larger diff; renames `Arc<Telemetry>` → `Arc<TelemetryPipeline>` in public builders | — |
| C: Reuse `Trace::span()` for everything | No new types | Spans are duration-based, not the keyed metrics we need; loses the retrieval/turn distinction | Rejected |

## Consequences

**Easier:** telemetry record producers are now added as `TelemetryStage` implementations registered
in `default_telemetry_pipeline()` (design doc `docs/llmwiki/archive/design/config-telemetry.md` §3.1 lists the
remaining planned sites — llm.rs spans, sandbox, domain/vector/kg, server main). One
`TelemetryPipeline::emit()` call replaces each scattered record construction.

**Harder:** consumers must pass a `TelemetryPipeline` (not the raw `Telemetry`) to
`AgentLoop::with_telemetry` / `MemoryStage::with_telemetry`. `Telemetry` is still reachable via
`TelemetryPipeline::start_trace` (returns `Trace`) and the sink internally.

**Behavior change (intended):** agent-turn and retrieval records now actually land in
`data/telemetry/metrics.db` in production when a trace is active, where previously they never fired.

## Affected Interfaces

- [x] `everevo-core::TelemetryPipeline` / `TelemetryStage` / `TelemetryEmitContext` / `TelemetryRecord` / `default_telemetry_pipeline` — new
- [x] `everevo-agent::AgentLoop::with_telemetry` — param `Arc<Telemetry>` → `Arc<TelemetryPipeline>`
- [x] `everevo-agent::MemoryStage::with_telemetry` — param `Arc<Telemetry>` → `Arc<TelemetryPipeline>`
- [x] `AppState::telemetry` → `telemetry_pipeline` (field rename), `AppState::build_memory_stage` now consumes `trace_id`

## Migration Path

Existing callers that constructed `AgentLoop`/`MemoryStage` with a raw `Arc<Telemetry>` must instead
pass the pipeline from `AppState` (`state.telemetry_pipeline.clone()`). The `Telemetry` type itself is
unchanged and still exported; it is now consumed by the pipeline rather than directly by producers.
