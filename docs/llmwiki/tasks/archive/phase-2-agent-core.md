# Phase 2: Agent Core
> **状态**:✅ 已完成(归档)— 阶段 2 计划,代码已落地;未勾选项如仍需跟进请新建任务

---


**Goal:** ReAct loop, tool calling, sandbox isolation, session memory. Agent can use tools autonomously.

---

## Tasks

### 2.1 — ReAct Agent Loop
- [ ] `everevo-agent::loop`: ReActLoop struct
- [ ] System prompt builder (inject tool schemas, session context)
- [ ] Loop: LLM call → parse tool_calls → execute → append results → repeat
- [ ] Max iterations guard, stop conditions
- [ ] Stream agent thinking as SSE events: `thinking`, `tool_call`, `tool_result`, `response`
- **Verify:** Agent calls a tool, gets result, continues reasoning, returns final answer

### 2.2 — Tool Registry + Built-in Tools
- [ ] `everevo-tools`: `Tool` trait + `ToolRegistry`
- [ ] JSON Schema generation from Tool definitions (for LLM function calling)
- [ ] Built-in: `web_search` (DuckDuckGo or Bing API)
- [ ] Built-in: `web_fetch` (reqwest + HTML-to-text)
- [ ] Built-in: `file_read` / `file_write` (sandboxed filesystem)
- [ ] Built-in: `shell` (delegates to sandbox executor)
- **Verify:** Agent uses `web_search` tool when asked a question needing current info

### 2.3 — Sandbox Executor
- [ ] `everevo-sandbox`: `TieredSandbox` struct
- [ ] `WasmSandbox` — wasmtime engine, execute code snippets
- [ ] `DockerSandbox` — bollard client, container management
- [ ] `FsIsolator` — per-session temp dirs, path allowlisting
- [ ] Auto-routing based on Tool's `risk_level`
- [ ] Timeout enforcement, resource limits
- **Verify:** `shell("echo hello")` runs in Docker container, result returned

### 2.4 — Session Manager
- [ ] `everevo-agent::session`: SessionManager
- [ ] Create/list/delete sessions (SQLite backed)
- [ ] Message CRUD within session
- [ ] Session title auto-generation (first user message)
- [ ] `GET /api/sessions`, `POST /api/sessions`, `GET /api/sessions/:id`
- **Verify:** Create session, chat, close browser, reopen — history intact

### 2.5 — Memory & Context Window
- [ ] `everevo-agent::memory`: MemoryManager
- [ ] Token counting (tiktoken-rs or equivalent)
- [ ] Context window awareness: track token usage, detect overflow
- [ ] Sliding window: keep last N messages within token budget
- [ ] Summarization: when history exceeds threshold, call LLM to compress older messages
- [ ] Inject summary + recent messages into each LLM call
- **Verify:** 50-message conversation stays within context window, agent remembers key facts

### 2.6 — E2E Agent Test
- [ ] Integration test: "Search the web for latest Rust release, save it to a file"
- [ ] Agent calls web_search → receives results → calls file_write → returns confirmation
- **Verify:** Full tool-use chain works in a single conversation turn

### 2.7 — Sandbox Trait (from Audit)
- [ ] Define `Sandbox` trait in `everevo-core` (pattern: same as `Tool` trait)
- [ ] `TieredSandbox` implements `Sandbox` with three-tier routing
- [ ] `ShellTool` calls `sandbox.execute()` instead of spawning directly
- **Verify:** ShellTool's execute method uses sandbox, not `tokio::process::Command` directly

### 2.8 — Tracing Instrumentation (from Audit)
- [ ] Add `#[tracing::instrument]` to all key async functions:
  - `ShellTool::execute` / `DownloadTool::execute` / `BootstrapTool::execute`
  - `Database` CRUD methods
  - `execute_task`, `download_chunked`, `download_simple`
  - `cmd_serve` / `cmd_bootstrap`
- **Verify:** `RUST_LOG=debug` shows structured span hierarchy for a chat request

### 2.9 — Server Integration Tests (from Audit)
- [ ] Test for health endpoint returns `200 + status: "ok"`
- [ ] Test for `build_app()` returns a Router with all routes
- **Verify:** `cargo test -p everevo-server` passes

### 2.10 — Audit: Trait Abstractions (from [audit-2026-07-18](../../audit-2026-07-18.md))
- [ ] Define `DownloadProvider` trait in `everevo-core`
- [ ] Define `BootstrapProvider` trait in `everevo-core`
- [ ] Define `ShellProvider` trait in `everevo-core`
- [ ] Convert `AppState` fields to `Arc<dyn Trait>` for testability
- [ ] Implement traits on concrete types in their respective crates
- **Verify:** Each tool can be instantiated with a mock backend in tests

### 2.11 — Audit: Security Hardening (from [audit-2026-07-18](../../audit-2026-07-18.md))
- [ ] Path traversal validation in `DownloadTool` (LLM-provided dest_path)
- [ ] Sandbox isolation in `ShellTool` (command timeout + working dir restriction)
- [ ] Content-length limit on `/api/chat` endpoint (DoS prevention)
- [ ] Redact secrets from shell command logging
- **Verify:** Malicious inputs (../ paths, huge payloads) rejected gracefully

### 2.12 — Audit: God-Function Refactor (from [audit-2026-07-18](../../audit-2026-07-18.md))
- [ ] Extract `download_handler` orchestration into `BootstrapProvisioner` in bootstrap crate
- [ ] Route handler becomes thin SSE mapping layer (<50 lines)
- **Verify:** Bootstrap download flow works identically via SSE after refactor

### 2.13 — Audit: Performance (from [audit-2026-07-18](../../audit-2026-07-18.md))
- [ ] Add `BufWriter` to download file I/O
- [ ] Add cancellation propagation to chunk download workers
- [ ] Replace `std::sync::Mutex` with `tokio::sync::Mutex` in `MockLlmProvider`
- **Verify:** Download throughput improved; chunk failure cancels sibling workers