# ADR-0003: Add catch_unwind at Agent-Loop and Chat-Handler Spawn Sites

- **Status:** Accepted
- **Date:** 2026-08-06

## Context

Tool execution runs inside `tokio::spawn` tasks at two levels:

1. **AgentLoop::run()** — spawns the main agent loop; tool panics kill the spawned task
2. **Chat handler** — spawns `handle_chat()`; panics kill the SSE stream

Before this change, a single `unwrap()` panic in any tool (e.g., Mutex poison in delegate.rs, semaphore close in team.rs) would silently kill the `tokio::spawn` task. The SSE channel would close with no error event, producing "typeerror error in input stream" on the frontend.

There were no `catch_unwind` calls anywhere in the tool execution, event streaming, or chat handler path.

## Decision

Add two `catch_unwind` boundaries using `AssertUnwindSafe` + `futures::FutureExt::catch_unwind()`:

1. **AgentLoop::run()** — wraps `run_loop()` so tool panics become `AgentEvent::Error` messages
2. **Chat handler** — wraps `handle_chat()` so handler panics become SSE `error` events

Additionally, replace all `unwrap()`/`expect()` calls in production code (delegate.rs, team.rs, tools.rs, web_search.rs) with `unwrap_or_else(|e| e.into_inner())` for Mutex/RwLock, and `match` + `tracing::error!` for other fallible operations.

## Consequences

**Easier:**
- Tool failures never crash the main conversation — errors are reported as structured events
- Frontend "typeerror error in input stream" should no longer occur
- New tools can be less defensive about internal panics (still discouraged)
- Mutex/RwLock poison is now recoverable

**Harder:**
- `AssertUnwindSafe` is an assertion — we're promising the code is unwind-safe (which it is, since we own all types)
- `catch_unwind` adds minor overhead at spawn boundaries (two extra checks per session lifecycle)

## Alternatives Considered

1. **`catch_unwind` on every tool.execute() call** — Finer granularity but 22+ sites to change; the spawn-level boundary covers all tools
2. **`JoinHandle` monitoring** — Would detect panics but not recover the channel; panicked tasks still lose SSE connection
3. **Status quo** — Already causing production issues with "typeerror error in input stream"
