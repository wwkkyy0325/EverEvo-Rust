# EverEvo-Rust Design Document

## Current State (2026-08)

**14 crates, 493 tests**, 22+ tools running on a content-block SSE streaming architecture. Desktop AI agent with sandboxed tool execution, MCP server + A2A protocol integration, long-term memory (facts + diary + knowledge graph), sub-agent orchestration with git worktree isolation, pluggable context pipeline with LLM autocompact, and embedded ONNX embeddings with HNSW vector store.

## Architecture

```
Frontend (React/Vite/Zustand)  ← HTTP/SSE →  Backend (Rust/Axum)
                                                 │
  Chat UI + TodoPanel          Content-block     Agent Loop (run / run_subagent)
  SubAgentPanel + MemoryPanel  SSE (start/       Tool Registry (22 builtins + MCP)
  SettingsView + CharacterConfig delta/stop/     Context Pipeline (7 stages)
  BootstrapView                tool_result)      ContentBlockStreamer
                                                 Orchestration (session/tools/response)
                                                 SQLite + HNSW + Oxigraph
```

## Crate Structure (14 crates + 2 binaries)

```
everevo-core         Shared types, traits, errors, config, context pipeline, telemetry
everevo-agent        Agent loop, 22 tools + MCP adapter, LLM client, memory, stages, skills
everevo-server       Axum HTTP, SSE chat, 14 route modules, orchestration layer
everevo-db           SQLite via SQLx, migrations, foreign_keys enabled
everevo-sandbox      Tiered sandbox (4 permission levels), process isolation
everevo-vector       ONNX embeddings, HNSW vector store (LanceDB abandoned)
everevo-knowledge    Knowledge graph (Oxigraph) + domain document ingestion
everevo-a2a          A2A protocol gateway (v0.3.0), agent cards, task execution
everevo-bootstrap    Runtime provisioning (Python/Node/Git/ONNX/models)
everevo-downloader   HTTP download engine (multi-mirror, resume, concurrent)
everevo-mcp          MCP protocol client (stdio + HTTP transports)
everevo-workflow     JSON-defined multi-step automation workflows
everevo-bundler      Standalone asset bundler binary (CLI) — no lib.rs
everevo-webagent     Standalone MCP search service binary — no lib.rs
```

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

## Context Pipeline (7 stages)

```
stages/
├── system_prompt.rs     Priority 0 — static instructions + tool descriptions
├── agent_character.rs   Priority 0 — the AGENT's own voice/style
├── persona.rs           Priority 1 — user communication style + thinking paradigm
├── best_practices.rs    Priority 2 — verification, planning, code quality rules
├── skill.rs             Priority 2 — matched skill instructions (selective injection)
├── memory.rs            Priority 3 — RRF-ranked memory facts + context
└── domain_stage.rs      Priority 4 — domain knowledge chunks
```

Plus two built-in stages in `everevo-core`:
- `TaskStateStage` (Priority 0) — LLM-facing state overview
- `SessionMetadataStage` (Priority 0) — runtime environment info

## Tool System

```
Built-in tools (22): shell, download, bootstrap, memory, TodoWrite,
                     EnterPlanMode, ExitPlanMode, Skill, Verify, Task,
                     CancelTask, Workflow (parallel_agents), web_fetch,
                     web_search, compact, team, code_search, code_map,
                     list_dir, read_file, write_file, cluster, workflow_run

Hook system: ToolHook trait (PreToolUse/PostToolUse)
             AuditHook — default audit trail for all tool calls

MCP tools:    McpTool adapter — MCP ToolDef → everevo Tool trait
              discover_mcp_tools() — one-shot discovery + registration

Sub-agents:   stype_guidance() — type-specific system prompts
              isolation: "worktree" — git worktree isolation
              depth limit: 3 (configurable via subagent_max_depth)

Registration: Single entry point at everevo-server/src/orchestration/tools.rs
              (8 phases: base → MCP → file-ops → sub-agent → team → workflow)
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
