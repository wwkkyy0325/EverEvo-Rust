# EverEvo Tool Registration(工具注册手册)

How tools are registered, when, how many, how they take effect, and how to
integrate/test a new tool.

## When tools register

- **Per chat request / per session**: `handler.rs` calls
  `orchestration::build_registry` → `assemble()` each request. The registry is
  rebuilt per session (stateful tools hold session_id).
- **On config change**: routing/model changes reload providers, not tools.
- **Plan mode**: the registry is filtered to read-only tools.

## How many

- Tool count = registry entries after `assemble()`. Main loop gets the full set
  (~49 registration calls per the design doc). Sub-agents get strict subsets:
  | Agent | Tools |
  |---|---|
  | Main | everything (bootstrap + MCP + stateful + code + cluster + task + workflow + team + hooks + pipeline + problem_model) |
  | task sub-agent (`base_for_task`) | shell, memory, code_map, list_dir, read_file, code_search |
  | cluster sub-agent (`cluster_base`) | shell, memory, read_file, list_dir, code_search |
  | workflow sub-agent (`base_for_workflow`) | shell, memory, code_map, list_dir, read_file, code_search |
  | team member (`team_base`) | shell, memory |

## How tools register

1. Implement `everevo_core::tool::Tool`:
   `name()`, `description()`, `parameters_schema()` (JSON Schema), `risk_level()`,
   `async execute(params, cancel) -> Result<ToolOutput, EverEvoError>`.
2. Register in `assemble()` (`orchestration/tools.rs`): `registry.register(Arc::new(MyTool { ... }))`.
3. Main-loop-only tools (ask_user, problem_model, pipeline) go in the "Global
   tools" block and are NOT added to `base_for_task`/`cluster_base`/`team_base`.
4. A tool may hold `session_id` + a shared `AppState` map (the `AskUserTool`
   pattern) for per-session state.

## How a tool takes effect

- `ToolRegistry::as_tool_schemas()` serializes each tool's name/description/
  parameters into the LLM tool-call schema block — the model sees it and calls
  by name.
- Execution: the agent loop (`run_subagent` / `run_loop`) calls
  `execute_with_hooks(tool, name, args, cancel, hooks)`, which runs audit +
  reflect hooks, then `tool.execute()`.
- Tool results are merged back as user messages (or paged to disk for large
  outputs).

## Integration / testing(联调)

1. **Compile**: the `Tool` trait impl type-checks.
2. **Unit test `execute()`**: call with valid/invalid params, assert
   `ToolOutput` / helpful `EverEvoError::InvalidInput`.
3. **Registry test**: register + assert `as_tool_schemas()` includes it.
4. **Live loop test**: a real `/api/chat` request; watch `gaia_bench_server.log`
   for `LLM tool call start tool_name=...` + `Tool execution completed`.
5. **Naming**: tool names must be unique across the registry — a duplicate
   silently overwrites in the HashMap.

## Recent additions (2026-08-13)

- `problem_model` — session problem-model store (main-loop only).
- `pipeline` — tool-callable pipeline (main-loop only).
Both follow the AskUserTool registration pattern.
