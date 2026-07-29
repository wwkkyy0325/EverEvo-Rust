# EverEvo-Rust Design Document

## Current State (2026-07)

**12 crates, 101 tests**, 11+ tools running on a content-block SSE streaming architecture. Desktop AI agent with sandboxed tool execution, MCP server integration, long-term memory, sub-agent orchestration with worktree isolation, and pluggable context pipeline with LLM autocompact.

## Architecture

```
Frontend (React/Vite/Zustand)  ← HTTP/SSE →  Backend (Rust/Axum)
                                                 │
  Chat UI + TodoPanel          Content-block     Agent Loop (run / run_subagent)
  SubAgentPanel + MemoryPanel  SSE (start/       Tool Registry (11 builtins + MCP)
  AuditPanel + ErrorBoundary   delta/stop/       Context Pipeline (5 stages)
                               tool_result)      ContentBlockStreamer
                                                 Orchestration (session/tools/response)
                                                 SQLite + LanceDB + Oxigraph
```

## Crate Structure (12 crates)

```
everevo-core         Shared types, traits, errors, config, context pipeline, ToolHook
everevo-agent        Agent loop, 11 tools + MCP adapter, LLM client, memory, stages
everevo-server       Axum HTTP, SSE chat, 9 route modules, orchestration layer
everevo-db           SQLite via SQLx, migrations, foreign_keys enabled
everevo-sandbox      Tiered sandbox (4 permission levels), process isolation
everevo-vector       ONNX embeddings, LanceDB vector store
everevo-bootstrap    Runtime provisioning (Python/Node/Git/ONNX/models)
everevo-downloader   HTTP download engine (multi-mirror, resume, concurrent)
everevo-telemetry    Agent turn metrics + retrieval observability
everevo-mcp          MCP protocol client (stdio), tools + resources + prompts
everevo-kg           MERGED → everevo-agent::knowledge::graph
everevo-domain       MERGED → everevo-agent::knowledge::domain
```

## Agent Loop (3 modes)

```
AgentLoop
├── run()              → mpsc::Receiver<AgentEvent>  (streaming, SSE)
├── run_subagent()     → String                       (sync, sub-agents)
└── run_loop() (内部)   → shared by both modes
                         ├── execute_with_hooks() → PreToolUse/PostToolUse
                         ├── autocompact()        → LLM summarization
                         └── trim_context()       → hard trim fallback
```

## SSE Streaming

```
ContentBlockStreamer (182 lines, server crate)
  AgentEvent → Anthropic content-block SSE events
  Used by: main loop + auto-continue loop (2× deduplicated)

Stream helpers (orchestration/stream.rs):
  thinking_start/delta, text_start/delta, tool_start, stop_event, message_start
```

## Context Pipeline (5 stages)

```
stages/
├── persona.rs          Priority 1 — user communication style
├── best_practices.rs   Priority 2 — verification, planning rules
├── skill.rs            Priority 2 — available skill names
├── memory.rs           Priority 3 — RRF-ranked memory facts
└── domain_stage.rs     Priority 4 — domain knowledge chunks
```

## Tool System

```
Built-in tools (11): shell, download, bootstrap, memory, TodoWrite,
                     EnterPlanMode, ExitPlanMode, Skill, Verify, Task, Workflow

Hook system: ToolHook trait (PreToolUse/PostToolUse)
             AuditHook — default audit trail for all tool calls

MCP tools:    McpTool adapter — MCP ToolDef → everevo Tool trait
              discover_mcp_tools() — one-shot discovery + registration

Sub-agents:   stype_guidance() — type-specific system prompts
              isolation: "worktree" — git worktree isolation
```

## Key Design Decisions

1. Content-block SSE (not raw token streaming) → enables interleaved thinking/tool/text
2. Draft-in-messages pattern → abort preserves partial content
3. ContentBlockStreamer → single SSE conversion point, 2× deduplicated
4. run_subagent() → unified sub-agent execution (workflow, delegate, future)
5. execute_with_hooks() → shared tool execution lifecycle
6. Pluggable ContextStage pipeline → each stage is a trait impl with priority
7. MCP via stdio → external tools as first-class citizen
