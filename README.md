# EverEvo

**Extensible desktop AI agent** with a plugin microkernel, sandboxed tool execution, long-term memory, and multi-protocol agent communication.

## Features

- **Agent Loop** — streaming content-block SSE with catch_unwind, autocompact, and context trimming
- **22+ Built-in Tools** — shell, file I/O, web search/fetch, code search, browser bridge, sub-agent delegation, workflow automation, and more
- **Microkernel Plugin System** — wasm-based plugins for tools, stages, and hooks with canary deployment and version management
- **Tiered Sandbox** — 4 permission levels with process isolation, path rules, and audit logging
- **Long-term Memory** — facts, reflection, diary, meta-agent curation, and knowledge graph (Oxigraph)
- **Vector Search** — ONNX embeddings with HNSW vector store and multi-collection support
- **A2A Protocol** — Agent-to-Agent communication gateway (v0.3.0) with agent cards and task execution
- **MCP Integration** — Model Context Protocol client with stdio + HTTP transports
- **Web Agent** — browser automation with anti-detection (fingerprint randomization, stealth), CAPTCHA solving, and structured content extraction
- **Multi-engine Web Search** — aggregated search across multiple providers with result deduplication and parsing
- **Code Search** — file watcher, incremental indexing, and semantic code retrieval
- **Sub-agent Orchestration** — parallel delegate agents with git worktree isolation
- **Slash Command System** — extensible command framework with configurable routing
- **React Frontend** — chat UI, thinking panel, todo tracking, memory browser, sub-agent monitor, character config

## Quick Start

### Prerequisites

- Rust 1.80+
- Node.js 22+
- PNPM or npm

### Build

```bash
# Backend
cargo build --release

# Frontend
cd frontend && npm install && npx vite build

# Tauri desktop app
cd src-tauri && cargo build --release
```

### Run

```bash
# Start the server
cargo run -p everevo-server

# Dev mode with hot reload
cd frontend && npx vite
```

## Project Structure

```
crates/
├── kernel/          # Core kernel, MCP protocol, plugin runtime
├── app/             # Agent, server, A2A, webagent, workflow
├── infra/           # DB, sandbox, vector, knowledge, downloader, bootstrap
├── tools/           # Standalone binaries (bundler)
plugins/             # Wasm plugins (tools, stages, hooks)
frontend/            # React + Vite + Zustand + Tailwind
migrations/          # SQL migration files
```

## License

Licensed under either of [MIT License](LICENSE) or [Apache License 2.0](LICENSE-APACHE) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you shall be dual licensed as above, without any additional terms or conditions.
