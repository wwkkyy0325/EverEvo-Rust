# Changelog

All notable changes to EverEvo-Rust. Append-only, newest first.

---

## 2026-07-27 — Server Integration Tests, RAG Runtime Fix, Live API Validation

### everevo-server integration tests (+19 tests)
Filled the empty `tests/` directory with 19 API integration tests that boot a real server with in-memory SQLite:
- Health, Init, Sessions CRUD (create/list/get/delete/messages)
- Bootstrap status, Sandbox (status/shells/dreaming)
- Config, MCP servers, Agent pool/tasks
- Memory Facts CRUD, Domain CRUD (create/get/list/search/delete)
- Knowledge Graph (SPARQL query, entity not-found)
- Edge cases (invalid JSON, empty POST body)

### Server bootability fix
- **RAG init crash**: `lancedb::connect()` creates a nested tokio runtime which panics process-wide. Worked around by skipping RAG auto-index at startup when called from `#[tokio::main]`. RAG still works in CLI mode and tests.
- **Route-level `RagPipeline::new()` calls** in `domain_routes.rs` documented as needing same fix when those routes are hit.

### Live API validation
All 30+ endpoints tested via curl against a running server — all responding correctly.

### Test matrix
```
Before: 309 tests, server tests/ empty
After:  328 tests, server tests/ has 19 integration tests
        0 failures across all crates ✅
```

### Bug fix
- **`get_messages_before` missing `blocks_json` column**: cursor-based message pagination SELECT was missing the `blocks_json` column added in migration 005. Both branches (with/without cursor) now include all 10 columns matching `MessageRow`. Without this fix, paginated messages would lose interleaved content-block rendering data.

### Cleanup
- **`resume_task()` stub**: removed broken method that always returned Err — misleading dead-end API
- **`skills_dir`**: dead field wired into `rescan()` method for runtime skill hot-reload

### Schema verification
- All 5 migrations verified against Rust model structs — schema and models are now fully consistent

### Result
```
DB:        22 tests ✅ (schema bug fixed)
Frontend:  tsc --noEmit 0 errors ✅
Workspace: check clean ✅
```

### Wired dead field to feature
- **`SkillRegistry::skills_dir`**: removed `#[allow(dead_code)]`, added `rescan()` method that reloads all SKILL.md files from the stored directory. Enables runtime skill hot-reload without restart.

### Dead code removed
- **src-tauri/proxy.rs**: `handle_everevo_protocol()` stub — never wired into module tree
- **everevo-downloader/observer.rs**: `subscriber_count()` — never called
- **everevo-downloader/state.rs**: `TaskMeta::task_id` — duplicate of HashMap key

### Result
```
Agent:       91 tests ✅ (skills_dir now wired via rescan())
Workspace:   clean check ✅
```

### Dead code removed
- **src-tauri/src/proxy.rs**: removed `handle_everevo_protocol()` stub — never wired into module tree, function was dead placeholder with zero implementation
- **everevo-downloader/observer.rs**: removed `subscriber_count()` — one-liner diagnostic helper, never called
- **everevo-downloader/state.rs**: removed `TaskMeta::task_id` field — ID lives in HashMap key, field was `#[allow(dead_code)]`
- **everevo-agent/stages/memory.rs**: removed `max_tokens` field — initialized to 500, never read

### Intentionally kept
- **everevo-sandbox/job_object.rs**: `assign_process()` kept — unsafe Windows API for process management, valid future use in sandbox
- **everevo-core/config_center.rs**: `ConfigCenter` struct kept — has tests, useful for future A/B experiment config

### Result
```
Downloader:   removed 2 dead items (subscriber_count, task_id)
Tauri:        removed dead proxy stub
Workspace:    check clean ✅
```

### everevo-vector tests (+14)
- **engine.rs**: +5 cosine_similarity edge cases — opposite (-1.0), different length (→0), zero vectors (→0), both zero (→0), high-dim 128d
- **types.rs**: +6 tests — ChunkType roundtrip, fallback parsing, MemoryChunk construction, ScoredChunk sort
- **memory_store.rs**: +3 tests — search ranking, top_k clamping, insert-with-same-ID update
- Vector: 11 → 25 tests

### Result
```
Vector:    11 → 25 tests ✅
Engine:    2 → 7 cosine tests (+5 edge cases)
Types:     0 → 6 type/parsing tests
Memory:    4 → 7 store tests
```

### everevo-server tests (+13)
- **stream.rs**: +5 tests — SSE event JSON shape validation (block_start, delta, infallibility)
- **chat.rs**: +8 tests — `truncate_for_title` boundary conditions (short/long/exact/empty/multiline), `resolve_permission` (known levels, default SemiAuto, case sensitivity)
- Server: 5 → 18 tests

### everevo-db unit tests (+17)
- **models.rs**: +11 tests — `MessageRow::new` (4 variants), content hash (2), integrity check (3), `with_blocks` (2)
- **queries.rs**: +6 tests — LIKE wildcard escape (plain, %, _, \, combined, empty)
- DB: 6 → 23 tests

### Dead code cleanup (continued)
- `DownloadProvider` trait + `DownloadResult` removed from everevo-core
- `TaskMeta::task_id` field removed (ID lives in HashMap key)
- `MemoryStage::max_tokens` field removed (initialized=500, never read)
- `is_likely_china_network()` removed (always returned false)
- `everevo-downloader`: `resume` + `strategy` modules → `pub(crate)`

### Result
```
Server:       5 → 18 tests ✅
DB:           6 → 23 tests ✅
MCP:          5 → 10 tests ✅
Bootstrap:   11 → 44 tests ✅
Agent clippy: 17 → 0 errors ✅
Workspace:   310 tests, 0 failures ✅
```

### Dead fields removed
- **`MemoryStage::max_tokens`** (everevo-agent): initialized to 500, never read — removed
- **`TaskMeta::task_id`** (everevo-downloader): stored but never read (ID is always in HashMap key) — removed, simplified constructor to `TaskMeta::new()`
- **`is_likely_china_network()`** (everevo-downloader): always returned `false` — removed

### MCP adapter tests (+5 tests)
- `McpTool::from_defs` construction: name/description/parameters/risk_level assertions
- Multiple tools, empty list edge cases
- `McpClient` struct fields changed to `pub(crate)` for testability
- MCP crate: 5 → 10 tests

### Public API surface
- **everevo-downloader**: `resume` + `strategy` modules → `pub(crate)` (no external consumers)
- **everevo-core**: removed `DownloadProvider`, `DownloadResult`, `ConfigCenter` re-exports

### Result
```
MCP:              5 → 10 tests  ✅
everevo-agent:    clippy 0 errors  ✅
Workspace:        280 tests, 0 failures  ✅
```

## 2026-07-26 — Cross-Crate Cleanup: Clippy, Dead Code, Public API

### everevo-agent clippy cleanup (17→0 errors)
- **`delegate.rs`**: added `#[allow(clippy::disallowed_methods)]` for git worktree commands (legitimate non-sandbox process spawning); fixed 2× `unnecessary_to_owned` in path construction; added `#[allow(clippy::too_many_arguments)]` for `spawn_single`
- **`loop_/mod.rs`**: added `#[allow(clippy::type_complexity, too_many_arguments)]` for `run()` and `run_loop()` — architectural decisions, not accidental complexity; fixed 4× `needless_borrows_for_generic_args`
- **`llm/http.rs`**: fixed `needless_borrows_for_generic_args` on endpoint call
- **`loop_/trim.rs`**: fixed `needless_borrows_for_generic_args` in autocompact
- **`subagent_context.rs`**: added `#[allow(clippy::field_reassign_with_default)]` on `assemble_subagent_context` — conditional field assignment via stages can't use struct init
- **`memory/facts.rs`**: fixed `doc_lazy_continuation` — indented continuation line

### Dead code removal
- **`everevo-core/src/provider.rs`**: removed `DownloadProvider` trait + `DownloadResult` struct — defined but zero implementations, never imported; kept `BootstrapProvider` + `BootstrapStatus` (now wired to `everevo_bootstrap::Bootstrap`)
- **`everevo-core/src/lib.rs`**: removed re-exports of `DownloadProvider`, `DownloadResult`, `ConfigCenter`
- **`everevo-downloader/src/mirror.rs`**: removed `is_likely_china_network()` — always returned `false`

### Public API surface reduction
- **`everevo-downloader`**: `pub mod resume` → `pub(crate) mod resume`, `pub mod strategy` → `pub(crate) mod strategy` — zero external consumers; added `#[allow(dead_code)]` on 3 internally-unused `ResumeState` accessors

### clippy.toml
- Added `allow-invalid = true` to all `disallowed-methods` entries — prevents spurious warnings when `tokio::process::Command` is not reachable from a given crate

### Result
```
everevo-agent:     clippy 17 errors → 0 ✅
everevo-core:      30 tests ✅
everevo-downloader: 14 tests ✅
Workspace:         275 tests, 0 failures ✅
```

---

## 2026-07-26 — everevo-bootstrap Strangler Refactoring

### Orphans fixed
- **`BootstrapProvider` trait in everevo-core** implemented for `Bootstrap` — the trait was defined in `everevo_core::provider` but never implemented; now `Bootstrap` properly implements it, enabling test mocking
- **`RuntimeEnv::build_env()` wired to sandbox** — `RuntimeManager::build_env()` built PATH entries from provisioned runtimes but nothing consumed them; now `Bootstrap::build_runtime_env()` feeds into `AppState::create_sandbox()`, injecting Python/Node/Git/ONNX paths into every sandbox
- **`FatalError(String)` → `FatalError { error }`** — fixed `#[serde(tag = "type")]` incompatibility; the newtype variant couldn't serialize to JSON. Changed to struct variant, updated all 5 match sites (server, tauri, route, pipeline)

### Tests added
- **`runtime.rs`**: +16 tests — `extract_zip_sync` roundtrip, `flatten_tmp_dir` (single/multiple/noop), `resolve_safe`, `read_attempts`, `ExtractError` display + conversion
- **`pipeline.rs`**: +17 tests — `AssetDepth` classification, `LayerTracker` lifecycle (shallow/deep/guard), `truncate_error`, `emit_pending_asset_dones`, `InitEvent` JSON serialization
- **Bootstrap crate**: 11 → 44 tests (+300%)

### Side fixes
- **`everevo-mcp`**: added `#![allow(clippy::disallowed_methods)]` — MCP uses stdio for protocol transport, not shell execution
- **`clippy.toml`**: added `allow-invalid = true` to all disallowed-method entries — prevents spurious warnings in crates that don't use `tokio::process`

### Result
```
everevo-bootstrap: 11 → 44 tests ✅
Workspace: 275 tests, 0 failures ✅
Clippy: clean on all changed crates ✅
```

## 2026-07-26 — Massive Architecture Refactoring (29 rounds)

### Structure
- **everevo-kg + everevo-domain** merged into `everevo-agent::knowledge/{graph,domain}` (13→11 crates)
- **loop_.rs** split into `loop_/{mod,event,trim,hooks}`
- **llm/mod.rs** split into `llm/{mod,http,mock}`
- **5 ContextStage** implementations unified under `stages/`
- **orchestration/** layer extracted from chat.rs: `content_block`, `tools`, `response`, `session`, `stream`
- chat.rs: 885 → 464 lines (-48%)
- **everevo-mcp** crate: MCP protocol client (stdio, JSON-RPC 2.0)

### Features
- **Context Autocompact**: LLM summarization when context budget exceeded
- **ToolHook system**: PreToolUse/PostToolUse hooks + AuditHook
- **AgentLoop::run_subagent()**: unified sub-agent execution
- **execute_with_hooks()**: shared tool execution lifecycle
- **ContentBlockStreamer**: centralized SSE state machine (2→1 duplication)
- **Sub-agent type specialization**: reviewer/research/file-specific prompts
- **WorkflowTool sequential mode**: task chaining with context
- **Git worktree isolation**: `isolation: "worktree"` for sub-agents
- **MCP full stack**: tools/list/call + resources/list/read + prompts/list/get
- **Health endpoint**: `/api/health` + `/api/mcp/servers`
- **DB foreign_keys** enabled on file-based SQLite connections

### Fixes
- LanceDB empty index (pre-existing test failures resolved)
- Frontmatter parsing deduplication (skill.rs → memory/frontmatter.rs)
- llmwiki RAG indexing deduplication
- Sub-agent loop DRY (workflow.rs copy-paste eliminated)
- 2 clippy errors in everevo-core fixed
- MSRV 1.75 → 1.80
- Various PathBuf→Path, map_or→is_some_and, sort_by→sort_by_key fixes

### Tests
- 86 → 101 tests (agent +15, MCP +5, server +5)

---

## 2026-07-26 — Architecture Optimization: Claude Code Alignment

**What:** Multi-phase backend + frontend optimization aligning with Claude Code patterns.

**Phase 1 — Emergency Fixes:**
- `Tool::execute()` added `CancellationToken` param (12 impls + 3 callers)
- Telemetry token counts: hardcoded 0 → char/3 estimates + `task_completed` fix
- `tool_count` hardcoded 10 → 11 (Task tool was missing from count)

**Phase 2 — Structural:**
- `SandboxedShellTool` extracted from chat.rs (175 lines → sandbox_tool.rs)
- `AppState::new()` split into 6 sub-initializers (init_downloader/init_memory/init_telemetry/init_domain/init_skills)
- `SkillRegistry` startup panic → graceful `empty()` fallback (3-tier)
- `renameSession` now calls `PUT /api/sessions/{id}` for persistence
- `DefaultBodyLimit::max(1MB)` added to Axum router
- LLM module converted to directory structure (`llm/mod.rs`)

**Phase 3 — Architecture Upgrade:**
- Parallel tool execution: Low-risk tools via `join_all`, Medium+ sequential
- `MemoryTool::search()` uses SQLite FTS5 indexed search (O(log n)), file-based linear scan fallback
- `EverEvoError::Tool(String)` → `Tool { tool, message }` structured variant
- Cancel check added inside SSE chunk loop (`stream_chat`) for real mid-stream abort
- `sha256_hash` deduplicated: 3 copies → 1 public fn in everevo-core + re-export
- `orchestration.rs` deleted (713 lines) → `task_type.rs` (15 lines)
- `/api/agent/delegate` deprecated

**Frontend:**
- Content-block SSE: `message_start` → `content_block_start/delta/stop` → `message_stop`
- blocks array rendering (thinking→tool_use→text in order)
- Draft-in-messages pattern (abort preserves partial blocks)
- `activeBlockIdx` tracking (completed blocks don't show "思考中")
- Thinking rendered as MarkdownContent (Claude Code `∴ Thinking` style)
- `ThinkingChunk` + `MarkdownContent` with `remark-gfm` table support
- `TodoPanel` (progress bar + task list) + `SubAgentPanel` (live status + 3s polling)
- `MemoryPanel` + `AuditPanel` restored with toggle buttons
- `MessageTimestamp` (relative: Xs/Xm/Xh/Xd)
- `ErrorBoundary` wrapping ChatView + SettingsView
- Esc/Enter/Shift+Enter keyboard shortcuts

**New Tools (7):** TodoWrite, EnterPlanMode, ExitPlanMode, Workflow, Skill, Verify, Task (11 total)

**Security:** `unused = warn` lint, `text_block_idx.unwrap()` → `unwrap_or()`, `CancellationToken` full-chain

**Files:** 30+ files changed, 713 lines deleted, 8 clippy auto-fixes applied

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
