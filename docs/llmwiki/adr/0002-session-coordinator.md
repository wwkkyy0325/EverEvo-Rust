# ADR-0002: Centralize Per-Session Data-Flow via SessionCoordinator

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

Each chat session required 8+ shared data structures (mpsc channels, cancellation tokens, sub-agent handles, status lists, backlog queues, compact focus, pending counters, confirmation channels). These were scattered across `AppState`, handler locals, and tool constructors — making it unclear which piece of state belonged to which session and who was responsible for cleanup.

Sub-agent orchestration (TaskTool, WorkflowTool, TeamTool) required shared access to the backlog and pending counter, but these were wired through individual `Arc` clones passed to each tool constructor.

## Decision

Create `SessionCoordinator` in `everevo-server/src/orchestration/session_coordinator.rs`:

```rust
pub struct SessionCoordinator {
    pub session_id: Uuid,
    pub tx_sse: Sender<Result<Event, Infallible>>,
    pub confirm_tx: UnboundedSender<ConfirmationNotification>,
    pub pending: Arc<AtomicUsize>,
    pub backlog: Arc<Mutex<Vec<(String, String, String)>>>,
    pub handles: Arc<Mutex<Vec<SubAgentHandle>>>,
    pub statuses: Arc<Mutex<Vec<SubAgentStatus>>>,
    pub compact_focus: Arc<Mutex<Option<String>>>,
    pub cancel: CancellationToken,
}
```

One struct owns all per-session primitives. Tools receive shared counters via `coord.pending.clone()` and `coord.backlog.clone()`.

## Consequences

**Easier:**
- Single struct to inspect for per-session state (debugging, testing)
- Cleanup is deterministic — drop the coordinator, everything unsubscribes
- Adding new per-session state is one field, not N scattered variables
- Sub-agent tools no longer need individual wiring; they clone from `coord`

**Harder:**
- More `Arc::clone()` calls in hot paths (negligible cost)
- All tools must accept the coordinator pattern (already done)

## Alternatives Considered

1. **Scattered `Arc`s in `AppState`** — What we had; state ownership unclear
2. **Actor model (Actix)** — Overkill for single-threaded agent loop
3. **`tokio::sync::broadcast` bus** — Would lose type safety and ordering guarantees of dedicated channels
