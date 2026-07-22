# EverEvo-Rust Design Document

## Project Overview

Desktop-grade AI Agent application built in Rust. Multi-turn conversation, sandboxed tool execution, knowledge graph, RAG pipeline, and llmwiki project knowledge base — all embedded in a single process, zero external service dependencies.

**Architecture Style:** Local-first, embedded-everything, browser-accessed web server (Tauri shell optional for later).

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                    Frontend (React/TypeScript + Vite)                 │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌─────────────┐ │
│  │ Chat UI      │ │ Tool Viz     │ │ Session List │ │ KG Viz      │ │
│  └──────┬───────┘ └──────┬───────┘ └──────┬───────┘ └──────┬──────┘ │
│         └─────────────────┴────────────────┴───────────────┘         │
│                              │ HTTP/SSE                              │
└──────────────────────────────┼───────────────────────────────────────┘
                               │
┌──────────────────────────────┼───────────────────────────────────────┐
│                   Backend (Rust - Axum Server, single binary)         │
│                                                                       │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │  Agent Core: ReAct Loop → Tool Registry → Session → Memory   │    │
│  └──────────────────────────────────────────────────────────────┘    │
│                                                                       │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │  Knowledge Layer: RAG Pipeline │ Knowledge Graph │ llmwiki   │    │
│  └──────────────────────────────────────────────────────────────┘    │
│                                                                       │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │  Sandbox Layer: WASM (wasmtime) + Docker (bollard, optional) │    │
│  └──────────────────────────────────────────────────────────────┘    │
│                                                                       │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │  Storage (all embedded, file-based):                         │    │
│  │  SQLite (SQLx) │ LanceDB (vector) │ Oxigraph (RDF graph)     │    │
│  └──────────────────────────────────────────────────────────────┘    │
│                                                                       │
│  Data directory: ~/.everevo/                                         │
└──────────────────────────────────────────────────────────────────────┘
```

### Why NOT ADK-Rust

ADK-Rust is designed for server-side microservice architecture (39 crates, multi-tenant, REST/A2A APIs). This project is desktop-grade: single-user, embedded storage, single binary, instant startup. The architectural mismatch is fundamental.

---

## Technology Stack

### Backend

| Module | Crate | Why |
|--------|-------|-----|
| Web Framework | **Axum** | Tokio-native, Tower middleware, function-handler model, best 2025 default |
| LLM SDK | **multi-llm** (v1.0) | Only stable v1.0 multi-provider Rust SDK; Anthropic + OpenAI + Ollama |
| Database | **SQLx** + SQLite | Native async, compile-time SQL validation, zero external deps |
| Vector Store | **LanceDB** | Embedded, Rust-native, file-based, 100B+ scale capability |
| Knowledge Graph | **Oxigraph** | Embedded RDF graph, SPARQL 1.1, Rust-native, RocksDB-backed |
| Embedding | **fastembed-rs** | Local ONNX inference, no API cost, ~50MB model files |
| WASM Sandbox | **wasmtime** | Instant startup, strong isolation, ~3MB embedded |
| Docker Sandbox | **bollard** | Optional, for heavier isolation needs |
| Document Parsing | pulldown-cmark, lopdf, tree-sitter | Markdown, PDF, Code chunking |
| Serialization | serde + serde_json | Standard |
| Async Runtime | Tokio | Axum dependency, de facto standard |

### Frontend

| Concern | Choice | Why |
|---------|--------|-----|
| Framework | **React 18+** + TypeScript | Best AI coding support, richest chat UI ecosystem, streaming SSE mature |
| Build | **Vite 6** | Fast HMR, standard for React SPAs |
| Styling | **Tailwind CSS v4** | CSS-first config (`@theme`), OKLCH color space, 5x faster builds |
| Component Library | **shadcn/ui** (new-york) | Source-copy pattern (no npm black box), Radix primitives, full customization |
| Design Tokens | **CSS Variables + OKLCH** | Three-tier: global → semantic → component. Theme-agnostic, runtime-swappable |
| Theming | `data-theme` attribute + `.dark` class | 4 color themes × dark/light = 8 combinations. localStorage persistence |
| Icons | **Lucide React** | Default shadcn/ui icon library, tree-shakeable |
| State | **Zustand 5** | Lightweight, no boilerplate |
| Chat Streaming | Fetch API + ReadableStream | SSE over HTTP, native browser support |
| Markdown | react-markdown + rehype-highlight | Code block syntax highlighting |

**Theme system architecture:**
```
CSS Variables (:root / .dark / [data-theme="ocean"])   ← Design tokens (OKLCH)
  → Tailwind v4 @theme inline mapping                   ← Utility classes (bg-primary, text-foreground)
  → shadcn/ui components                                ← Primitives consume tokens
  → App components                                      ← Business components consume tokens
```

**Supported themes (all with dark/light variants):**
- `default` — Blue-gray, professional tech (primary: OKLCH 264° blue)
- `ocean` — Teal/cyan, calm & clean (primary: OKLCH 200° teal)
- `sunset` — Warm orange/amber, energetic (primary: OKLCH 55° orange)
- `forest` — Emerald green, natural (primary: OKLCH 155° green)
- `pixel` — Minecraft-inspired 8-bit retro: grass green, stone gray, gold accents, Press Start 2P font, sharp corners, pixel shadows. Zero component code changes — purely CSS `[data-theme="pixel"]`

### Storage Summary

```
SQLite (~/.everevo/data.db)    → Sessions, Messages, Tool executions, Entity metadata
LanceDB (~/.everevo/vectors/)  → Document chunks, Embeddings
Oxigraph (~/.everevo/graph/)   → Knowledge graph triples (entity-relation-entity)
Filesystem (~/.everevo/files/) → Uploaded documents, sandbox workspaces, llmwiki cache
```

### Data Directory

All runtime data lives under `data/` in the project root. Dev and prod are the same — no platform directories.

| Priority | Path | Condition |
|----------|------|-----------|
| 1 (env) | `$EVEREVO_DATA_DIR` | Explicit override |
| 2 (default) | `./data/` relative to CWD | Always — dev & prod both |

> Run `everevo-server.exe` from the project root and everything stays local.

### Compiled Binary Location

Standard Cargo behavior — no custom configuration needed:

```
target/debug/everevo-server.exe     ← cargo build / cargo run
target/release/everevo-server.exe   ← cargo build --release
```

---

## Testing (see [testing-strategy.md](testing-strategy.md) for full details)

**Four-layer pyramid — every new feature follows this:**

| Layer | What | Command | Cost |
|-------|------|---------|------|
| L1 — Unit | Pure functions (config, types, errors, mirror transforms) | `cargo test --workspace` | $0, <10ms |
| L2 — Agent Logic | `MockLlmProvider` → canned responses → assert ReAct loop | `cargo test -p everevo-agent` | $0, ~50ms |
| L3 — Integration | Real DB (SQLite :memory:), real mirror registry, cross-crate wiring | `cargo test --test integration` | $0, ~1-5s |
| L4 — E2E | Real LLM + real tools, gated behind `#[ignore]` | `cargo test -- --ignored` | ~$0.02, ~30s |

**Core principle:** `LlmProvider` trait enables a `MockLlmProvider` that returns pre-configured
responses via `.with_text()` / `.with_tool_call()`. Agent code operates against the trait —
it never knows whether it's talking to a mock or the real API. **Write the mock test first,
verify with real LLM last.**

**Quick verify after any change:**
```bash
cargo test --workspace && cargo clippy --workspace --all-targets
```

---

## Core Modules

### 1. Agent Core (ReAct Loop)

```
User Message
  → SessionManager.load_history(session_id)
  → MemoryManager.build_context(query, history, rag_results, kg_results)
  → LLM.chat(messages, tools) → Response
  → If tool_calls:
      → SandboxExecutor.execute(tool_call)
      → Append result to messages
      → LLM.chat(messages, tools)  // continue loop
  → SessionManager.save_message(...)
  → MemoryManager.maybe_summarize(...)  // if context exceeds threshold
  → Stream final response via SSE
```

### 2. Sandbox (Three-Tier Isolation)

| Tier | Technology | Use Case | Startup |
|------|-----------|----------|---------|
| WASM | wasmtime | Code execution (light), plugin sandbox | Instant |
| Docker | bollard (Docker API) | Shell commands, complex tool execution | ~1-2s |
| Filesystem | cap-std + per-session tmp dir | File read/write isolation | Instant |

Strategy pattern: Tool declares `risk_level`, executor auto-routes to appropriate tier.

### 3. Tool System (extensible by design)

`Tool` trait + `ToolRegistry` live in `everevo-core` — **any crate can implement a tool**
without depending on `everevo-agent`. Built-in implementations are in
`everevo-agent::tools::builtins`.

```rust
// everevo-core/src/tool.rs — trait definition (pure abstraction)
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    fn risk_level(&self) -> RiskLevel;
    async fn execute(&self, params: Value) -> Result<ToolOutput, EverEvoError>;
}
```

**Implemented (3 builtins):**
- `shell` — CLI execution via ShellResolver (WSL→GitBash→PowerShell→CMD fallback)
- `download` — File download via Downloader (multi-mirror, resume, concurrent)
- `bootstrap_check` — Runtime/model status check via Bootstrap

**Planned (Phase 3-4):**
- `web_search`, `web_fetch`, `file_read/write`, `code_exec`
- `kg_query`, `rag_search`, `llmwiki_read/write`
- MCP tools (external, via stdio protocol)

**Extension pattern:**
```rust
// Any crate can add a tool:
use everevo_core::tool::{Tool, ToolOutput};
struct MyTool;
impl Tool for MyTool { /* ... */ }
registry.register(Arc::new(MyTool));
```

### 4. Downloader Engine (everevo-downloader)

General-purpose async download engine. Later exposed as `download` tool to the agent.

**Capabilities:**
- **Multi-mirror** — pre-configured domestic (CN) + international mirrors, auto-failover
- **Resumable** — HTTP Range requests + persistent `.resume.json` checkpoint
- **Concurrent** — auto-split large files into chunks, parallel download, then assemble
- **Observable** — three access patterns: Observer callbacks, broadcast channel events, state polling
- **Region-aware** — `Region::Domestic` / `International` / `Auto` hint for mirror selection

**Pre-configured mirrors:**

| Region | Mirror | Coverage |
|--------|--------|----------|
| CN | Tsinghua TUNA | GitHub releases, Linux ISOs, language toolchains |
| CN | USTC | GitHub, PyPI, Maven, Node |
| CN | Aliyun | GitHub releases, PyPI |
| CN | Tencent Cloud | GitHub, PyPI, Node, Go, Maven |
| CN | Huawei Cloud | GitHub, PyPI, Maven, Node |
| CN | NetEase 163 | GitHub, PyPI, Alpine, MySQL |
| Global | jsDelivr | GitHub raw, npm (works in CN too) |
| Global | Fastly GH | GitHub raw via Fastly CDN |
| Global | ghproxy | GitHub content proxy |

**Three result access patterns:**

```rust
// Pattern 1: Fire-and-wait (oneshot)
let handle = downloader.submit(task).await?;
let result = handle.await?;

// Pattern 2: Broadcast event stream
let mut rx = downloader.events();
while let Ok(event) = rx.recv().await { ... }

// Pattern 3: State polling
let state = downloader.get_state(&task_id).await;
```

**Internal architecture:**
- `DownloadTask` — URL, dest, priority, region, chunk config, headers, checksum
- `TaskState` — Pending → ResolvingMirror → Downloading → Completed/Failed/Cancelled
- `MirrorRegistry` — host-indexed mirror lookup with region+speed scoring
- `ResumeState` — per-task JSON file tracking completed chunks
- `DownloaderConfig` — max_concurrent_tasks, chunk_size, chunk_threshold, timeouts

### 5. Bootstrap (everevo-bootstrap)

First-run provisioning of portable runtimes and embedding models. Consumes `everevo-downloader`.

**Provisioned assets:**

| Asset | Version | Size | Source |
|-------|---------|------|--------|
| Python (embeddable) | 3.12.8 | ~10 MB | python.org + Huawei mirror |
| Node.js (portable) | 22.12.0 | ~30 MB | nodejs.org + npmmirror |
| Git (MinGit) | 2.47.1 | ~50 MB | GitHub + Huawei mirror |
| ONNX Runtime | 1.21.0 | ~15 MB | GitHub + Aliyun mirror |
| BGE-small-zh (CN model) | v1.5 | ~35 MB | HuggingFace + hf-mirror |
| all-MiniLM-L6-v2 (EN model) | v1 | ~22 MB | HuggingFace + hf-mirror |

**Model selection rationale:**
- Chinese: BGE-small-zh (35MB, 384 dims, best Chinese retrieval)
- English: all-MiniLM-L6-v2 (22MB, 384 dims, fastest/smallest)
- Two specialized models (57MB) < one multilingual model (120MB)

**Startup flow:**
```
Bootstrap::check() → reads .manifest.json → returns {ready, missing, corrupt, download_size}
  ├─ First run: 6 items missing, 162 MB to download
  └─ Subsequent: 0 missing, instant skip
```

### 7. Knowledge Graph

Entity extraction from conversation via LLM → RDF triples → Oxigraph SPARQL queries.

Example flow:
```
User: "I modified UserService.login() yesterday"
  → LLM Entity Extraction:
    Entity: UserService (type: Service)
    Entity: login (type: Method)
    Relation: UserService --hasMethod--> login
    Relation: login --modifiedAt--> 2026-07-16
    Relation: login --modifiedBy--> current_user
  → Oxigraph INSERT triples
  → Agent can later query: "Which methods does UserService have?"
```

### 8. RAG Pipeline

```
Ingestion:  File → Parse → Chunk → Embed → Store (LanceDB)
Retrieval:  Query → Embed → ANN Search → Rerank → Top-K → Inject Context
```

Chunking strategies: Fixed-size, Recursive (paragraph → sentence), Semantic (tree-sitter for code).

### 9. llmwiki Manager

Follows CLAUDE.md convention: `docs/llmwiki/` directory with `design.md`, `tasks/`, `changelog.md`.

- Agent reads llmwiki docs for project context
- Agent can suggest updates based on conversation insights
- All docs indexed in RAG pipeline for retrieval

### 10. Multi-turn Conversation

- Session identified by UUID
- Full message history in SQLite
- Context window management: sliding window + LLM summarization when threshold exceeded
- Cross-session search for historical context
- Knowledge graph entities linked to sessions

---

## Cargo Workspace Structure

```
EverEvo-Rust/
├── Cargo.toml                     # [workspace] members
├── crates/
│   ├── everevo-core/              # Shared types, traits, errors
│   ├── everevo-llm/               # LLM Provider abstraction + multi-llm integration
│   ├── everevo-agent/             # ReAct loop, session manager, memory, prompt building
│   ├── everevo-tools/             # Tool trait + registry + built-in tools
│   ├── everevo-sandbox/           # WASM + Docker + filesystem isolation
│   ├── everevo-kg/                # Knowledge graph (Oxigraph wrapper + extraction)
│   ├── everevo-rag/               # RAG pipeline (chunking, embedding, LanceDB, retrieval)
│   ├── everevo-llmwiki/           # llmwiki manager (read/write/index project docs)
│   ├── everevo-db/                # SQLx models, migrations, queries
│   └── everevo-server/            # Axum app (assembles all crates, routes, config)
├── frontend/                      # React + TypeScript + Vite
├── migrations/                    # SQLx migration files
├── docs/llmwiki/                  # Project knowledge base (self-hosted)
│   ├── design.md
│   ├── changelog.md
│   └── tasks/
└── docker-compose.yml             # Optional: PostgreSQL for production mode
```

---

## Implementation Phases

### Phase 1: Foundation
- Cargo workspace + all crate scaffolding
- Axum server + health/chat route stubs
- SQLx + SQLite + migrations (sessions, messages tables)
- LLM Provider integration (multi-llm: Anthropic + OpenAI)
- SSE streaming endpoint functional
- **Verify:** `curl POST /chat` returns streamed LLM response

### Phase 2: Agent Core
- ReAct loop implementation
- Tool trait + ToolRegistry
- 5 built-in tools: web_search, web_fetch, file_read, file_write, shell
- TieredSandbox (WASM + Docker + filesystem)
- Session manager + context window + summarization
- **Verify:** Multi-turn conversation with tool calling works via curl

### Phase 3: Knowledge Layer
- RAG pipeline: chunk → embed → store → search
- Knowledge graph: entity extraction → Oxigraph → SPARQL query
- llmwiki manager: read/write/index project docs
- Agent can query KG and RAG during conversation
- **Verify:** "What methods does UserService have?" returns correct answer from ingested codebase

### Phase 4: Frontend + Polish
- React chat UI with SSE streaming
- Tool call visualization (expandable cards)
- Session list + management
- Knowledge graph visualization (force-directed graph)
- llmwiki management UI
- Docker Compose one-command startup
- **Verify:** Full E2E: chat → tool call → kg query → rag search → response

---

## Key Design Decisions

1. **Embedded everything** — SQLite not PostgreSQL, LanceDB not Qdrant, Oxigraph not Neo4j. Zero external services.
2. **Self-built agent** — Not ADK-Rust. Desktop-grade needs fundamentally differ from server-side frameworks.
3. **Direction B first** — Local web server + browser UI. Tauri shell can wrap later with zero architecture changes.
4. **Multi-provider from day one** — Anthropic + OpenAI via multi-llm. Ollama local model support built in as option.
5. **All storage under `~/.everevo/`** — Portable, copyable, no hidden global state.

---

## References

- [Axum vs Actix Web (2025)](https://www.bacancytechnology.com/insights/axum-vs-actixweb-vs-rocket)
- [ADK-Rust](https://github.com/zavora-ai/adk-rust) — evaluated, rejected for desktop mismatch
- [multi-llm crate](https://github.com/darval/multi-llm) — v1.0 stable multi-provider LLM SDK
- [LanceDB](https://lancedb.com/) — embedded vector DB, Rust-native
- [Oxigraph](https://github.com/oxigraph/oxigraph) — embedded RDF graph database
- [wasmtime](https://wasmtime.dev/) — WASM runtime for sandboxed code execution
- [fastembed-rs](https://crates.io/crates/fastembed) — local embedding via ONNX
