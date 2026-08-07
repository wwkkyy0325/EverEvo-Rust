# Architecture Audit — 2026-08-06

## 1. Crate Dependency & Layering

### Dependency Graph

```
everevo-core  ←──  12 crates (foundation — zero workspace deps) ✅

everevo-agent ←──  everevo-core, everevo-db, everevo-sandbox,
                   everevo-vector, everevo-mcp, everevo-knowledge,
                   everevo-downloader, everevo-workflow (8 deps)

everevo-server ←── everevo-core, everevo-db, everevo-sandbox,
                   everevo-vector, everevo-mcp, everevo-agent,
                   everevo-a2a, everevo-bootstrap (8 deps)
```

**Verdict: ✅ Clean DAG layering. No circular dependencies.**

Confirmed by full Cargo.toml audit — all `path = "../everevo-*"` edges point from higher to lower layers.

### 🔴 Concern: `axum` in `everevo-core`

`everevo-core/Cargo.toml` declares `axum` as a dependency solely for `impl IntoResponse for ApiError` in `error.rs`. This pulls axum (and transitively `hyper`, `tower`, `sync_wrapper`, `http-body`) into **every** dependent crate — including `everevo-db`, `everevo-sandbox`, `everevo-vector`, `everevo-mcp`, `everevo-workflow` — none of which serve HTTP.

**Impact:** 5+ crates compile a full web framework they don't use. **Recommendation:** Either:
- Move `IntoResponse` impl to `everevo-server` (requires `ApiError` in server, but we already have `pub use`)
- Or feature-gate: `axum = { workspace = true, optional = true }` behind an `http-error` feature

### ⚠️ Unused workspace dependency: `pin-project-lite`

Declared in root `Cargo.toml` `[workspace.dependencies]` but imported by zero crates. Dead weight.

- `everevo-core` depends on zero workspace crates — correct foundation
- `everevo-agent` is the integration layer (8 deps) — expected
- `everevo-server` is the top-level aggregator (8 deps) — expected

### ⚠️ Concern: everevo-agent re-exports everevo-knowledge

`everevo-agent/src/lib.rs:17`: `pub use everevo_knowledge as knowledge;`

This lets consumers do `use everevo_agent::knowledge::*` — bypassing the standalone `everevo-knowledge` crate. Creates unnecessary coupling and confuses the dependency graph. **Recommendation:** Server should depend on `everevo-knowledge` directly.

### Core module count: 14

Each module is used by 2+ crates (cross-cutting). `slash_command` could move to server (only used there), but low priority.

---

## 2. Coupling Assessment

### 🔴 God Object: AppState (1029 lines, 40+ public fields)

| Problem | Detail |
|---------|--------|
| 40+ fields | Spans 10+ crate types — Config, LLM, Sandbox, MCP, Fact, Diary, Dreaming, KnowledgeGraph, Telemetry, Skills, Commands, Workspace, A2A |
| Monolithic init | `new()` spans ~200 lines initializing ALL subsystems at once |
| Tight coupling | Server directly accesses `everevo_agent::memory::FactManager`, `everevo_agent::knowledge::KnowledgeGraph` internals |

**Recommendation:** Extract focused subsystems (not a rewrite — extract method + move fields):
- `LlmState` — clients map + notify + model registry
- `SandboxState` — sandbox lifecycle + confirmations + permission levels
- `MemoryState` — fact_manager, diary_manager, scheduler, dreaming_engine, wiki_generator
- `OrchestrationState` — session_actors, subagent_handles, bg_sessions, context_snapshots

### ⚠️ Server → Agent internal module access (9 modules)

Server directly imports from these `everevo_agent::` paths:
- `memory` (FactManager, DiaryManager, DreamingEngine, Scheduler)
- `knowledge` (KnowledgeGraph)
- `skill` (SkillRegistry)
- `subagent_context`, `subagent_pool`
- `tools` (TodoStore)
- `build_character_block`, `load_character`, `synthesize_character`

**Not wrong but fragile.** Recommendation: Declare a stable integration facade in `everevo_agent/src/lib.rs` that re-exports these with doc comments labeling them as "Server integration surface."

---

## 3. Async Patterns

| Pattern | Count | Assessment |
|---------|-------|------------|
| `std::fs::*` | ~260 total; ~80 on hot paths | ⚠️ FactManager/DiaryManager/LlmwikiManager use blocking I/O in async call chains |
| `tokio::spawn` | 34 | ✅ Fire-and-forget with backpressure (semaphore in team.rs) |
| `tokio::sync::Mutex` | 17 | ✅ Correctly used where locks span `.await` (MCP clients) |
| `std::sync::Mutex/RwLock` | ~98 | ✅ No instances found held across `.await` — this anti-pattern is **absent** |

### 🔴 std::fs blocking on hot async paths (5 confirmed sites)

These synchronous I/O calls run on the tokio runtime worker threads:

| Location | Context | Impact |
|----------|---------|--------|
| `FactManager::save()` → `std::fs::write` + `regenerate_index()` | Called from `extract_from_turn()`, `reflect_on_turn()`, `execute_deep()`, `memory_tool::add()` — all async | Blocks runtime thread for each fact save + index write |
| `DreamingEngine::write_themes()` → `std::fs::write` | Called from `async fn execute_rem()` | Write per REM phase |
| `DreamingEngine::read_themes()` → `std::fs::read_to_string` | Called from `async fn execute_deep()` | Read per DEEP phase |
| `delegate/mod.rs` → `std::fs::create_dir_all` + `std::fs::write` | Called from sync `dispatch_one()` which is called from `async fn execute()` | Telemetry writes block runtime |
| `spawn.rs` → `std::fs::create_dir_all` + `std::fs::write` | Inside `async fn spawn_single()` | Sub-agent telemetry persistence |

**Assessment:** Small-file I/O with `std::fs` is common Rust practice. Currently not broken under normal load, but:

- `spawn_blocking` for diary/wiki generation (>10ms latency)
- `spawn_blocking` or a dedicated thread for MDINDEX fact regeneration
- Telemetry writes (delegate/spawn.rs) can stay blocking — they're fire-and-forget anyway

### ✅ No Mutex-across-await bugs — **confirmed absent**

Full audit: all `std::sync::Mutex`/`RwLock` guards are short-lived. `tokio::sync::Mutex` correctly used where guards survive `.await`. `RwLock::read()` used in async MCP health checker (`app_state.rs:330`) is a minor concern — `std::sync::RwLock` can block if a writer holds the lock.

### ⚠️ Fire-and-forget spawns (26 with no JoinHandle)

| Location | Risk |
|----------|------|
| `post_turn.rs` ×4 | Memory extraction, reflection — panics are silent but don't crash main loop |
| `workflow.rs:295` | Spawn in a loop — unbounded concurrency if many tasks |
| `bootstrap.rs:75` | Init pipeline runner |
| `facts.rs:181` | Fact SQLite indexing fallback |

Panics in these tasks kill the task but NOT the process (tokio catches spawn panics by default). However, the `post_turn.rs` spawns have no error recovery — if memory extraction panics, the turn's reflection is silently lost.

### ⚠️ Missing timeouts (4 sites)

| Location | Risk |
|----------|------|
| MCP client `lock().await` | Hung MCP server → permanent lock |
| `semaphore.acquire_owned()` in team dispatch | Hung tasks → dispatch waits forever |
| SQLx queries in dreaming pipeline | Slow DB → blocks progress |
| WebSocket sends in browser_bridge | Hung write → indefinite block |

### ✅ tokio::spawn patterns

- Agent loop: fire-and-forget, result via channel (now with catch_unwind)
- Sub-agents: fire-and-forget, result via backlog + pending decrement
- Semaphore permits (`_permit`) held correctly until task completes
- Disconnect watchdog: spawned once per session

---

## 4. Design Patterns — Consistency Scorecard

| Pattern | Status | Notes |
|---------|--------|-------|
| Error handling (REST) | ✅ Consistent | ApiError envelope on all 14 route modules |
| Error handling (SSE) | ✅ Consistent | AgentEvent::Error via catch_unwind boundaries |
| Panic defense | ✅ Consistent | Dual catch_unwind at agent-loop + chat-handler |
| Tool registration | ✅ Single entry | orchestration/tools.rs, now documented |
| Context pipeline | ✅ Clean | ContextStage trait + priority ordering |
| State management | ⚠️ Mixed | AppState (global) vs SessionCoordinator (per-session) — split is correct but undocumented |
| Channel patterns | ✅ Consistent | mpsc for streaming, broadcast for events, oneshot for confirmations |
| Configuration | ✅ Consistent | AppConfig + env vars + file config |

---

## 5. Recommendations (Priority-Ordered)

### 🔴 High Priority

1. **Move `axum` out of `everevo-core`** — `IntoResponse` impl forces axum into 5+ non-HTTP crates. Feature-gate or move to server.

2. **`spawn_blocking` for fact I/O** — `FactManager::save()` and `regenerate_index()` use `std::fs` on async runtime threads. Move to `spawn_blocking`.

3. **Document Mutex discipline** — Add to CONTRIBUTING.md: "Never hold `std::sync::Mutex` across `.await`." (No bugs found — preventative.)

### 🟡 Medium Priority

4. **Split AppState** — Extract `LlmState`, `SandboxState`, `MemoryState`, `OrchestrationState`.

5. **Add timeouts** — 4 sites: MCP lock acquire, semaphore in team dispatch, SQLx in dreaming, WS sends in browser_bridge.

6. **Add backpressure to workflow spawns** — `workflow.rs:295` spawns in a loop; cap at semaphore or use JoinSet.

7. **`spawn_blocking` for `std::sync::RwLock` read in MCP health checker** — `app_state.rs:330` reads a std::sync::RwLock in an async task — could block runtime.

### 🟢 Low Priority

6. **Test coverage** — Add unit tests for route handlers. Add property-based tests (`proptest`) for parsers.

7. **Benchmark suite** — `criterion` for: tool execution, LLM streaming latency, fact save/load, context assembly.

8. **Core split planning** — If `everevo-core` exceeds 20 modules, split into `everevo-core` (types + traits) + `everevo-common` (implementations).
