# Changelog

All notable changes to EverEvo-Rust. Append-only, newest first.

---

## 2026-07-21 — Frontend Redesign: Theme System + Component Architecture

**What:** Comprehensive frontend overhaul — migrated to Tailwind CSS v4, built CSS variable design token system (OKLCH), integrated shadcn/ui component library, implemented 4-theme multi-theme system, and refactored component architecture.

**Phase 1 — Design Token Foundation + Tailwind v4:**
- Migrated Tailwind CSS v3.4 → v4.1 (CSS-first `@theme`, OKLCH, 5× faster builds)
- Removed `tailwind.config.js`, `postcss.config.js` (no longer needed)
- Defined 40+ CSS custom properties in OKLCH across `:root` (light) and `.dark` (dark)
- Created `ThemeProvider` React Context + `localStorage` persistence + system preference detection
- Added `ThemeToggle` with sun/moon icons in nav bar
- Replaced ALL hardcoded colors across 8 components with semantic tokens

**Phase 2 — shadcn/ui Component Library:**
- Integrated shadcn/ui (new-york style) with Tailwind v4 compatibility
- Created base components: `Button`, `Input`, `Card`, `Badge`, `Separator`
- Added utilities: `cn()` (clsx + tailwind-merge), `class-variance-authority`
- Path aliases configured: `@/*` → `./src/*`

**Phase 3 — Multi-Theme System:**
- 4 color themes: `default` (blue), `ocean` (teal), `sunset` (orange), `forest` (green)
- Each theme × dark/light = 8 visual combinations, independent axes
- `ThemeSelector` dropdown component with color preview dots
- All shadcn/ui + app components auto-adapt to theme changes

**Phase 4 — Component Architecture:**
- Extracted reusable components: `ChatBubble`, `ToolCallCard`, `ThinkingPanel`
- `ChatView` refactored to use shadcn `Button` + `Input`
- New directory structure: `components/ui/` (shadcn), `components/chat/`, `components/layout/`
- `ToolCallCard` now has expand/collapse with tool-specific color coding

**Files affected (new):**
- `frontend/src/index.css` — design token system + Tailwind v4 theme mapping
- `frontend/src/hooks/useTheme.tsx` — ThemeProvider + two-axis theme system
- `frontend/src/components/ThemeToggle.tsx` — dark/light toggle
- `frontend/src/components/ThemeSelector.tsx` — color theme picker
- `frontend/src/components/ui/button.tsx` — shadcn Button (cva variants)
- `frontend/src/components/ui/input.tsx` — shadcn Input
- `frontend/src/components/ui/card.tsx` — shadcn Card family
- `frontend/src/components/ui/badge.tsx` — shadcn Badge
- `frontend/src/components/ui/separator.tsx` — shadcn Separator
- `frontend/src/components/chat/ChatBubble.tsx` — reusable message bubble
- `frontend/src/components/chat/ToolCallCard.tsx` — expandable tool call display
- `frontend/src/components/chat/ThinkingPanel.tsx` — thinking process panel
- `frontend/src/lib/utils.ts` — cn() utility
- `frontend/components.json` — shadcn/ui configuration

**Files affected (modified):**
- `frontend/package.json` — updated deps (Tailwind v4, clsx, cva, tailwind-merge, lucide-react)
- `frontend/vite.config.ts` — @tailwindcss/vite plugin, path alias
- `frontend/tsconfig.json` — path alias config
- `frontend/src/main.tsx` — ThemeProvider wrapper
- `frontend/src/App.tsx` — semantic tokens, ThemeToggle + ThemeSelector
- `frontend/src/components/ChatView.tsx` — shadcn Button/Input, extracted sub-components
- `frontend/src/components/SessionSidebar.tsx` — semantic tokens
- `frontend/src/components/BootstrapView.tsx` — semantic tokens
- `frontend/src/components/SettingsView.tsx` — semantic tokens
- `frontend/src/components/AuditPanel.tsx` — semantic tokens
- `frontend/src/components/ConfirmDialog.tsx` — semantic tokens
- `frontend/src/components/MemoryPanel.tsx` — semantic tokens
- `frontend/src/components/DomainPanel.tsx` — semantic tokens

**Files removed:**
- `frontend/tailwind.config.js` — replaced by CSS-first `@theme`
- `frontend/postcss.config.js` — replaced by `@tailwindcss/vite` plugin

**Key design decisions:**
- CSS custom properties as single source of truth (not JS config)
- OKLCH color space for perceptual uniformity and native opacity
- shadcn/ui source-copy pattern (not npm black box) aligns with EverEvo "self-built" philosophy
- Two-axis theme system (color × brightness) = 8 independent visual combinations
- All shadcn components reference semantic tokens — theme-switching requires zero component changes

**Research-backed choices (deep web research on Hermes, ClawX, local-ai, shadcn/ui ecosystem):**
- Tailwind v4 + shadcn/ui is the 2025 consensus stack for AI chat applications
- Three-tier token architecture (global → semantic → component) is the W3C DTCG standard
- `data-theme` attribute pattern scales to N themes without custom variants
- OKLCH recommended over HSL for perceptually uniform shade scales

**Task doc:** [docs/llmwiki/tasks/frontend-redesign-theme-system.md](docs/llmwiki/tasks/frontend-redesign-theme-system.md)

---

## 2026-07-19 — Security Hardening, Coupling Fix, File Splitting, Phase 2/3

**What:** Fixed 2 security issues (ZIP Slip defense, CORS tightening), removed stale `everevo-agent` dependency from domain crate, split all 5 files >800 lines into focused sub-modules, added `Agent` trait to core, replaced `std::sync::Mutex` with `tokio::sync::Mutex` in MockLlmProvider, and added proper error variants (`Bootstrap`, `Download`) with `From` impls.

**Files affected:**
- `crates/everevo-core/src/error.rs` — added `Bootstrap`, `Download` variants
- `crates/everevo-core/src/agent.rs` — new `Agent` trait + `AgentContext` + `AgentOutput`
- `crates/everevo-core/src/lib.rs` — export `agent` module
- `crates/everevo-server/src/main.rs` — ZIP Slip defense in `extract_zip()`
- `crates/everevo-server/src/lib.rs` — CORS restricted to `EVEREVO_CORS_ORIGINS` env var
- `crates/everevo-domain/Cargo.toml` — removed unused `everevo-agent` dependency
- `crates/everevo-agent/src/llm.rs` — `MockLlmProvider` uses `tokio::sync::Mutex`
- `crates/everevo-bootstrap/src/lib.rs` — `From<BootstrapError>` uses `Bootstrap` variant
- `crates/everevo-downloader/src/error.rs` — added `From<DownloadError> for EverEvoError`
- Split: `everevo-sandbox/src/permission.rs` → `permission/{mod,level,paths,patterns,rules}.rs`
- Split: `everevo-vector/src/lib.rs` → `{types,embedding,store_trait,memory_store,lancedb_store,persistent,engine}.rs`
- Split: `everevo-domain/src/lib.rs` → `{registry,document,classifier,parser,chunker,retriever,watcher,manager,helpers}.rs`
- Split: `everevo-telemetry/src/lib.rs` → `{config,records,trace,writer}.rs`
- Split: `everevo-kg/src/lib.rs` → `{types,resolver,graph,extraction}.rs`

## 2026-07-18 — Session System + Context Pipeline + Thinking Architecture

**What:** Full session CRUD, cursor-paginated message history, extensible context injection pipeline, and model-native thinking display.

**Session & chat:**
- Session CRUD: `GET/POST /api/sessions`, `GET/PUT/DELETE /api/sessions/{id}`
- Cursor-based message pagination: `GET /api/sessions/{id}/messages?before=<uuid>&limit=50`
- Chat endpoint rewritten: auto-create session, load history via context pipeline, persist user+assistant messages
- Session list enriched with `message_count` + `last_message` preview
- Unified response envelope: `{ data, has_more, next_cursor?, total? }`

**Context injection pipeline** ([crates/everevo-core/src/context.rs](../crates/everevo-core/src/context.rs)):
- `ContextStage` trait with `priority()` + `build()` — pluggable stages
- `ContextPipeline::assemble()` composes full LLM context from all stages
- Built-in stages: `SystemPromptStage` (0), `ConversationHistoryStage` (80), `LatestMessageStage` (90)
- Reserved priority gaps for future: UserMemory (10), SessionMetadata (20), KnowledgeBase (40), ToolDefinitions (50)
- Adding RAG/KG/Tools = implement a trait, call `with_stage()` — zero core logic changes

**Thinking architecture** ([docs/llmwiki/thinking-architecture.md](docs/llmwiki/thinking-architecture.md)):
- Added `StreamEvent::Thinking(String)` for model-native chain-of-thought tokens
- Anthropic format: parses `content_block_delta` → `delta.thinking`
- OpenAI format: parses `delta.reasoning_content`
- Frontend: collapsible purple thinking panel, auto-open during streaming, auto-collapse on answer
- Design decision: same bubble for model-native thinking and future prompt draft (different labels: 🧠 深度思考 vs 📝 分析草稿)
- DeepSeek V4 Pro thinking tokens cost same as output — effectively free reasoning

**Frontend refactor:**
- Zustand store for session list + active session + messages + streaming state
- `SessionSidebar`: create/switch/delete sessions, last_message preview
- `ChatView`: session-aware, cursor-paginated history, infinite scroll, thinking panel
- App layout: sidebar + main area

**Storage decision:** SQLite only, no JSON sidecar. WAL mode provides crash safety; single-file portability; FTS5 search possible later.

---

## 2026-07-18 — Permission Model + Agent Hierarchy Architecture (Design)

**What:** Complete redesign of permission model and agent hierarchy. Design finalized; implementation pending.

**Decision:** [docs/llmwiki/permission-agent-architecture.md](docs/llmwiki/permission-agent-architecture.md)

**Permission model (4 levels, redesigned):**
- `ReadOnly` (0) — read files, no writes, no commands
- `FullyManual` (1) — every command requires user confirmation
- `SemiAuto` (2) — dangerous commands + plans flagged; safe commands auto-run (default)
- `FullyAuto` (3) — no confirmation, full audit trail

**Agent hierarchy:**
- `MainAgent` (ReadOnly) — planner, scheduler, auditor. Spawns sub-agents via delegation. Never executes directly.
- Sub-agents: `ResearchAgent`, `CodeAgent`, `ShellAgent`, `ReviewAgent` — each with scoped permission levels
- Authority attenuation (Narrowing Property): sub-agent level ≤ delegator level
- Max delegation depth = 3; cascade revocation

**Audit architecture:**
- Per-session `audit.jsonl` (append-only) + `decisions.jsonl` (delegation events)
- Cross-session `audit.db` SQLite index for queries
- Full causal chain: who asked → who approved → who executed → what happened

**References:** Claude Code 7-mode permission system, IETF MAD Protocol (draft-sato-soos-mad-02), AWS RAI Multi-Agent 7-layer governance, TDCommons Orchestrator Framework, SecureYeoman ADR 004

---

## 2026-07-18 — Session Sandbox + Audit Trail (Implemented)

**What:** Per-session sandbox isolation with JSONL audit trail. Removed redundant `files/` directory.

**Sandbox:**
- `SessionSandbox` (new): `data/sandbox/{session_id}/work/` — isolated per-session working directory
- `AuditWriter` (new): append-only JSONL with flush-after-write crash safety
- Wired into session lifecycle: create → init sandbox, delete → flush audit + cleanup
- `files/` removed from startup dirs (redundant with sandbox/work/)

---

## 2026-07-18 — Tauri Desktop Shell Fixed + Config Persistence

**What:** Fixed build errors blocking Tauri desktop shell launch, config now survives restart.

**Tauri fixes:**
- `icon.ico` regenerated (was 77-byte corrupt PNG renamed to .ico)
- `axum` dependency added to `src-tauri/Cargo.toml` (separate workspace)
- `frontend/dist/` created for Tauri build macro
- `EVEREVO_DATA_DIR` set to project root via `CARGO_MANIFEST_DIR` at compile time
- `tracing_subscriber` initialized in Tauri main.rs (was missing — all sandbox/server logs invisible)

**Config persistence:**
- `AppState::new()` now calls `load_llm_from_file()` to populate LLM clients from `data/config.toml`
- Previously: config saved to file but never read at startup — LLM map was always empty

---

## 2026-07-18 — Sandbox Phase 2 + Complete Plan

**What:** `everevo-sandbox` crate + 5-tier permission model + network policy + audit trail.
Based on Claude Code 6-mode permission system and Firecracker/gVisor isolation benchmarks.

**Sandbox crate (everevo-sandbox):**
- `SandboxProvider` trait in core — part of the Hexagonal Ports-Adapter pattern
- `TieredSandbox`: WSL → Job Objects → Filesystem 3-layer fallback
- `PermissionLevel`: ReadOnly / Sandboxed / Confirmed / Audited / Trusted (5 tiers)
- `NetworkPolicy`: Allowed / Restricted (whitelist) / Denied
- `AuditRecord`: structured log per execution (timestamp, command, exit_code, etc.)
- Deny patterns: `rm -rf /*`, `curl * | sh`, `format C:` blocked at sandbox level
- `ShellTool` refactored: takes `Arc<dyn SandboxProvider>` for testability

**Complete plan:** [docs/llmwiki/sandbox-complete-plan.md](docs/llmwiki/sandbox-complete-plan.md)
- Phase 2: Permission levels, deny patterns, audit trail ✅
- Phase 3: AppContainer (Win), bubblewrap (Linux), path allowlisting, UI confirmation
- Phase 4: WASM sandbox, Docker sandbox, gVisor, audit dashboard

**References:** Claude Code Permission Model, Arapuca cross-platform sandbox, rappct (AppContainer), Firecracker microVM, gVisor user-space kernel

---

## 2026-07-18 — Comprehensive Audit + 16 Fixes

**What:** 4-agent parallel audit (Architecture, Code Quality, Security & Performance, Decoupling). 49 findings, 16 fixed.

**Critical fixes:**
- Zip Slip path traversal in ZIP extraction (canonicalize + boundary check)
- API key leaked via `#[derive(Debug)]` on `LlmProviderConfig` → manual Debug with `[REDACTED]`
- Poisoned mutex crash in `MirrorRegistry::resolve()` → `unwrap_or_else(|e| e.into_inner())`

**High fixes:**
- Blocking I/O: `std::fs::create_dir_all` → `tokio::fs::create_dir_all` in main.rs
- CWD fallback changed from `"."` to exe-relative path
- `StreamEvent` name collision resolved (types.rs → `SseEvent`)
- LIKE wildcard injection fixed in `search_sessions` (escape `%`, `_`, `\`)
- Unused dependencies removed from 4 crates (agent: 5, bootstrap: 1, server: 3)
- Bootstrap cache invalidation added after extraction (4 call sites)

**Deferred (33 items):**
- 10 tasks added to Phase 2: trait abstractions, security hardening, god-function refactor, server tests, performance
- 9 tasks deferred to Phase 3-4: naming, config split, serde conventions, stub completion
- Full audit report: [docs/llmwiki/audit-2026-07-18.md](docs/llmwiki/audit-2026-07-18.md)

---

## 2026-07-17 — Bootstrap crate + full downloader verification

**Bootstrap (everevo-bootstrap):**
- New crate: first-run provisioning of portable runtimes + embedding models
- Assets: Python 3.12 embed, Node.js portable, MinGit, ONNX Runtime, BGE-small-zh (35MB CN), all-MiniLM-L6-v2 (22MB EN)
- Model reasoning: two specialized models (57MB) < one multilingual model (120MB), better per-language quality
- Startup flow: `check()` reads `.manifest.json` → returns {ready, missing, corrupt}
- Consumes everevo-downloader for actual downloads with mirror failover

**Downloader: 16+ compilation/logic fixes:**
- L1: EventBroadcaster Clone, Arc deref, add_mirror lock, 6 unused imports/deps
- Agent review: tokio features, per-request timeout, blocking→async I/O, resume bytes, Default derives, Debug impl, MutexGuard across await, recursive async
- Result: `cargo check --workspace` = 0e 0w across 6 crates

**Environment: Rust 1.96.0 @ F:\dev\rust, Aliyun mirror, CARGO_TARGET_DIR on F:**

---

## 2026-07-17 — Testing infrastructure + MockLlmProvider

**What:** Comprehensive testing strategy and infrastructure across all crates.

**Delivered:**
- `MockLlmProvider` — built-in test double implementing `LlmProvider` trait. FIFO response queue, call log for assertion, zero deps. Enables full agent loop testing without API calls.
- `LlmProvider` trait (async) — abstract interface with `chat()` and `chat_stream()`. Real client and mock share the same trait.
- L1 unit tests: `everevo-core` (error display, config, types), `everevo-downloader` (mirror transforms, config, task builder)
- L2 agent logic tests: `everevo-agent/tests/mock_agent_loop.rs` — ReAct loop, tool dispatch, call log verification
- L3 integration tests: `everevo-db/tests/integration.rs` (SQLite in-memory: CRUD, search, cascade), `everevo-downloader/tests/integration.rs` (mirror resolution)
- Testing strategy doc: `docs/llmwiki/testing-strategy.md` — 4-layer pyramid, quick verification workflow, MockLlmProvider design principles

**4-layer test pyramid:**
1. L1 (pure fn, <10ms) — `cargo test --workspace`
2. L2 (mock agent, ~50ms) — `cargo test -p everevo-agent`
3. L3 (integration, ~1-5s) — `cargo test --workspace --test integration`
4. L4 (real LLM, ~30s, $$) — `cargo test -- --ignored`

---

## 2026-07-17 — everevo-downloader crate

**What:** New `everevo-downloader` crate — general-purpose async download engine (11 source files).

**Features delivered:**
- Task-based download with priority queue (`Priority::Low/Normal/High/Critical`)
- Multi-mirror failover: 8 pre-configured mirrors (6 domestic CN + 2 international), region-aware scoring
- Resume/checkpoint: persistent `.resume.json` with chunk-level progress tracking
- Concurrent chunked download: auto-split large files (>10 MiB threshold), N workers, then assemble
- Three result access patterns:
  1. **Oneshot** — `handle.await` for fire-and-wait
  2. **Broadcast** — `tokio::broadcast` event stream (`DownloadEvent::Progress/Completed/Failed/...`)
  3. **Polling** — `downloader.get_state(task_id)` for on-demand state queries
- Observer pattern: register `DownloadObserver` trait implementations for lifecycle callbacks
- Mirror transforms: typed URL mapping (GitHubRelease, GitHubRaw, PathOnly) — no regex dependency
- Graceful cancellation via `tokio-util::CancellationToken` (pause = cancel with resume preserved)

**Why:** Agent needs to download files from the internet. Domestic users face slow/failed downloads from GitHub, PyPI, etc. The downloader provides transparent mirror failover, resumability, and observable progress — all essential for a reliable agent tool.

---

## 2026-07-17 — Phase 1 Scaffold Complete

**What:** Full project directory structure, all 4 crates, frontend skeleton, migrations, and tooling configuration created.

**Structure (47 files):**
- Root: `Cargo.toml` (virtual workspace, 4 members), `rustfmt.toml`, `rust-toolchain.toml`, `.gitignore`
- `crates/everevo-core` — Shared types, `EverEvoError`, `AppConfig` with 3-tier data dir resolution
- `crates/everevo-db` — SQLx models (`sessions`, `messages`), CRUD queries, `Database` struct
- `crates/everevo-agent` — Module stubs for `llm`, `tools`, `sandbox`, `memory`, `kg`, `rag`, `llmwiki`, `loop_`
- `crates/everevo-server` — Axum binary with `main.rs` (init tracing → config → db → serve), `lib.rs` (app builder), `health` + `chat` routes
- `migrations/001_initial.sql` — Sessions + messages tables with indexes
- `data/` — Dev-mode data directory (gitignored runtime files)
- `frontend/` — Vite + React 18 + TypeScript + Tailwind, proxy `/api` → `localhost:3000`

**Key design decisions refined:**
- 4 crates not 10: `everevo-core` (types/error/config), `everevo-agent` (all business logic), `everevo-db` (data access), `everevo-server` (Axum binary)
- Data directory: 3-tier resolution — `EVEREVO_DATA_DIR` env → `./data/` (dev) → platform data dir (prod)
- Strict dependency direction: `server → agent → core`, `server → db → core`, `agent → db`
- `core` has zero heavy I/O deps (no `tokio`, `sqlx`, `reqwest`)

**Next:** Implement Phase 1 tasks — LLM provider integration, SSE streaming, real chat endpoint.

---

## 2026-07-17 — Project Initialization

**What:** Project created. Technology research and architecture design completed.

**Decisions made:**
- Rust workspace with 9 internal crates, Axum web server, React frontend
- Embedded storage stack: SQLite + LanceDB + Oxigraph (zero external services)
- Self-built agent loop (ReAct pattern), rejected ADK-Rust for desktop mismatch
- LLM: multi-llm crate (Anthropic + OpenAI + Ollama)
- Sandbox: wasmtime (WASM) + bollard (Docker, optional)
- Frontend: React + TypeScript + Vite, browser-accessed (Tauri later optional)

**Why:** Desktop-grade agent application. All prior Go experience informs this Rust re-architecture. Core insight: server-side agent frameworks (ADK-Rust) are architecturally incompatible with local-first, embedded-everything desktop design.

**Initial state:** Empty workspace. Design doc written. Awaiting Phase 1 scaffold.
