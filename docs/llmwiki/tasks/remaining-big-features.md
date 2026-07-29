# Remaining Big Features — Design & Implementation Plans

> Written: 2026-07-27 | Status: planning

## 1. MCP Auto-Reconnect

### Status: ✅ IMPLEMENTED

Background tokio task in `AppState` checks MCP health every 60 seconds.
Dead stdio processes are re-spawned; dead HTTP connections are re-established.
Config is read from `state.config.mcp_servers` for each reconnect attempt.

**Key files:**
- `crates/everevo-server/src/app_state.rs` — `spawn_mcp_health_checker()`
- `crates/everevo-mcp/src/client.rs` — `is_alive()`, `connect_stdio()`, `connect_http()`

---

## 2. Agent Teams / Coordinator Mode

### Design

Multi-agent coordination where a "coordinator" agent decomposes complex tasks
and dispatches them to specialized sub-agents, then synthesizes results.

#### Architecture

```
User → Coordinator Agent → [SubAgent Pool]
                │                │
                ├─ Reviewer      ├─ code review
                ├─ Researcher    ├─ investigate codebase
                ├─ Coder         ├─ implement changes
                └─ Tester        └─ write/run tests
                         │
                ← Synthesis ←
```

#### Implementation Plan

| Phase | Task | Effort | Files |
|-------|------|--------|-------|
| 1 | `CoordinatorMode` config in `AgentLoopConfig` | S | `loop_/mod.rs` |
| 2 | `TeamRole` enum: Reviewer, Researcher, Coder, Tester | S | `tools/builtins/teams.rs` |
| 3 | `TeamTool` — dispatches to N sub-agents in parallel | M | `tools/builtins/teams.rs` |
| 4 | Role-specific prompt templates (per `TeamRole`) | S | `tools/builtins/teams.rs` |
| 5 | Result synthesis — coordinator merges N sub-agent outputs | M | `loop_/mod.rs` |
| 6 | Progress tracking — per-role status in frontend | M | `routes/`, `frontend/` |

**Total effort: ~3-5 sessions**

#### Key Design Decisions

1. **Build on existing infrastructure** — reuse `TaskTool`, `SubAgentContext`, `AgentLoop::run_subagent()`
2. **Roles are prompt templates, not separate code paths** — each role gets a specialized system prompt; same agent loop
3. **Parallel by default** — all team members run concurrently; results collected via `mpsc::channel`
4. **Synthesizer is the coordinator agent itself** — after all results arrive, coordinator makes one more LLM call with results injected as context

---

## 3. Workflow JS Engine

### Design

Execute user-defined JavaScript workflows via an embedded JS runtime.
Workflows define multi-step automation: "when X happens, do Y, then Z".

#### Architecture

```
User defines workflow.js
        │
        ▼
WorkflowEngine (Rust)
  ├─ deno_core runtime (V8 isolate)
  ├─ API surface: shell(), fetch(), memory(), agent()
  └─ Sandboxed: no fs, no net (except through API)
```

#### Implementation Plan

| Phase | Task | Effort | Files |
|-------|------|--------|-------|
| 1 | Evaluate deno_core vs boa vs quickjs | Research | — |
| 2 | Add `deno_core` dependency, create `WorkflowRuntime` | M | `crates/everevo-workflow/` |
| 3 | Define JS API: `shell(cmd)`, `fetch(url)`, `memory.save(...)` | M | `workflow/js_api.rs` |
| 4 | `WorkflowEngine::execute(script)` — run JS, collect results | M | `workflow/engine.rs` |
| 5 | `WorkflowTool` integration — LLM can run workflows as a tool | S | `tools/builtins/workflow_js.rs` |
| 6 | Safety: op sanitization, timeout, memory limit | M | `workflow/safety.rs` |
| 7 | Frontend: workflow editor + run button | L | `frontend/` |

**Total effort: ~5-8 sessions**

#### Key Design Decisions

1. **deno_core** (not quickjs/boa) — most mature, same engine as Deno, well-documented embed API
2. **Op-based API** — JS calls ops (Rust functions) for all I/O; no direct fs/net access
3. **Separate crate** — `everevo-workflow` to avoid bloating agent/server with V8
4. **No eval from LLM** — only user-authored workflows; LLM can call `Workflow` tool but not inject code
5. **Output is structured JSON** — workflows return typed results, not raw text

---

## 4. Daemon / Background Sessions

### Design

Long-running agent sessions that persist across frontend connections.
The user starts a task, closes the browser, and comes back later to results.

#### Architecture

```
┌─────────────────────────────────────────┐
│              AppState                     │
│  ┌─────────────────────────────────┐    │
│  │  ActiveSessions (in-memory)      │    │
│  │  ┌───────┐ ┌───────┐ ┌───────┐ │    │
│  │  │ Sess1 │ │ Sess2 │ │ Sess3 │ │    │
│  │  │ active│ │bg     │ │bg     │ │    │
│  │  └───────┘ └───┬───┘ └───┬───┘ │    │
│  │                │         │      │    │
│  │         ┌──────▼─────────▼──┐   │    │
│  │         │ BackgroundWorker  │   │    │
│  │         │ (tokio task pool) │   │    │
│  │         └──────────────────┘   │    │
│  └─────────────────────────────────┘    │
└─────────────────────────────────────────┘
         │                    │
    SSE (active)      Poll (background)
```

#### Implementation Plan

| Phase | Task | Effort | Files |
|-------|------|--------|-------|
| 1 | `SessionMode` enum: `Interactive` / `Background` | S | `core/types.rs` |
| 2 | Background worker pool — N concurrent daemon sessions | M | `server/background.rs` |
| 3 | Session state machine: Idle → Running → WaitingUser → Complete | M | `server/session_state.rs` |
| 4 | SSE reconnection — resume existing stream on reconnect | M | `routes/chat.rs` |
| 5 | `/api/sessions/{id}/status` — poll endpoint for background sessions | S | `routes/session_routes.rs` |
| 6 | Frontend: session list shows active/bg status, poll indicator | M | `frontend/` |
| 7 | Notification on completion — push to frontend via SSE | S | `routes/chat.rs` |

**Total effort: ~4-6 sessions**

#### Key Design Decisions

1. **Same agent loop, different output target** — `AgentLoop::run()` already uses channels; background sessions write to a buffered channel that's drained on SSE reconnect
2. **One session, one database row** — no new tables; session status column tracks `interactive`/`background`/`completed`
3. **No separate process** — background sessions run as tokio tasks in the same server process (simpler deployment, shared DB pool)
4. **Result persistence** — all messages saved to DB in real-time (already implemented); SSE reconnection replays from DB

---

## Implementation Priority

| Rank | Feature | Reason |
|------|---------|--------|
| 1 | **MCP Auto-Reconnect** | ✅ DONE — simple, high UX impact |
| 2 | **Agent Teams** | Medium effort, builds on existing sub-agent infrastructure |
| 3 | **Daemon Sessions** | Medium effort, leverages existing DB + SSE architecture |
| 4 | **Workflow JS Engine** | Largest effort, requires new crate + V8 embedding |

### Next Step

Start Agent Teams implementation — it has the best effort-to-impact ratio and
builds directly on the sub-agent system already in place.
