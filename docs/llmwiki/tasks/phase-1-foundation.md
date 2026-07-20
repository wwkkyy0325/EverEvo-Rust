# Phase 1: Foundation

**Goal:** Cargo workspace + Axum server + LLM Provider + SQLite + SSE streaming functional.

---

## Tasks

### 1.1 — Cargo Workspace Scaffold
- [ ] Create root `Cargo.toml` with `[workspace]` members
- [ ] Create all 9 internal crates (`everevo-core`, `everevo-llm`, `everevo-agent`, etc.)
- [ ] Establish inter-crate dependency graph
- [ ] Add key external dependencies (axum, tokio, sqlx, multi-llm, serde, uuid, etc.)
- [ ] `cargo build` succeeds from root
- **Verify:** `cargo build --workspace` compiles all crates without errors

### 1.2 — Configuration & Data Directory
- [ ] Implement `everevo-core::config` — AppConfig struct (data_dir, llm providers, server port)
- [ ] Default config: `~/.everevo/` as data root
- [ ] Config loading: env vars → config file → defaults
- [ ] Create data directory on startup if missing
- **Verify:** App starts and creates `~/.everevo/` with correct subdirectories

### 1.3 — Database Layer
- [ ] SQLx migrations: `sessions` table, `messages` table
- [ ] `everevo-db` crate: connection pool, models, basic CRUD queries
- [ ] SQLite as default backend
- [ ] `sqlx::migrate!()` auto-run on startup
- **Verify:** `cargo test` for db crate — create session, add message, query history

### 1.4 — LLM Provider Integration
- [ ] `everevo-llm` crate: `LlmProvider` trait (chat, chat_stream)
- [ ] Anthropic provider via multi-llm
- [ ] OpenAI provider via multi-llm
- [ ] Provider selection from config
- [ ] Basic chat test (no tools yet): send message, get response
- **Verify:** Unit test — call Anthropic/OpenAI, receive response text

### 1.5 — Axum Server + SSE Streaming
- [ ] `everevo-server`: Axum app setup with CORS
- [ ] `GET /health` — returns server status
- [ ] `POST /api/chat` — accepts `{session_id, message}`, streams response via SSE
- [ ] Request/response models in `everevo-core`
- [ ] AppState with LlmProvider + DbPool shared via Axum state
- **Verify:** `curl -N POST /api/chat -d '{"message":"hello"}'` streams tokens in real-time

### 1.6 — Frontend Scaffold
- [ ] Vite + React + TypeScript project in `frontend/`
- [ ] Tailwind CSS setup
- [ ] Basic App shell with routing
- [ ] API client module (fetch wrapper)
- [ ] `useChat` hook — SSE streaming with ReadableStream
- [ ] Bare-bones chat component (input + message list)
- **Verify:** `npm run dev` → browser → type message → see streamed LLM response

---

## Testing Requirements (per [testing-strategy.md](../testing-strategy.md))

After each task, add tests at the appropriate layer before marking it complete:

| Task | L1 (unit) | L2 (mock agent) | L3 (integration) |
|------|-----------|-----------------|-------------------|
| 1.1 Workspace | `cargo build` succeeds | — | — |
| 1.2 Config | `test_config_defaults`, `test_data_dir` | — | — |
| 1.3 Database | — | — | `test_create_session`, `test_add_message` |
| 1.4 LLM Provider | `test_mock_basic_text` | `test_mock_tool_call_then_text` | `test_real_provider_responds` (L4) |
| 1.5 Server | — | — | `curl /health` returns 200 |
| 1.6 Frontend | — | — | Browser renders, SSE tokens appear |
