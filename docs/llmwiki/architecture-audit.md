# Architecture Audit

Audited 2026-07-17 against three reference frameworks:
- **ripgrep layered pattern** (Rust workspace best practices)
- **Hexagonal Ports-Adapter** (domain-driven design)
- **Agent orchestration papers** (Google ADK 8 patterns, FoA, Blackboard)

---

## Audit Results

### ✅ Passed (17 items)

| # | Check | Evidence |
|---|-------|----------|
| 1 | `everevo-core` zero IO deps | No tokio/sqlx/reqwest/axum imports |
| 2 | Dependency flow one-way | DAG: core ← db/downloader/bootstrap ← agent ← server |
| 3 | No circular dependencies | Verified by dependency matrix |
| 4 | `Tool` trait in core | `crates/everevo-core/src/tool.rs` |
| 5 | `LlmProvider` trait in core | `crates/everevo-core/src/llm.rs` |
| 6 | Workspace unified deps | `[workspace.dependencies]` in root Cargo.toml |
| 7 | `pub(crate)` default visibility | Minimal public API surface per crate |
| 8 | `#[deny(unsafe_code)]` | Workspace-level lint |
| 9 | Mock injection via traits | `MockLlmProvider` implements `LlmProvider` |
| 10 | 4-layer test pyramid | L1 (unit) → L2 (mock agent) → L3 (integration) → L4 (real LLM) |
| 11 | SQLite :memory: for DB tests | Zero filesystem footprint |
| 12 | Async throughout | Tokio-based, async_trait for provider/tool traits |
| 13 | CancellationToken support | Downloader supports graceful cancellation |
| 14 | Semaphore-based concurrency | Downloader chunk workers |
| 15 | Observability via tracing | Structured logging with EnvFilter |
| 16 | Thin binary entry | `main.rs` → config → bootstrap check → build app → serve |
| 17 | Configuration via env vars | `AppConfig::load()` with env override |

### ⚠️ Fixed During Audit (2 items)

| # | Was | Now |
|---|-----|-----|
| 1 | `LlmProvider` trait defined in `everevo-agent` | Moved to `everevo-core/src/llm.rs` |
| 2 | `ShellTool` used blocking `std::process::Command` in async fn | Changed to `tokio::process::Command` |

### ⬜ Deferred (non-blocking for Phase 1-2)

| # | Item | Severity | When |
|---|------|----------|------|
| 1 | No `Sandbox` trait — `ShellTool` bypasses sandbox abstraction | Medium | Phase 2 |
| 2 | No structured `#[tracing::instrument]` spans on key async fns | Medium | Phase 2 |
| 3 | `AppConfig` env-only, no file config support | Low | Post-Phase 1 |
| 4 | No OpenAPI/Swagger for API routes | Low | Phase 4 |
| 5 | No rate limiting on chat endpoint | Low | Phase 4 |
| 6 | No `Agent` trait — multi-agent orchestration blocked | Medium | Phase 3 |
| 7 | `everevo-agent` becoming "god crate" (depends on all 4 crates) | Medium | Phase 3 (split plan below) |

---

## Agent Crate Split Plan (Phase 3)

When `everevo-agent` exceeds ~25 source files, split into:

```
crates/everevo-agent/          # Agent loop + session + memory (core orchestration)
crates/everevo-tools/          # All built-in tool impls (currently in agent/tools/builtins)
crates/everevo-sandbox/        # TieredSandbox + WASM + Docker executors
crates/everevo-kg/             # Knowledge graph (Oxigraph wrapper)
crates/everevo-rag/            # RAG pipeline (LanceDB + fastembed)
crates/everevo-llmwiki/        # llmwiki manager
```

**Trigger:** split when any module exceeds 500 lines or needs a different dependency profile.

---

## Multi-Agent Readiness

The current architecture supports ADK's orchestration patterns:

| Pattern | EverEvo Support |
|---------|----------------|
| Sequential | ✅ `Tool` trait + `ToolRegistry` = agent-as-tool |
| Coordinator/Dispatcher | ✅ LLM routing via tool descriptions |
| Parallel Fan-Out | ⬜ Needs `ParallelAgent` wrapper (Phase 3) |
| Generator-Critic Loop | ⬜ Needs `LoopAgent` with exit condition |
| Blackboard | ⬜ KG already serves as shared state, missing event subscription |

**Missing key abstraction:** `Agent` trait. Currently only `Tool` trait exists. An agent wrapping another agent requires agents to implement a common interface. This is deferred to Phase 3 when multi-agent orchestration becomes a requirement.

---

## Security Posture

| Control | Status |
|---------|--------|
| `#[deny(unsafe_code)]` | ✅ |
| Shell command isolation | ⚠️ Sandbox bypassed in Phase 1, TieredSandbox in Phase 2 |
| Dependency audit | ⬜ `cargo-deny` + `cargo-audit` not yet in CI |
| Input validation | ⚠️ Basic per-tool, no framework |
| Rate limiting | ⬜ Not implemented |

---

## Development Rules (from issues encountered)

### Database: SQLite path handling on Windows

**Rule:** `Database::connect()` takes `&Path`, not a URL string.

**Why:** `sqlite://` URL construction breaks on Windows paths with backslashes and drive letters.
`SqliteConnectOptions::new().filename(path)` handles all platforms natively.

```rust
// ✅ Correct
Database::connect(&db_path).await

// ❌ Never do this on Windows
let url = format!("sqlite://{}", path.display()); // fails with "unable to open database file"
Database::connect(&url).await
```

### Config: `create_dir_all` must propagate errors

**Rule:** Never use `.ok()` on `create_dir_all` — use `?` or `map_err`.

**Why:** Silent directory creation failures surface as confusing downstream errors
("unable to open database file" vs "failed to create data/db/: permission denied").

```rust
// ✅ Correct
std::fs::create_dir_all(&dir).map_err(|e| EverEvoError::Config(...))?;

// ❌ Never do this
std::fs::create_dir_all(&dir).ok(); // swallows the real error
```

### Server: LLM is optional

**Rule:** `AppState.llm` is `Option<Arc<LlmClient>>` — server starts without API keys.

**Why:** Bootstrap UI and health checks must work before any LLM credentials are configured.
The chat route returns a helpful message when `llm` is `None`.

### Start first, fix later

When a startup error occurs, check in this order:
1. Is `data/` and its subdirectories actually created?
2. Is the SQLite connection using `&Path`, not a URL string?
3. Are LLM API keys optional or required?

---

## Conclusion

**Phase 1-2 Ready.** The two critical findings (trait placement, blocking I/O) are fixed.
The 7 deferred items are all Phase 2-4 scope. No architectural debt that blocks current development.
