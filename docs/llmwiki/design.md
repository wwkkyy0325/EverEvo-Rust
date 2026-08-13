# EverEvo-Rust Design Document
> **状态**:✅ 仍有效(现状速览单页)— 数字已更新对齐 architecture/ 01-14

---


## Current State (2026-08)

**14 crates + 2 binaries, 796 tests (764 root workspace + 32 plugins)**, tool registry with **49 个注册调用** running on a content-block SSE streaming architecture. Desktop AI agent with sandboxed tool execution, MCP server + A2A protocol integration, long-term memory (facts + diary + knowledge graph), sub-agent orchestration with git worktree isolation, pluggable context pipeline (**14 个 stage**) with LLM autocompact, and embedded ONNX embeddings with HNSW vector store.

> **精确数字与每一层的现状说明以 [architecture/00-overview.md](architecture/00-overview.md) 01-14 为准**:stage 数见 03,工具注册见 06,记忆见 04,遥测见 13。本文是单页速览。

> **File organization (2026-08-12):** the 9 largest source files were split along semantic
> module-cohesion boundaries (never by line count) into sibling submodules; public paths are
> preserved via root `pub use` re-exports, so external call sites are unchanged. See the per-file
> map in `docs/llmwiki/changelog.md` (2026-08-12 split entry).

## Architecture

```
Frontend (React/Vite/Zustand)  ← HTTP/SSE →  Backend (Rust/Axum)
                                                 │
  Chat UI + TodoPanel          Content-block     Agent Loop (run / run_subagent)
  SubAgentPanel + MemoryPanel  SSE (start/       Tool Registry (49 注册,见 06)
  SettingsView + CharacterConfig delta/stop/     Context Pipeline (14 stages,见 03)
  BootstrapView                tool_result)      ContentBlockStreamer
                                                 Orchestration (session/tools/response)
                                                 SQLite + HNSW + Oxigraph
```

## Crate Structure (15 crates + 2 binaries, layer-grouped 2026-08-13)

```
kernel/   everevo-core         Shared types, traits, errors, config, context pipeline, telemetry
kernel/   everevo-kernel       Microkernel: plugin runtime, protection, bootstrap tools
infra/    everevo-db           SQLite via SQLx, migrations, foreign_keys enabled
infra/    everevo-sandbox      Tiered sandbox (4 permission levels), process isolation
infra/    everevo-vector       ONNX embeddings, HNSW vector store (LanceDB abandoned)
infra/    everevo-net          Unified HTTP egress (proxy-aware)
infra/    everevo-knowledge    Knowledge graph (Oxigraph) + domain document ingestion
infra/    everevo-downloader   HTTP download engine (multi-mirror, resume, concurrent)
infra/    everevo-bootstrap    Runtime provisioning (Python/Node/Git/ONNX/models)
infra/    everevo-mcp          MCP protocol client (stdio + HTTP transports) — moved from kernel 2026-08-13
infra/    everevo-mcp-protocol MCP protocol types (zero-async) — moved from kernel 2026-08-13
app/      everevo-agent        Agent loop, 内置工具集 + MCP adapter, LLM client, memory, stages, skills
app/      everevo-server       Axum HTTP, SSE chat, route modules, orchestration layer
app/      everevo-a2a          A2A protocol gateway (v0.3.0), agent cards, task execution
app/      everevo-workflow     JSON-defined multi-step automation workflows
tools/    everevo-bundler      Standalone asset bundler binary (CLI) — no lib.rs
tools/    everevo-webagent     Standalone MCP search service binary — no lib.rs (moved from app 2026-08-13)
```

> Dependency direction acyclic: `kernel → infra → app → tools` (+ `app→app` peers).

> `everevo-telemetry` was merged into `everevo-core::telemetry`.
> `everevo-kg` and `everevo-domain` were merged into `everevo-knowledge`.

## Agent Loop (3 modes)

```
AgentLoop
├── run()              → mpsc::Receiver<AgentEvent>  (streaming, SSE)
├── run_subagent()     → String                       (sync, sub-agents)
└── run_loop() (internal) → shared by both modes
                             ├── execute_with_hooks() → PreToolUse/PostToolUse
                             ├── catch_unwind()       → panic → AgentEvent::Error
                             ├── autocompact()        → LLM summarization
                             └── trim_context()       → hard trim fallback
```

## SSE Streaming

```
ContentBlockStreamer (orchestration/content_block.rs)
  AgentEvent → Anthropic content-block SSE events
  Used by: main loop + auto-continue loop

Stream helpers (orchestration/stream.rs):
  thinking_start/delta, text_start/delta, tool_start, stop_event, message_start
```

## Context Pipeline (14 stages)

`ContextStage` trait + priority 排序的 stage 队列;核心管线在 `everevo-core/src/context`,阶段实现在 `everevo-agent/src/stages`。完整 stage 清单、优先级与注入内容见 [03-context-pipeline.md](architecture/03-context-pipeline.md)。

## Tool System

```
工具 = 「可实现的 Tool trait + 可组合的注册表 + 全量审计的沙箱壳」;完整工具清单、hooks、注册顺序与四级权限见 [06-tool-system.md](architecture/06-tool-system.md)。

Hook system: ToolHook trait (PreToolUse/PostToolUse)
             AuditHook — default audit trail for all tool calls

MCP tools:    McpTool adapter — MCP ToolDef → everevo Tool trait
              discover_mcp_tools() — one-shot discovery + registration

Sub-agents:   stype_guidance() — type-specific system prompts
              isolation: "worktree" — git worktree isolation
              depth limit: 3 (configurable via subagent_max_depth)

Registration: Single entry point at everevo-server/src/orchestration/tools.rs
              (每阶段 HashMap::insert 覆写:MCP plugin → in-process fallback → stateful)
```

## Error Handling

```
ApiError + ErrorCode    Unified REST error envelope (everevo-core/src/error.rs)
{"error": {"code": "NOT_FOUND", "message": "...", "details": null}}

Panic boundaries        catch_unwind at:
                        - AgentLoop::run() → AgentEvent::Error
                        - Chat handler handler() → SSE error event
                        - Mutex poison: unwrap_or_else(|e| e.into_inner()) throughout
```

## Key Design Decisions

1. Content-block SSE (not raw token streaming) → enables interleaved thinking/tool/text
2. Draft-in-messages pattern → abort preserves partial content
3. ContentBlockStreamer → single SSE conversion point
4. run_subagent() → unified sub-agent execution (workflow, delegate, team)
5. execute_with_hooks() → shared tool execution lifecycle
6. Pluggable ContextStage pipeline → each stage is a trait impl with priority
7. MCP via stdio + HTTP → external tools as first-class citizen
8. SessionCoordinator → centralized per-session channel/data-flow hub
9. HNSW vector store → embedded, no external DB dependency (replaced LanceDB)
10. ApiError + catch_unwind → tool failures never crash main conversation